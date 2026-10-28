//! Conservative static recognition of simple Python source-file writes.
//!
//! This is an advisory ledger, not a Python interpreter. Unknown syntax,
//! dynamic paths, control flow and non-project output remain unclassified.

use std::collections::BTreeMap;
use std::path::Path;

use crate::shell_ast::{self, BaseEnv, Dialect, Value};

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Name(String),
    Literal(String),
    BytesLiteral(String),
    Opaque,
    Punct(char),
    End,
    Semi,
}

/// Never execute the script. Restrict recognition to a literal `-c` argument
/// or a heredoc fed directly to a Python executable.
pub(super) fn writes_source(command: &str, cwd: &Path) -> bool {
    if command.len() > 32_768
        || !command.contains("python")
        || !(command.contains("open") || command.contains("write_"))
        || command.contains("&&")
        || command.contains("||")
    {
        return false;
    }
    let Ok(cwd) = cwd.canonicalize() else {
        return false;
    };
    let Some(root) = super::super::block_bad_cmd_cd::nearest_repo_root(&cwd)
        .and_then(|root| root.canonicalize().ok())
    else {
        return false;
    };
    let script = shell_ast::parse(
        command,
        Dialect::Posix,
        &BaseEnv::new(cwd.to_string_lossy()),
    );
    if script.has_error
        || script.depth_capped
        || !script.constructs.is_empty()
        || !script.loops.is_empty()
    {
        return false;
    }
    if script.invocations.iter().any(|inv| {
        inv.background
            || inv.pipeline.is_some()
            || inv.depth != 0
            || inv
                .effective_program()
                .is_some_and(|p| matches!(p.as_str(), "exit" | "return"))
    }) {
        return false;
    }
    script
        .invocations
        .iter()
        .enumerate()
        .any(|(position, invocation)| {
            // Pre-tool observation cannot know whether earlier shell commands
            // fail or whether errexit is set. Recognize the first command only.
            if position != 0 {
                return false;
            }
            let Some(program) = invocation.effective_program() else {
                return false;
            };
            if !is_python(&program) || invocation.in_error {
                return false;
            }
            let Some(cwd) = invocation
                .cwd
                .as_literal()
                .and_then(|path| Path::new(path).canonicalize().ok())
            else {
                return false;
            };
            if !cwd.starts_with(&root) {
                return false;
            }
            let argv = invocation.effective_argv();
            let source = match argv.get(1).and_then(Value::as_literal) {
                Some("-c") if argv.len() == 3 && invocation.redirects.is_empty() => {
                    argv.get(2).and_then(Value::as_literal)
                }
                None | Some("-") if argv.len() <= 2 && quoted_heredoc(invocation, command) => {
                    invocation.heredoc.as_deref()
                }
                _ => None,
            };
            source.is_some_and(|source| simple_python_source_write(source, &cwd, &root))
        })
}

fn quoted_heredoc(invocation: &shell_ast::Invocation, command: &str) -> bool {
    let [redirect] = invocation.redirects.as_slice() else {
        return false;
    };
    if redirect.operator != "<<" {
        return false;
    }
    let Some(target) = redirect.target.as_literal() else {
        return false;
    };
    let quoted = target.len() >= 2
        && ((target.starts_with('\'') && target.ends_with('\''))
            || (target.starts_with('"') && target.ends_with('"')));
    if !quoted {
        return false;
    }
    let Some(header) = command.lines().next() else {
        return false;
    };
    let marker = format!("<<{target}");
    let Some((prefix, suffix)) = header.split_once(&marker) else {
        return false;
    };
    suffix.trim().is_empty() && prefix.chars().last().is_some_and(char::is_whitespace)
}

fn is_python(program: &str) -> bool {
    program == "python"
        || program == "python3" // python-name-lint: allow -- classify external command spelling
        || program
            .strip_prefix("python3.") // python-name-lint: allow -- classify external command spelling
            .is_some_and(|minor| !minor.is_empty() && minor.bytes().all(|b| b.is_ascii_digit()))
}

#[derive(Clone, Copy)]
enum Binding<'a> {
    Literal(&'a str),
    Text,
}

fn simple_python_source_write(source: &str, cwd: &Path, root: &Path) -> bool {
    if source.len() > 16_384 || source.contains('\0') {
        return false;
    }
    if source.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.is_empty() && !trimmed.starts_with('#') && trimmed.len() != line.len()
    }) {
        return false;
    }
    let Some(tokens) = tokenize(source) else {
        return false;
    };
    let mut has_statement = false;
    for token in &tokens {
        match token {
            Token::Semi if !has_statement => return false,
            Token::Semi | Token::End => has_statement = false,
            _ => has_statement = true,
        }
    }
    let mut values = BTreeMap::<&str, Binding<'_>>::new();
    let mut pathlib_imported = false;
    let mut found = false;
    for statement in tokens
        .split(|token| matches!(token, Token::End | Token::Semi))
        .filter(|part| !part.is_empty())
    {
        match statement {
            [Token::Name(a), Token::Name(b), Token::Name(c), Token::Name(d)]
                if a == "from" && b == "pathlib" && c == "import" && d == "Path" =>
            {
                pathlib_imported = true;
            }
            [Token::Name(name), Token::Punct('='), Token::Literal(value)]
                if valid_variable(name) =>
            {
                values.insert(name, Binding::Literal(value));
            }
            [Token::Name(name), Token::Punct('='), Token::Name(open), Token::Punct('('), path, Token::Punct(')'), Token::Punct('.'), Token::Name(read), Token::Punct('('), Token::Punct(')')]
                if valid_variable(name) && open == "open" && read == "read" =>
            {
                let Some(path) = literal_path(path, &values) else {
                    return false;
                };
                if !is_source_path(path, cwd, root) || !cwd.join(path).is_file() {
                    return false;
                }
                values.insert(name, Binding::Text);
            }
            [Token::Name(name), Token::Punct('='), Token::Name(input), Token::Punct('.'), Token::Name(method), Token::Punct('('), Token::Literal(_), Token::Punct(','), Token::Literal(_), Token::Punct(')')]
                if valid_variable(name)
                    && method == "replace"
                    && values.contains_key(input.as_str()) =>
            {
                values.insert(name, Binding::Text);
            }
            [Token::Name(name), Token::Punct('('), path, Token::Punct(','), Token::Literal(mode), Token::Punct(')'), tail @ ..]
                if name == "open" =>
            {
                if !write_tail(tail, &values) {
                    return false;
                }
                let path = literal_path(path, &values);
                if writes_mode(mode) && path.is_some_and(|path| is_source_path(path, cwd, root)) {
                    found = true;
                }
            }
            [Token::Name(name), Token::Punct('('), Token::Literal(path), Token::Punct(')'), Token::Punct('.'), Token::Name(method), Token::Punct('('), argument @ ..]
                if name == "Path"
                    && pathlib_imported
                    && matches!(method.as_str(), "write_text" | "write_bytes") =>
            {
                if !pathlib_argument(method, argument, &values) {
                    return false;
                }
                if is_source_path(path, cwd, root) {
                    found = true;
                }
            }
            _ => return false,
        }
    }
    found
}

fn literal_path<'a>(token: &'a Token, values: &BTreeMap<&str, Binding<'a>>) -> Option<&'a str> {
    match token {
        Token::Literal(path) => Some(path),
        Token::Name(name) => match values.get(name.as_str()) {
            Some(Binding::Literal(path)) => Some(path),
            _ => None,
        },
        _ => None,
    }
}

fn valid_variable(name: &str) -> bool {
    !matches!(
        name,
        "open"
            | "Path"
            | "write_text"
            | "write_bytes"
            | "False"
            | "True"
            | "None"
            | "and"
            | "as"
            | "assert"
            | "async"
            | "await"
            | "break"
            | "class"
            | "continue"
            | "def"
            | "del"
            | "elif"
            | "else"
            | "except"
            | "finally"
            | "for"
            | "from"
            | "global"
            | "if"
            | "import"
            | "in"
            | "is"
            | "lambda"
            | "nonlocal"
            | "not"
            | "or"
            | "pass"
            | "raise"
            | "return"
            | "try"
            | "while"
            | "with"
            | "yield"
            | "__debug__"
    )
}

fn write_tail(tokens: &[Token], values: &BTreeMap<&str, Binding<'_>>) -> bool {
    if tokens.is_empty() {
        return true;
    }
    match tokens {
        [Token::Punct('.'), Token::Name(method), Token::Punct('('), argument @ ..]
            if method == "write" =>
        {
            call_argument(argument, values)
        }
        _ => false,
    }
}

fn call_argument(tokens: &[Token], values: &BTreeMap<&str, Binding<'_>>) -> bool {
    match tokens {
        [Token::Literal(_), Token::Punct(')')] => true,
        [Token::BytesLiteral(_), Token::Punct(')')] => true,
        [Token::Name(name), Token::Punct(')')] => values.contains_key(name.as_str()),
        _ => false,
    }
}

fn pathlib_argument(method: &str, tokens: &[Token], values: &BTreeMap<&str, Binding<'_>>) -> bool {
    match (method, tokens) {
        ("write_text", [Token::Literal(_), Token::Punct(')')]) => true,
        ("write_text", [Token::Name(name), Token::Punct(')')]) => {
            values.contains_key(name.as_str())
        }
        ("write_bytes", [Token::BytesLiteral(_), Token::Punct(')')]) => true,
        _ => false,
    }
}

fn writes_mode(mode: &str) -> bool {
    matches!(
        mode,
        "w" | "wb"
            | "w+"
            | "wb+"
            | "w+b"
            | "a"
            | "ab"
            | "a+"
            | "ab+"
            | "a+b"
            | "x"
            | "xb"
            | "x+"
            | "xb+"
            | "x+b"
    )
}

fn is_source_path(value: &str, cwd: &Path, root: &Path) -> bool {
    if value.is_empty() || value.starts_with('/') || value.contains('\\') || value.contains('\0') {
        return false;
    }
    let parts: Vec<_> = value.split('/').collect();
    if parts.iter().any(|part| {
        matches!(
            *part,
            ".." | ".git" | "target" | "node_modules" | "build" | "dist" | "tmp" | "generated"
        )
    }) {
        return false;
    }
    let Some((_, extension)) = value.rsplit_once('.') else {
        return false;
    };
    let target = cwd.join(value);
    let verified = match std::fs::symlink_metadata(&target) {
        Ok(_) => target.canonicalize(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => target
            .parent()
            .ok_or_else(|| std::io::Error::other("missing parent"))
            .and_then(Path::canonicalize),
        Err(error) => Err(error),
    };
    if !verified.is_ok_and(|resolved| resolved.starts_with(root)) {
        return false;
    }
    matches!(
        extension,
        "rs" | "py"
            | "toml"
            | "md"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "json"
            | "yaml"
            | "yml"
            | "sh"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "go"
    )
}

/// Small fail-closed lexer: quoted text and comments are never searched as
/// code; ambiguous string escapes and non-ASCII syntax are rejected.
fn tokenize(source: &str) -> Option<Vec<Token>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' || b == b';' {
            tokens.push(if b == b';' { Token::Semi } else { Token::End });
            i += 1;
            continue;
        }
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'\'' || b == b'"' {
            let quote = b;
            let triple = bytes.get(i..i + 3) == Some(&[quote, quote, quote]);
            i += if triple { 3 } else { 1 };
            let start = i;
            let mut escaped = false;
            let mut has_escape = false;
            loop {
                let c = *bytes.get(i)?;
                if !triple && (c == b'\n' || c == b'\r') {
                    return None;
                }
                if !escaped
                    && c == quote
                    && (!triple || bytes.get(i..i + 3) == Some(&[quote, quote, quote]))
                {
                    break;
                }
                if !escaped && c == b'\\' {
                    escaped = true;
                    has_escape = true;
                } else {
                    escaped = false;
                }
                i += 1;
            }
            let text = std::str::from_utf8(&bytes[start..i]).ok()?;
            let bytes_literal = start >= 2
                && bytes[start - 2] == b'b'
                && matches!(tokens.last(), Some(Token::Name(name)) if name == "b");
            if bytes_literal {
                tokens.pop();
            }
            tokens.push(
                if triple || has_escape || (bytes_literal && !text.is_ascii()) {
                    Token::Opaque
                } else if bytes_literal {
                    Token::BytesLiteral(text.to_owned())
                } else {
                    Token::Literal(text.to_owned())
                },
            );
            i += if triple { 3 } else { 1 };
            continue;
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            tokens.push(Token::Name(
                std::str::from_utf8(&bytes[start..i]).ok()?.to_owned(),
            ));
            continue;
        }
        if !b.is_ascii() {
            return None;
        }
        tokens.push(Token::Punct(char::from(b)));
        i += 1;
    }
    if tokens.len() > 4096 {
        return None;
    }
    tokens.push(Token::End);
    Some(tokens)
}
