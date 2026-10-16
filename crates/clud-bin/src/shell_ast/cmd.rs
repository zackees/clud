//! cmd.exe front end.
//!
//! cmd has no grammar a parser library can offer: `%VAR%` is expanded when
//! a line is read, `^` escapes the next character outside quotes, and
//! everything else is word splitting around `&`, `&&`, `||` and `|`. This
//! splitter models exactly that and produces the same [`Invocation`]s as the
//! tree-sitter dialects.
//!
//! [`Invocation`]: super::Invocation

use super::env::Env;
use super::words::{self, Collector, Ctx, Found};
use super::{
    paths, Construct, Dialect, LoopKind, PipelineSlot, Redirect, SyntaxError, UnknownReason, Value,
};

pub(super) fn walk_text(text: &str, env: &mut Env, ctx: Ctx, out: &mut Collector) {
    for line in text.split(['\n', '\r']) {
        if line.trim().is_empty() {
            continue;
        }
        let expanded = expand_percent(line, env);
        walk_line(&expanded, env, ctx, out);
    }
}

/// One `%VAR%` expansion pass over a whole line, before anything on the
/// line runs. Unknown references keep their text and taint their words.
#[derive(Debug, Clone)]
struct Piece {
    text: String,
    unknown: Option<UnknownReason>,
}

fn expand_percent(line: &str, env: &Env) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        let Some(len) = after.find('%') else { break };
        let name = &after[..len];
        let valid = !name.is_empty() && !name.contains([' ', '"']);
        if !valid {
            pieces.push(literal_piece(&rest[..start + 1]));
            rest = after;
            continue;
        }
        pieces.push(literal_piece(&rest[..start]));
        let raw = format!("%{name}%");
        pieces.push(match env.get(name) {
            Some(Value::Literal(value)) => literal_piece(value),
            Some(Value::Unknown(unknown)) => Piece {
                text: raw,
                unknown: Some(unknown.reason),
            },
            None => Piece {
                text: raw,
                unknown: Some(UnknownReason::UnsetVariable),
            },
        });
        rest = &after[len + 1..];
    }
    pieces.push(literal_piece(rest));
    pieces
}

fn literal_piece(text: &str) -> Piece {
    Piece {
        text: text.to_string(),
        unknown: None,
    }
}

/// A word being built: its text and the first unknown that fed it.
#[derive(Debug, Default)]
struct WordBuf {
    text: String,
    unknown: Option<UnknownReason>,
    started: bool,
}

impl WordBuf {
    fn take(&mut self) -> Option<Value> {
        if !self.started {
            return None;
        }
        let text = std::mem::take(&mut self.text);
        let value = match self.unknown.take() {
            None => Value::Literal(text),
            Some(reason) => Value::unknown(text, reason),
        };
        self.started = false;
        Some(value)
    }

    fn push(&mut self, ch: char, unknown: Option<UnknownReason>) {
        self.text.push(ch);
        self.started = true;
        if let Some(reason) = unknown {
            self.unknown.get_or_insert(reason);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Seq,
    Pipe,
}

/// One command on a line and the operator that follows it.
#[derive(Debug, Default)]
struct Part {
    words: Vec<Value>,
    redirects: Vec<Redirect>,
}

/// Splits one expanded line into commands.
#[derive(Default)]
struct Splitter {
    parts: Vec<(Part, Op)>,
    current: Part,
    word: WordBuf,
    /// A redirect operator waiting for its target word.
    pending: Option<String>,
    subshells: usize,
}

impl Splitter {
    fn end_word(&mut self) {
        let Some(value) = self.word.take() else {
            return;
        };
        match self.pending.take() {
            Some(operator) => self.current.redirects.push(Redirect {
                operator,
                target: value,
            }),
            None => self.current.words.push(value),
        }
    }

    fn end_command(&mut self, op: Op) {
        self.end_word();
        let part = std::mem::take(&mut self.current);
        self.parts.push((part, op));
    }

    /// `>`, `>>`, `2>`, `2>&1`, `<`: a digits-only word just before is the
    /// descriptor.
    fn redirect(
        &mut self,
        chars: &[(char, Option<UnknownReason>)],
        index: &mut usize,
        first: char,
    ) {
        let descriptor = self.word.started
            && !self.word.text.is_empty()
            && self.word.text.chars().all(|ch| ch.is_ascii_digit());
        let mut operator = if descriptor {
            let fd = std::mem::take(&mut self.word.text);
            self.word.started = false;
            fd
        } else {
            self.end_word();
            String::new()
        };
        operator.push(first);
        if chars.get(*index).is_some_and(|(next, _)| *next == first) {
            operator.push(first);
            *index += 1;
        }
        if chars.get(*index).is_some_and(|(next, _)| *next == '&') {
            operator.push('&');
            *index += 1;
            while let Some((digit, _)) = chars.get(*index).filter(|(ch, _)| ch.is_ascii_digit()) {
                operator.push(*digit);
                *index += 1;
            }
            self.current.redirects.push(Redirect {
                target: Value::literal(operator.rsplit('&').next().unwrap_or_default()),
                operator,
            });
            return;
        }
        self.pending = Some(operator);
    }
}

/// Split one expanded line into commands, each with the operator that
/// follows it, plus whether a quote was left open.
fn split_commands(pieces: &[Piece], subshells: &mut usize) -> (Vec<(Part, Op)>, bool) {
    let chars: Vec<(char, Option<UnknownReason>)> = pieces
        .iter()
        .flat_map(|piece| piece.text.chars().map(move |ch| (ch, piece.unknown)))
        .collect();
    let mut split = Splitter::default();
    let mut quoted = false;
    let mut index = 0;
    while index < chars.len() {
        let (ch, unknown) = chars[index];
        index += 1;
        if quoted {
            if ch == '"' {
                quoted = false;
            } else {
                split.word.push(ch, unknown);
            }
            split.word.started = true;
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                split.word.started = true;
            }
            '^' => {
                if let Some((next, next_unknown)) = chars.get(index).copied() {
                    split.word.push(next, next_unknown.or(unknown));
                    index += 1;
                }
            }
            '>' | '<' => split.redirect(&chars, &mut index, ch),
            '&' | '|' => {
                let doubled = chars.get(index).is_some_and(|(next, _)| *next == ch);
                if doubled {
                    index += 1;
                }
                let op = if ch == '|' && !doubled {
                    Op::Pipe
                } else {
                    Op::Seq
                };
                split.end_command(op);
            }
            '(' | ')' => {
                if ch == '(' {
                    split.subshells += 1;
                }
                split.end_command(Op::Seq);
            }
            ch if ch.is_whitespace() => split.end_word(),
            _ => split.word.push(ch, unknown),
        }
    }
    split.end_command(Op::Seq);
    *subshells += split.subshells;
    (split.parts, quoted)
}

fn walk_line(pieces: &[Piece], env: &mut Env, ctx: Ctx, out: &mut Collector) {
    let mut subshells = 0;
    let (commands, unbalanced) = split_commands(pieces, &mut subshells);
    for _ in 0..subshells {
        out.construct(ctx, Construct::Subshell);
    }
    if unbalanced {
        out.syntax_error(SyntaxError::UnterminatedDoubleQuote);
    }
    let ctx = if unbalanced { ctx.errored() } else { ctx };
    let mut stage_group: Vec<Part> = Vec::new();
    for (mut part, op) in commands {
        part.words = strip_prefixes(part.words);
        if part.words.is_empty() {
            continue;
        }
        stage_group.push(part);
        if op == Op::Seq {
            emit_group(std::mem::take(&mut stage_group), env, ctx, out);
        }
    }
    emit_group(stage_group, env, ctx, out);
}

/// `@` (echo off) and `call` change nothing about what runs.
fn strip_prefixes(mut words: Vec<Value>) -> Vec<Value> {
    if let Some(Value::Literal(first)) = words.first_mut() {
        if let Some(rest) = first.strip_prefix('@') {
            *first = rest.to_string();
            if first.is_empty() {
                words.remove(0);
            }
        }
    }
    while words
        .first()
        .and_then(Value::as_literal)
        .is_some_and(|word| word.eq_ignore_ascii_case("call"))
    {
        words.remove(0);
    }
    words
}

fn emit_group(group: Vec<Part>, env: &mut Env, ctx: Ctx, out: &mut Collector) {
    if group.is_empty() {
        return;
    }
    if group.len() == 1 {
        let part = group.into_iter().next().unwrap_or_default();
        run(part, env, ctx, out);
        return;
    }
    let id = out.pipeline_id();
    let count = group.len();
    for (stage, part) in group.into_iter().enumerate() {
        let slot = PipelineSlot {
            id,
            stage,
            stages: count,
        };
        let mut stage_env = env.clone();
        run(part, &mut stage_env, ctx.in_pipeline(slot), out);
    }
}

fn run(part: Part, env: &mut Env, ctx: Ctx, out: &mut Collector) {
    let Part { words, redirects } = part;
    let program = words
        .first()
        .and_then(Value::as_literal)
        .map(paths::program_name);
    if program.as_deref() == Some("for") {
        let body_at = words
            .iter()
            .position(|w| w.as_literal().is_some_and(|t| t.eq_ignore_ascii_case("do")));
        if let Some(body_at) = body_at {
            let body = Part {
                words: words[body_at + 1..].to_vec(),
                redirects,
            };
            out.construct(ctx, Construct::ControlFlow("for"));
            out.loop_seen(LoopKind::ForEach);
            run(body, env, ctx.in_loop(LoopKind::ForEach), out);
            return;
        }
    }
    let source = words.iter().map(Value::text).collect::<Vec<_>>().join(" ");
    let found = Found {
        words: words.clone(),
        raw: words.iter().map(|word| word.text().to_string()).collect(),
        assignments: Vec::new(),
        redirects,
        source: &source,
    };
    if !out.emit(found, env, ctx) {
        return;
    }
    let Some(program) = program else { return };
    let args: Vec<Value> = words[1..]
        .iter()
        .filter(|word| {
            !word
                .as_literal()
                .is_some_and(|t| t.eq_ignore_ascii_case("/d"))
        })
        .cloned()
        .collect();
    match program.as_str() {
        "set" => {
            if let Some(name) = set_builtin(&args, env) {
                out.assigned(&name);
            }
        }
        "cd" | "chdir" if args.is_empty() => {}
        _ => {
            if !words::apply_directory_builtin(Dialect::Cmd, &program, &args, env) {
                words::walk_nested(&words, env, ctx, out);
            }
        }
    }
}

/// `set NAME=value` assigns; `set /p` and `set /a` depend on input.
/// Returns the assigned name.
fn set_builtin(args: &[Value], env: &mut Env) -> Option<String> {
    let first = args.first()?;
    let flag = first.as_literal().map(str::to_ascii_lowercase);
    let runtime = matches!(flag.as_deref(), Some("/p" | "/a"));
    let assignment = if runtime { args.get(1) } else { Some(first) };
    let assignment = assignment?;
    let joined = args[usize::from(runtime)..]
        .iter()
        .map(Value::text)
        .collect::<Vec<_>>()
        .join(" ");
    let (name, value) = joined.split_once('=')?;
    if runtime || assignment.is_unknown() {
        env.set_runtime(name);
    } else {
        env.set(name, Value::literal(value), Some(true));
    }
    Some(name.to_string())
}
