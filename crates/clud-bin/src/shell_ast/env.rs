//! Variables and working directory while walking a script.

use std::collections::{HashMap, HashSet};

use super::{paths, Dialect, UnknownReason, Value};

/// Inherited environment variables and working directory: what every Bash
/// tool call starts from. In production this is the hook's own environment
/// and the payload's `cwd`; tests pass a fixed one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BaseEnv {
    pub(super) vars: HashMap<String, String>,
    pub(super) cwd: String,
}

impl BaseEnv {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            vars: HashMap::new(),
            cwd: cwd.into(),
        }
    }

    #[must_use]
    pub fn with_var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    /// The current process environment, starting in `cwd`.
    pub fn from_process(cwd: &std::path::Path) -> Self {
        Self {
            vars: std::env::vars().collect(),
            cwd: cwd.to_string_lossy().into_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Var {
    value: Value,
    exported: bool,
}

/// The evaluator's view of one shell: variables, `cd` state and the
/// `pushd` stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Env {
    vars: HashMap<String, Var>,
    pub(super) cwd: Value,
    dir_stack: Vec<Value>,
    case_insensitive: bool,
}

impl Env {
    pub(super) fn from_base(base: &BaseEnv, dialect: Dialect) -> Self {
        let case_insensitive = dialect != Dialect::Posix;
        let mut env = Self {
            vars: HashMap::new(),
            cwd: if base.cwd.is_empty() {
                Value::unknown("$PWD", UnknownReason::RuntimeValue)
            } else {
                Value::literal(base.cwd.clone())
            },
            dir_stack: Vec::new(),
            case_insensitive,
        };
        for (name, value) in &base.vars {
            env.set(name, Value::literal(value.clone()), Some(true));
        }
        env
    }

    fn key(&self, name: &str) -> String {
        if self.case_insensitive {
            name.to_ascii_lowercase()
        } else {
            name.to_string()
        }
    }

    pub(super) fn get(&self, name: &str) -> Option<&Value> {
        if name == "PWD" || (self.case_insensitive && name.eq_ignore_ascii_case("pwd")) {
            return Some(&self.cwd);
        }
        self.vars.get(&self.key(name)).map(|var| &var.value)
    }

    /// Assign `name`. `exported: None` keeps the variable's current flag.
    pub(super) fn set(&mut self, name: &str, value: Value, exported: Option<bool>) {
        let key = self.key(name);
        let exported = exported
            .or_else(|| self.vars.get(&key).map(|var| var.exported))
            .unwrap_or(false);
        self.vars.insert(key, Var { value, exported });
    }

    pub(super) fn export(&mut self, name: &str) {
        let key = self.key(name);
        if let Some(var) = self.vars.get_mut(&key) {
            var.exported = true;
        }
    }

    pub(super) fn unset(&mut self, name: &str) {
        let key = self.key(name);
        self.vars.remove(&key);
    }

    /// A variable whose value now depends on run-time input (`read`).
    pub(super) fn set_runtime(&mut self, name: &str) {
        let raw = format!("${name}");
        self.set(name, Value::unknown(raw, UnknownReason::RuntimeValue), None);
    }

    /// The environment a child shell (`bash -c`, a heredoc fed to `sh`)
    /// starts with: exported variables only, same directory.
    pub(super) fn child_shell(&self, dialect: Dialect) -> Self {
        Self {
            vars: self
                .vars
                .iter()
                .filter(|(_, var)| var.exported)
                .map(|(key, var)| (key.clone(), var.clone()))
                .collect(),
            cwd: self.cwd.clone(),
            dir_stack: Vec::new(),
            case_insensitive: dialect != Dialect::Posix,
        }
    }

    pub(super) fn home(&self) -> Option<String> {
        ["HOME", "USERPROFILE"]
            .iter()
            .find_map(|name| self.get(name).and_then(Value::as_literal))
            .map(str::to_string)
    }

    /// `cd target`: join against the current directory.
    pub(super) fn change_dir(&mut self, target: &Value) {
        self.cwd = match (&self.cwd, target) {
            (_, Value::Unknown(unknown)) => Value::Unknown(unknown.clone()),
            (Value::Unknown(_), Value::Literal(path)) if !paths::is_absolute(path) => {
                Value::unknown(path.clone(), UnknownReason::RuntimeValue)
            }
            (Value::Unknown(_), Value::Literal(path)) => Value::literal(paths::normalize(path)),
            (Value::Literal(base), Value::Literal(path)) => Value::literal(paths::join(base, path)),
        };
    }

    pub(super) fn push_dir(&mut self, target: &Value) {
        self.dir_stack.push(self.cwd.clone());
        self.change_dir(target);
    }

    pub(super) fn pop_dir(&mut self) {
        self.cwd = self
            .dir_stack
            .pop()
            .unwrap_or_else(|| Value::unknown("$OLDPWD", UnknownReason::RuntimeValue));
    }

    /// Merge the states a construct can end in. A variable (or the cwd)
    /// that differs between the paths becomes a run-time unknown.
    pub(super) fn join(&mut self, outcomes: &[Env]) {
        let Some((first, rest)) = outcomes.split_first() else {
            return;
        };
        let mut names: HashSet<&String> = first.vars.keys().collect();
        for outcome in rest {
            names.extend(outcome.vars.keys());
        }
        let mut merged = HashMap::new();
        for name in names {
            let values: Vec<Option<&Var>> = outcomes.iter().map(|o| o.vars.get(name)).collect();
            if let Some(var) = agreed(&values) {
                merged.insert(name.clone(), var.clone());
            } else {
                let exported = values.iter().flatten().any(|var| var.exported);
                let value = Value::unknown(format!("${name}"), UnknownReason::RuntimeValue);
                merged.insert(name.clone(), Var { value, exported });
            }
        }
        self.vars = merged;
        self.cwd = if outcomes.iter().all(|o| o.cwd == first.cwd) {
            first.cwd.clone()
        } else {
            Value::unknown("$PWD", UnknownReason::RuntimeValue)
        };
        self.dir_stack = first.dir_stack.clone();
    }
}

/// The variable every path agrees on; `None` when they differ or some path
/// leaves it unset.
fn agreed<'a>(values: &[Option<&'a Var>]) -> Option<&'a Var> {
    let first = values.first().copied().flatten()?;
    values
        .iter()
        .all(|value| value.is_some_and(|var| var.value == first.value))
        .then_some(first)
}
