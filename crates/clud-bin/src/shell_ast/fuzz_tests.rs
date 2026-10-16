//! Seeded fuzz and property tests for the shell AST (#1962).
//!
//! No fuzzing crate: a fixed xorshift generator keeps every run
//! reproducible in CI, and a failure prints the exact input.

use super::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

/// Fragments chosen to hit every grammar construct and its error paths:
/// quotes, escapes, operators, expansions, heredocs, PowerShell and cmd
/// syntax, unbalanced brackets and non-ASCII text.
const FRAGMENTS: &[&str] = &[
    " ",
    "  ",
    "\t",
    "\n",
    ";",
    "&",
    "&&",
    "|",
    "||",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    "'",
    "\"",
    "`",
    "\\",
    "$",
    "$(",
    "${",
    "}",
    "$((",
    "))",
    "<",
    ">",
    ">>",
    "2>&1",
    "<<EOF\n",
    "\nEOF\n",
    "<<-'X'\n",
    "#",
    "=",
    "*",
    "?",
    "~",
    "!",
    "%",
    "^",
    "@(",
    "$env:",
    "-C",
    "--",
    "cd",
    "git",
    "rm",
    "-rf",
    "/",
    "..",
    "for",
    "in",
    "do",
    "done",
    "while",
    "until",
    "if",
    "then",
    "fi",
    "case",
    "esac",
    ";;",
    "function",
    "export",
    "eval",
    "bash -c",
    "pwsh -Command",
    "cmd /c",
    "x",
    "a b",
    "é",
    "日本",
    "\u{0}",
    "\r\n",
    "$'\\t'",
    "${X:-d}",
    "$1",
    "&{",
    "Set-Location",
];

fn random_text(rng: &mut Rng) -> String {
    let len = 1 + rng.below(24);
    (0..len).map(|_| rng.pick(FRAGMENTS)).collect()
}

#[test]
fn fuzz_never_panics_and_keeps_invariants() {
    let base = BaseEnv::new("/work").with_var("HOME", "/home/u");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..3000 {
        let text = random_text(&mut rng);
        for dialect in [Dialect::Posix, Dialect::PowerShell, Dialect::Cmd] {
            let script = std::panic::catch_unwind(|| parse(&text, dialect, &base))
                .unwrap_or_else(|_| panic!("parse panicked on {dialect:?} {text:?}"));
            for invocation in &script.invocations {
                assert!(
                    !invocation.words.is_empty(),
                    "empty invocation for {dialect:?} {text:?}"
                );
                if let Some(slot) = invocation.pipeline {
                    assert!(slot.stage < slot.stages && slot.stages >= 2, "{text:?}");
                }
            }
        }
    }
}

const PROGRAMS: &[&str] = &["git", "rm", "gh", "cargo", "ls", "python"];
const ARGS: &[&str] = &[
    "status",
    "-C",
    "/repo",
    "worktree",
    "add",
    "../wt",
    "-b",
    "feat/x",
    "-rf",
    "/tmp/x",
    "--json",
    "a.b",
    "origin/main",
    "x.txt",
];

/// A random simple command, as plain words with no shell metacharacters.
fn random_command(rng: &mut Rng) -> Vec<String> {
    let mut words = vec![rng.pick(PROGRAMS).to_string()];
    for _ in 0..rng.below(6) {
        words.push(rng.pick(ARGS).to_string());
    }
    words
}

fn effective_texts(text: &str, dialect: Dialect) -> Vec<Vec<String>> {
    let base = BaseEnv::new("/work").with_var("HOME", "/home/u");
    parse(text, dialect, &base)
        .invocations
        .iter()
        .map(|inv| {
            inv.effective_argv()
                .iter()
                .map(|value| value.text().to_string())
                .collect()
        })
        .collect()
}

/// Rewrites that must never change what runs.
fn posix_equivalents(words: &[String], rng: &mut Rng) -> Vec<String> {
    let plain = words.join(" ");
    let spaced = words.join("   \t ");
    let single: Vec<String> = words.iter().map(|w| format!("'{w}'")).collect();
    let double: Vec<String> = words.iter().map(|w| format!("\"{w}\"")).collect();
    let mixed: Vec<String> = words
        .iter()
        .map(|w| match rng.below(3) {
            0 => w.clone(),
            1 => format!("'{w}'"),
            _ => format!("\"{w}\""),
        })
        .collect();
    vec![
        format!("  {spaced}  "),
        single.join(" "),
        double.join(" "),
        mixed.join(" "),
        format!("( {plain} )"),
        format!("{{ {plain}; }}"),
        format!("env {plain}"),
        format!("command {plain}"),
        format!("exec {plain}"),
        format!("V={}; $V {}", words[0], words[1..].join(" ")),
        format!("{plain} # trailing comment"),
        format!("{plain} 2>/dev/null"),
    ]
}

#[test]
fn equivalent_spellings_produce_the_same_command() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..400 {
        let words = random_command(&mut rng);
        let expected = vec![words.clone()];
        for variant in posix_equivalents(&words, &mut rng) {
            assert_eq!(
                effective_texts(&variant, Dialect::Posix),
                expected,
                "`{variant}` should mean `{}`",
                words.join(" ")
            );
        }
    }
}

#[test]
fn powershell_equivalent_spellings_produce_the_same_command() {
    let mut rng = Rng(0x0DDB_1A5E_5BAD_5EED);
    for _ in 0..200 {
        let words = random_command(&mut rng);
        let expected = vec![words.clone()];
        let plain = words.join(" ");
        let quoted: Vec<String> = words.iter().map(|w| format!("'{w}'")).collect();
        for variant in [
            format!("  {}  ", words.join("    ")),
            format!("& {}", quoted.join(" ")),
            format!("$p = '{}'; & $p {}", words[0], words[1..].join(" ")),
            format!("{plain} # trailing comment"),
        ] {
            assert_eq!(
                effective_texts(&variant, Dialect::PowerShell),
                expected,
                "`{variant}`"
            );
        }
    }
}

#[test]
fn reordering_independent_flags_keeps_the_same_word_multiset() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    for _ in 0..200 {
        let mut words = random_command(&mut rng);
        words.push("--alpha".to_string());
        words.push("--beta".to_string());
        let forward = effective_texts(&words.join(" "), Dialect::Posix);
        let len = words.len();
        words.swap(len - 1, len - 2);
        let mut backward = effective_texts(&words.join(" "), Dialect::Posix);
        let mut forward_sorted = forward.clone();
        forward_sorted[0].sort();
        backward[0].sort();
        assert_eq!(forward_sorted, backward);
        assert_eq!(forward[0][0], words[0]);
    }
}
