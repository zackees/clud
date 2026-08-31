use super::*;
use std::collections::HashMap;

const UNRESOLVED_RM_REASON_PREFIX: &str =
    "Blocked unsafe removal: a path variable could not be proven to contain one nonempty literal path.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RmVariableResolution {
    Unchanged,
    Rewritten(String),
    Deny { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Separator {
    Start,
    Sequence,
    And,
    Or,
    Pipe,
    Background,
}

#[derive(Debug, Clone)]
struct Statement<'a> {
    text: &'a str,
    start: usize,
    separator_before: Separator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteContext {
    Unquoted,
    Double,
}

#[derive(Debug, Clone)]
struct Expansion {
    name: String,
    raw_start: usize,
    raw_end: usize,
    logical_start: usize,
    logical_end: usize,
    quote: QuoteContext,
}

#[derive(Debug, Clone)]
struct Word {
    cooked: String,
    expansions: Vec<Expansion>,
    dynamic: bool,
    /// The word contains a `${...}` parameter expansion using an operator this
    /// interpreter does not model (offset `:o:l`, `:-`, `#`, `%`, `!indirect`,
    /// …). Such a form can yield `/` from a non-root variable
    /// (`${HOME:0:1}` → `/`), so at a removal operand it is an unprovable
    /// hazard regardless of what follows it. (#1073)
    unmodeled_expansion: bool,
    /// The word contains an unquoted brace group `{...}` (#1077).
    brace: bool,
    /// The word begins with a tilde that the shell would expand to a home
    /// directory (`~/`, `~user/`). HOME can be `/`, so this is an unprovable
    /// root-adjacent base. (#1076)
    tilde_at_start: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VariableValue {
    Known(String),
    Unset,
    Conflict,
    /// The variable was assigned a value that is itself an unprovable deletion
    /// base — an unprovable expansion at the value's start immediately followed
    /// by `/` (`D="$V"/`). Using `$D` as a removal operand is then as dangerous
    /// as `$V/`, even without a trailing slash of its own. (#1072)
    Hazard,
}

#[derive(Debug, Clone)]
struct Replacement {
    start: usize,
    end: usize,
    value: String,
}

pub(super) fn resolve_posix_rm_variable_expansions(command: &str) -> RmVariableResolution {
    resolve_posix_rm_variable_expansions_at_depth(command, 0)
}

fn resolve_posix_rm_variable_expansions_at_depth(
    command: &str,
    depth: usize,
) -> RmVariableResolution {
    let scan_command = mask_heredoc_bodies_preserving_offsets(command);
    let statements = match split_statements(&scan_command) {
        Ok(statements) => statements,
        Err(()) => {
            return if looks_like_hazardous_rm(&scan_command) {
                deny("the command has malformed quoting or shell structure")
            } else {
                RmVariableResolution::Unchanged
            };
        }
    };

    if contains_unsupported_control_flow(&statements) && looks_like_hazardous_rm(&scan_command) {
        return deny("control flow prevents proving one value on every path");
    }

    for inner in scan_command_substitutions(&scan_command) {
        if depth >= MAX_SUBSTITUTION_RECURSION_DEPTH {
            // Fail closed at the cap. `looks_like_hazardous_rm` alone misses a
            // hazard buried under further `$( … )` nesting, because its path
            // scanner steps over a whole substitution; flattening the
            // substitution syntax first reveals the residual removal + `$VAR/`
            // so it can be denied instead of allowed. (#1088)
            if looks_like_hazardous_rm(&inner)
                || looks_like_hazardous_rm(&flatten_command_substitutions(&inner))
            {
                return deny("nested shell structure exceeded the static analysis depth limit");
            }
            continue;
        }
        if !matches!(
            resolve_posix_rm_variable_expansions_at_depth(&inner, depth + 1),
            RmVariableResolution::Unchanged
        ) {
            return deny("the removal appears inside a command or process substitution");
        }
    }

    let mut variables = HashMap::<String, VariableValue>::new();
    let mut assignment_counts = HashMap::<String, usize>::new();
    let mut replacements = Vec::<Replacement>::new();
    // Statements whose removal was recognized (`rm`/`rmdir`, possibly wrapped)
    // and whose operands the main loop fully examined — either rewriting each
    // hazardous one or proving it safe. Such a statement cannot leak an
    // unproven hazard, so the fallback skips it; a statement the main loop did
    // NOT recognize is still weighed conservatively there. (#1078)
    let mut proven_statements = std::collections::HashSet::<usize>::new();

    for statement in &statements {
        if matches!(
            statement.separator_before,
            Separator::Pipe | Separator::Background
        ) {
            for value in variables.values_mut() {
                *value = VariableValue::Conflict;
            }
        }
        let words = match lex_words(statement.text, statement.start) {
            Ok(words) => words,
            Err(()) => {
                if looks_like_hazardous_rm(statement.text) {
                    return deny("the removal command could not be tokenized safely");
                }
                continue;
            }
        };
        if words.is_empty() {
            continue;
        }

        apply_assignment_statement(
            &words,
            statement.separator_before,
            &mut variables,
            &mut assignment_counts,
        );
        mark_unsupported_assignment_mutations(&words, &mut variables, &mut assignment_counts);

        let program_index = match removal_program_index(&words) {
            Ok(Some(index)) => index,
            Ok(None) => continue,
            Err(()) => {
                return deny("a nested or wrapped removal command cannot be proven safely");
            }
        };

        for word in &words[program_index + 1..] {
            if word.dynamic
                && (word.cooked.starts_with('$') || word.cooked.starts_with('`'))
                && word.cooked.contains('/')
            {
                return deny("a removal operand uses an indirect or dynamic path expansion");
            }
            // Comprehensive operand hazard analysis (tilde, unmodeled
            // expansions, brace roots, value-carried roots, `$IFS`-split
            // operands, and `$VAR/` shapes the per-expansion loop below does
            // not reach). Proven-safe operands return `None` here and fall
            // through to the rewrite loop. (#1072, #1073, #1074, #1075, #1076,
            // #1077)
            if let Some(reason) = operand_hazard_reason(word, &variables) {
                return deny(reason);
            }
            for expansion in &word.expansions {
                if expansion.logical_start != 0
                    || word.cooked.as_bytes().get(expansion.logical_end) != Some(&b'/')
                {
                    continue;
                }
                let Some(VariableValue::Known(value)) = variables.get(&expansion.name) else {
                    return deny_for_variable(
                        &expansion.name,
                        "its value is unset, dynamic, or ambiguous",
                    );
                };
                if let Some(reason) = unsafe_delete_base_reason(value) {
                    return deny_for_variable(&expansion.name, reason);
                }
                let Some(resolved_operand) = resolve_static_operand(word, &variables) else {
                    return deny("a removal operand contains a dynamic or ambiguous suffix");
                };
                if resolved_operand.contains(['<', '>']) {
                    return deny("a removal operand has an attached shell redirection");
                }
                if let Some(reason) = unsafe_delete_base_reason(&resolved_operand) {
                    return deny_for_variable(&expansion.name, reason);
                }
                if expansion.quote == QuoteContext::Unquoted
                    && value
                        .chars()
                        .any(|ch| ch.is_whitespace() || matches!(ch, '*' | '?' | '[' | ']'))
                {
                    return deny_for_variable(
                        &expansion.name,
                        "an unquoted expansion would change shell splitting or glob semantics",
                    );
                }
                let value = match expansion.quote {
                    QuoteContext::Double => escape_double_quoted(value),
                    QuoteContext::Unquoted => quote_posix_word(value),
                };
                replacements.push(Replacement {
                    start: expansion.raw_start,
                    end: expansion.raw_end,
                    value,
                });
            }
        }
        // The removal was recognized and every operand examined without a
        // denial: this statement is proven and the fallback may skip it.
        proven_statements.insert(statement.start);
    }

    // The fallback runs unconditionally: a sibling operand that WAS rewritten
    // must not disable the check for a different, unproven removal elsewhere in
    // the command. The check is variable-aware, so an operand that was proven
    // safe (and rewritten) is not itself counted as a hazard. (#1078)
    if let Some(reason) = unproven_hazard_reason(&statements, &variables, &proven_statements) {
        return deny(reason);
    }
    if replacements.is_empty() {
        return RmVariableResolution::Unchanged;
    }
    replacements.sort_by_key(|replacement| replacement.start);
    if replacements
        .windows(2)
        .any(|pair| pair[0].end > pair[1].start)
    {
        return deny("overlapping variable expansions could not be rewritten safely");
    }

    let mut rewritten = command.to_string();
    for replacement in replacements.into_iter().rev() {
        rewritten.replace_range(replacement.start..replacement.end, &replacement.value);
    }
    RmVariableResolution::Rewritten(rewritten)
}

fn removal_program_index(words: &[Word]) -> Result<Option<usize>, ()> {
    let mut index = 0usize;
    while words
        .get(index)
        .and_then(|word| assignment_parts(&word.cooked))
        .is_some()
    {
        index += 1;
    }
    let Some(first_word) = words.get(index) else {
        return Ok(None);
    };
    let first = program_name(&first_word.cooked);
    if matches!(first.as_str(), "rm" | "rmdir") {
        return Ok(Some(index));
    }

    if first == "time" {
        index += 1;
        while words
            .get(index)
            .is_some_and(|word| word.cooked.starts_with('-'))
        {
            index += 1;
        }
        let Some(program) = words.get(index) else {
            return Ok(None);
        };
        if matches!(program_name(&program.cooked).as_str(), "rm" | "rmdir") {
            return Ok(Some(index));
        }
        let remaining = words[index..]
            .iter()
            .map(|word| word.cooked.clone())
            .collect::<Vec<_>>();
        return if hazardous_cooked_words(&remaining) {
            Err(())
        } else {
            Ok(None)
        };
    }

    let cooked = words[index..]
        .iter()
        .map(|word| word.cooked.clone())
        .collect::<Vec<_>>();
    if matches!(first.as_str(), "sudo" | "env" | "command" | "exec") {
        let configured = vec!["sudo".to_string()];
        let Some(unwrapped) = unwrap_configured_wrappers(&cooked, &configured) else {
            return if hazardous_cooked_words(&cooked) {
                Err(())
            } else {
                Ok(None)
            };
        };
        let Some(program) = unwrapped.first() else {
            return Ok(None);
        };
        if matches!(program_name(program).as_str(), "rm" | "rmdir") {
            if let Some(offset) = cooked
                .windows(unwrapped.len())
                .position(|candidate| candidate == unwrapped.as_slice())
            {
                return Ok(Some(index + offset));
            }
            return Err(());
        }
        if hazardous_cooked_words(&unwrapped) {
            return Err(());
        }
    }

    if matches!(first.as_str(), "bash" | "sh" | "zsh" | "eval") && hazardous_cooked_words(&cooked) {
        return Err(());
    }
    Ok(None)
}

fn hazardous_cooked_words(words: &[String]) -> bool {
    let joined = words.join(" ");
    let has_removal = contains_removal_program_text(&joined.to_ascii_lowercase())
        || lex_words(&joined, 0).is_ok_and(|nested| {
            nested
                .iter()
                .any(|word| matches!(program_name(&word.cooked).as_str(), "rm" | "rmdir"))
        });
    has_removal && joined.contains('$') && joined.contains('/')
}

fn resolve_static_operand(
    word: &Word,
    variables: &HashMap<String, VariableValue>,
) -> Option<String> {
    if word.dynamic {
        return None;
    }
    let mut resolved = word.cooked.clone();
    for expansion in word.expansions.iter().rev() {
        let VariableValue::Known(value) = variables.get(&expansion.name)? else {
            return None;
        };
        resolved.replace_range(expansion.logical_start..expansion.logical_end, value);
    }
    Some(resolved)
}

pub(super) fn mask_heredoc_bodies_preserving_offsets(command: &str) -> String {
    if !command.contains("<<") {
        return command.to_string();
    }
    let mut masked = command.as_bytes().to_vec();
    let mut lines = Vec::<(usize, usize)>::new();
    let mut start = 0usize;
    for (index, byte) in command.bytes().enumerate() {
        if byte == b'\n' {
            lines.push((start, index));
            start = index + 1;
        }
    }
    lines.push((start, command.len()));

    let mut i = 0usize;
    while i < lines.len() {
        let (line_start, line_end) = lines[i];
        let line = &command[line_start..line_end];
        let Some(delimiter) = find_heredoc_delimiter(line) else {
            i += 1;
            continue;
        };
        let mut terminator = None;
        for (candidate, &(body_start, body_end)) in lines.iter().enumerate().skip(i + 1) {
            let body_line = command[body_start..body_end]
                .trim_start_matches('\t')
                .trim_end_matches('\r');
            if body_line == delimiter {
                terminator = Some(candidate);
                break;
            }
        }
        let Some(terminator) = terminator else {
            break;
        };
        let mask_start = lines[i + 1].0;
        let mask_end = lines[terminator].1;
        for byte in &mut masked[mask_start..mask_end] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
        i = terminator + 1;
    }
    String::from_utf8(masked).expect("masking ASCII bytes preserves UTF-8")
}

fn split_statements(command: &str) -> Result<Vec<Statement<'_>>, ()> {
    let bytes = command.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0usize;
    let mut separator_before = Separator::Start;
    let mut quote = None::<u8>;
    let mut escaped = false;
    let mut paren_depth = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        let byte = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if quote == Some(b'\'') {
            if byte == b'\'' {
                quote = None;
            }
            i += 1;
            continue;
        }
        if quote == Some(b'"') {
            if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quote = None;
            } else if byte == b'$' && bytes.get(i + 1) == Some(&b'(') {
                paren_depth += 1;
                i += 1;
            } else if byte == b')' && paren_depth > 0 {
                paren_depth -= 1;
            }
            i += 1;
            continue;
        }
        match byte {
            b'\\' => escaped = true,
            b'\'' | b'"' => quote = Some(byte),
            b'(' => paren_depth += 1,
            b')' if paren_depth > 0 => paren_depth -= 1,
            b';' | b'\n' | b'\r' if paren_depth == 0 => {
                push_statement(command, start, i, separator_before, &mut statements);
                start = i + 1;
                separator_before = Separator::Sequence;
            }
            b'&' if paren_depth == 0 && bytes.get(i + 1) == Some(&b'&') => {
                push_statement(command, start, i, separator_before, &mut statements);
                start = i + 2;
                separator_before = Separator::And;
                i += 1;
            }
            b'&' if paren_depth == 0
                && bytes.get(i + 1) != Some(&b'>')
                && i.checked_sub(1).and_then(|previous| bytes.get(previous)) != Some(&b'>') =>
            {
                push_statement(command, start, i, separator_before, &mut statements);
                start = i + 1;
                separator_before = Separator::Background;
            }
            b'|' if paren_depth == 0 => {
                push_statement(command, start, i, separator_before, &mut statements);
                if bytes.get(i + 1) == Some(&b'|') {
                    start = i + 2;
                    separator_before = Separator::Or;
                    i += 1;
                } else {
                    start = i + 1;
                    separator_before = Separator::Pipe;
                }
            }
            _ => {}
        }
        i += 1;
    }
    if quote.is_some() || escaped || paren_depth != 0 {
        return Err(());
    }
    push_statement(
        command,
        start,
        command.len(),
        separator_before,
        &mut statements,
    );
    Ok(statements)
}

fn push_statement<'a>(
    command: &'a str,
    start: usize,
    end: usize,
    separator_before: Separator,
    statements: &mut Vec<Statement<'a>>,
) {
    let text = &command[start..end];
    let leading = text.len() - text.trim_start().len();
    let text = text.trim();
    if !text.is_empty() {
        statements.push(Statement {
            text,
            start: start + leading,
            separator_before,
        });
    }
}

fn lex_words(segment: &str, command_offset: usize) -> Result<Vec<Word>, ()> {
    let bytes = segment.as_bytes();
    let mut words = Vec::new();
    let mut word = None::<Word>;
    let mut quote = None::<u8>;
    let mut i = 0usize;

    while i < bytes.len() {
        let byte = bytes[i];
        if quote.is_none() && byte.is_ascii_whitespace() {
            if let Some(word) = word.take() {
                words.push(word);
            }
            i += 1;
            continue;
        }
        let current = word.get_or_insert_with(|| Word {
            cooked: String::new(),
            expansions: Vec::new(),
            dynamic: false,
            unmodeled_expansion: false,
            brace: false,
            tilde_at_start: false,
        });
        if quote == Some(b'\'') {
            if byte == b'\'' {
                quote = None;
            } else {
                push_utf8_char(segment, &mut current.cooked, &mut i)?;
                continue;
            }
            i += 1;
            continue;
        }
        if byte == b'\'' && quote.is_none() {
            quote = Some(b'\'');
            i += 1;
            continue;
        }
        if byte == b'"' {
            quote = if quote == Some(b'"') {
                None
            } else {
                Some(b'"')
            };
            i += 1;
            continue;
        }
        if byte == b'\\' {
            let next_index = i + 1;
            if bytes.get(next_index).is_none() {
                return Err(());
            }
            if quote == Some(b'"')
                && !matches!(bytes[next_index], b'$' | b'`' | b'"' | b'\\' | b'\n')
            {
                current.cooked.push('\\');
            }
            i = next_index;
            push_utf8_char(segment, &mut current.cooked, &mut i)?;
            continue;
        }
        if byte == b'`' {
            current.dynamic = true;
            current.cooked.push('`');
            i += 1;
            continue;
        }
        if matches!(byte, b'<' | b'>') && bytes.get(i + 1) == Some(&b'(') {
            current.dynamic = true;
        }
        if quote.is_none() && byte == b'~' && current.cooked.is_empty() {
            // A leading, unquoted tilde undergoes home-directory expansion.
            current.tilde_at_start = true;
        }
        if quote.is_none()
            && (byte == b'{' || byte == b'}' || (byte == b'~' && current.cooked.ends_with('=')))
        {
            current.dynamic = true;
            // A brace that starts a group (rather than the `{` of a `${...}`
            // parameter expansion, whose `$` we would already have pushed) can
            // undergo brace expansion.
            if byte == b'{' && !current.cooked.ends_with('$') {
                current.brace = true;
            }
        }
        if byte == b'$' {
            if bytes.get(i + 1) == Some(&b'(') {
                current.dynamic = true;
                current.cooked.push('$');
                i += 1;
                continue;
            }
            // ANSI-C quoting: `$'...'` decodes its escapes into a literal
            // string, so `$'\x2f'` is the slash `/` and `$'\x72\x6d'` is the
            // program name `rm`. Decoding lets the ordinary literal path see
            // the real bytes. (#1074)
            if bytes.get(i + 1) == Some(&b'\'') {
                if let Some((decoded, end)) = decode_ansi_c_string(bytes, i) {
                    current.cooked.push_str(&decoded);
                    i = end;
                    continue;
                }
                // An unterminated `$'…` cannot be proven; fall through and let
                // the dynamic marker below flag the word.
            }
            if let Some((name, end)) = parse_simple_expansion(bytes, i) {
                let logical_start = current.cooked.len();
                current.cooked.push_str(&format!("${{{name}}}"));
                let logical_end = current.cooked.len();
                current.expansions.push(Expansion {
                    name,
                    raw_start: command_offset + i,
                    raw_end: command_offset + end,
                    logical_start,
                    logical_end,
                    quote: if quote == Some(b'"') {
                        QuoteContext::Double
                    } else {
                        QuoteContext::Unquoted
                    },
                });
                i = end;
                continue;
            }
            current.dynamic = true;
            // A `${...}` the simple-expansion parser rejected uses an operator
            // we do not model; treat it as unprovable at a removal operand. (#1073)
            if bytes.get(i + 1) == Some(&b'{') {
                current.unmodeled_expansion = true;
            }
        }
        push_utf8_char(segment, &mut current.cooked, &mut i)?;
    }
    if quote.is_some() {
        return Err(());
    }
    if let Some(word) = word {
        words.push(word);
    }
    Ok(words)
}

fn push_utf8_char(segment: &str, output: &mut String, index: &mut usize) -> Result<(), ()> {
    let ch = segment
        .get(*index..)
        .and_then(|tail| tail.chars().next())
        .ok_or(())?;
    output.push(ch);
    *index += ch.len_utf8();
    Ok(())
}

fn parse_simple_expansion(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let mut i = start + 1;
    if bytes.get(i) == Some(&b'{') {
        i += 1;
        let name_start = i;
        while bytes.get(i).is_some_and(|byte| is_name_continue(*byte)) {
            i += 1;
        }
        if i == name_start || bytes.get(i) != Some(&b'}') {
            return None;
        }
        return Some((
            String::from_utf8_lossy(&bytes[name_start..i]).to_string(),
            i + 1,
        ));
    }
    let name_start = i;
    if !bytes.get(i).is_some_and(|byte| is_name_start(*byte)) {
        return None;
    }
    i += 1;
    while bytes.get(i).is_some_and(|byte| is_name_continue(*byte)) {
        i += 1;
    }
    Some((
        String::from_utf8_lossy(&bytes[name_start..i]).to_string(),
        i,
    ))
}

/// Decode an ANSI-C quoted string `$'...'` beginning at `start` (which points
/// at the `$`, with `bytes[start + 1] == b'\''`).
///
/// Returns the decoded literal and the index just past the closing quote, or
/// `None` when the string is never closed. Enough of the escape grammar is
/// handled to unmask a smuggled `/` or program name: `\xHH`, `\0NNN` octal,
/// `\uHHHH`, `\UHHHHHHHH`, and the common single-character escapes. An escape
/// this decoder does not recognize is emitted verbatim (bash keeps the
/// backslash too), which is safe here — the point is only to reveal literal
/// bytes, and over-revealing cannot hide a hazard.
fn decode_ansi_c_string(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let mut i = start + 2; // past `$'`
    let mut out = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => return Some((out, i + 1)),
            b'\\' => {
                let Some(&esc) = bytes.get(i + 1) else {
                    out.push('\\');
                    i += 1;
                    continue;
                };
                match esc {
                    b'n' => {
                        out.push('\n');
                        i += 2;
                    }
                    b't' => {
                        out.push('\t');
                        i += 2;
                    }
                    b'r' => {
                        out.push('\r');
                        i += 2;
                    }
                    b'a' => {
                        out.push('\u{7}');
                        i += 2;
                    }
                    b'b' => {
                        out.push('\u{8}');
                        i += 2;
                    }
                    b'e' | b'E' => {
                        out.push('\u{1b}');
                        i += 2;
                    }
                    b'f' => {
                        out.push('\u{c}');
                        i += 2;
                    }
                    b'v' => {
                        out.push('\u{b}');
                        i += 2;
                    }
                    b'\\' => {
                        out.push('\\');
                        i += 2;
                    }
                    b'\'' => {
                        out.push('\'');
                        i += 2;
                    }
                    b'"' => {
                        out.push('"');
                        i += 2;
                    }
                    b'x' => {
                        let (value, consumed) = read_radix_digits(bytes, i + 2, 16, 2);
                        if consumed == 0 {
                            out.push('\\');
                            out.push('x');
                            i += 2;
                        } else {
                            push_code_point(&mut out, value);
                            i += 2 + consumed;
                        }
                    }
                    b'u' => {
                        let (value, consumed) = read_radix_digits(bytes, i + 2, 16, 4);
                        if consumed == 0 {
                            out.push('\\');
                            out.push('u');
                            i += 2;
                        } else {
                            push_code_point(&mut out, value);
                            i += 2 + consumed;
                        }
                    }
                    b'U' => {
                        let (value, consumed) = read_radix_digits(bytes, i + 2, 16, 8);
                        if consumed == 0 {
                            out.push('\\');
                            out.push('U');
                            i += 2;
                        } else {
                            push_code_point(&mut out, value);
                            i += 2 + consumed;
                        }
                    }
                    b'0'..=b'7' => {
                        let (value, consumed) = read_radix_digits(bytes, i + 1, 8, 3);
                        push_code_point(&mut out, value);
                        i += 1 + consumed;
                    }
                    other => {
                        // Unknown escape: keep both characters verbatim.
                        out.push('\\');
                        out.push(other as char);
                        i += 2;
                    }
                }
            }
            _ => {
                push_utf8_char(
                    std::str::from_utf8(bytes).unwrap_or_default(),
                    &mut out,
                    &mut i,
                )
                .ok()?;
            }
        }
    }
    None
}

/// Read up to `max` digits in `radix` starting at `start`. Returns the decoded
/// value and the number of digits consumed (0 when none were valid).
fn read_radix_digits(bytes: &[u8], start: usize, radix: u32, max: usize) -> (u32, usize) {
    let mut value = 0u32;
    let mut consumed = 0usize;
    while consumed < max {
        let Some(&byte) = bytes.get(start + consumed) else {
            break;
        };
        let Some(digit) = (byte as char).to_digit(radix) else {
            break;
        };
        value = value.saturating_mul(radix).saturating_add(digit);
        consumed += 1;
    }
    (value, consumed)
}

/// Append a decoded code point as a literal character. An invalid code point
/// cannot be a path separator, so it is simply dropped.
fn push_code_point(out: &mut String, value: u32) {
    if let Some(ch) = char::from_u32(value) {
        out.push(ch);
    }
}

fn is_name_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_name_continue(byte: u8) -> bool {
    is_name_start(byte) || byte.is_ascii_digit()
}

fn apply_assignment_statement(
    words: &[Word],
    separator: Separator,
    variables: &mut HashMap<String, VariableValue>,
    counts: &mut HashMap<String, usize>,
) {
    let assignments = if words.first().is_some_and(|word| word.cooked == "export") {
        &words[1..]
    } else {
        words
    };
    if !assignments.is_empty()
        && assignments
            .iter()
            .all(|word| assignment_parts(&word.cooked).is_some())
    {
        for word in assignments {
            let (name, value) = assignment_parts(&word.cooked).expect("checked above");
            let count = counts.entry(name.to_string()).or_default();
            *count += 1;
            let mut state = if *count > 1
                || matches!(
                    separator,
                    Separator::And | Separator::Or | Separator::Pipe | Separator::Background
                )
                || word.dynamic
                || !word.expansions.is_empty()
            {
                VariableValue::Conflict
            } else if value.is_empty() {
                VariableValue::Unset
            } else {
                VariableValue::Known(value.to_string())
            };
            // A value that is itself an unprovable deletion base (`D="$V"/`)
            // poisons `$D` even without a trailing slash of its own. (#1072)
            if matches!(state, VariableValue::Conflict) && assignment_value_is_hazard(value) {
                state = VariableValue::Hazard;
            }
            variables.insert(name.to_string(), state);
        }
        return;
    }
    if words.first().is_some_and(|word| word.cooked == "unset") {
        for word in &words[1..] {
            if is_valid_name(&word.cooked) {
                *counts.entry(word.cooked.clone()).or_default() += 1;
                variables.insert(word.cooked.clone(), VariableValue::Unset);
            }
        }
    }
}

fn mark_unsupported_assignment_mutations(
    words: &[Word],
    variables: &mut HashMap<String, VariableValue>,
    counts: &mut HashMap<String, usize>,
) {
    let is_plain_assignment_statement = words
        .iter()
        .all(|word| assignment_parts(&word.cooked).is_some())
        || (words.first().is_some_and(|word| word.cooked == "export")
            && words[1..]
                .iter()
                .all(|word| assignment_parts(&word.cooked).is_some()));
    if is_plain_assignment_statement {
        return;
    }
    for word in words {
        let Some((left, _)) = word.cooked.split_once('=') else {
            continue;
        };
        let name = left.trim_end_matches('+');
        if is_valid_name(name) {
            *counts.entry(name.to_string()).or_default() += 1;
            variables.insert(name.to_string(), VariableValue::Conflict);
        }
    }
}

fn assignment_parts(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    is_valid_name(name).then_some((name, value))
}

/// Whether an assignment's (cooked) value is itself an unprovable deletion
/// base: it begins with a parameter expansion whose next character is `/`
/// (`"$V"/` cooks to `${V}/`). With the leading variable unprovable, the value
/// can be `/` (empty variable) or `//…` — so `$D` is as dangerous as `$V/`,
/// with or without a trailing slash of its own. (#1072)
fn assignment_value_is_hazard(value_cooked: &str) -> bool {
    let Some(rest) = value_cooked.strip_prefix("${") else {
        return false;
    };
    let Some(close) = rest.find('}') else {
        return false;
    };
    rest.as_bytes().get(close + 1) == Some(&b'/')
}

fn is_valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(is_name_start) && bytes.all(is_name_continue)
}

fn contains_unsupported_control_flow(statements: &[Statement<'_>]) -> bool {
    statements.iter().any(|statement| {
        let first = statement
            .text
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default();
        matches!(
            first,
            "if" | "then"
                | "elif"
                | "else"
                | "fi"
                | "for"
                | "while"
                | "until"
                | "case"
                | "esac"
                | "select"
                | "function"
                | "eval"
        ) || statement.text.starts_with('(')
            || statement.text.starts_with('{')
            || statement.text.contains("()")
    })
}

/// Strip command- and process-substitution syntax so a hazard buried under
/// deep `$( … )` / backtick nesting becomes visible to the flat text probes.
/// Parameter expansions (`${V}`) are left intact.
fn flatten_command_substitutions(text: &str) -> String {
    text.replace("$(", " ").replace(['(', ')', '`'], " ")
}

fn looks_like_hazardous_rm(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    contains_removal_program_text(&lower)
        && (contains_path_variable_prefix(command)
            || (lower.contains("eval") && command.contains('$')))
}

/// Whether raw, unparsed payload bytes appear to name a removal program.
///
/// This is deliberately *not* [`contains_removal_program_text`], which reads
/// shell command text that has already been extracted from the payload. Here
/// there is no extracted command — parsing is precisely what failed — so the
/// input is a JSON fragment that may be truncated mid-token, and three of its
/// properties break the command-text probe:
///
/// - A newline inside a JSON string is the two characters `\` and `n`, not
///   whitespace. `cd /tmp\nrm -rf $SP/` puts a literal `n` before the `rm`.
/// - Truncation is one of the paths that lands here, so the bytes can simply
///   stop after `rm`, leaving no following character to inspect.
/// - The removal may be invoked by path (`/bin/rm`, `\usr\bin\rm.exe`).
///
/// So this scans for the removal as a *word*: escape sequences collapse to
/// separators, tokens are cut on anything that cannot appear in a command
/// name, and end-of-input counts as a boundary. It over-matches on purpose —
/// `git rm` in a commit message trips it, and that costs one retry, while
/// under-matching costs a filesystem.
///
/// Covers `rm` and `rmdir` only, matching the interpreter it backstops. Other
/// destructive verbs (`shred`, `unlink`, `find -delete`, `git clean -xfd`) are
/// deliberately out of scope: each widens the over-match, and only `rm` has
/// incident evidence behind it.
pub(super) fn raw_payload_mentions_removal(raw: &str) -> bool {
    let decoded = decode_short_unicode_escapes(&raw.to_ascii_lowercase());
    // Two readings of the same bytes, because the payload is malformed and
    // there is no telling which one the writer meant. Under JSON rules `\n` is
    // a newline and its `n` must vanish, or `cd /tmp\nrm` hides the removal;
    // under literal rules the backslash is just a separator, and swallowing
    // the next character would hide the `rm` in `\rm` (the shell idiom for
    // bypassing an alias). Neither reading subsumes the other, so a match in
    // either is a match — over-matching is the safe direction here.
    [
        flatten_escape_sequences(&decoded),
        strip_backslashes(&decoded),
        collapse_word_concatenation(&decoded),
    ]
    .iter()
    .any(|text| {
        text.split(|ch: char| !is_command_name_char(ch))
            .any(is_removal_program_word)
    })
}

/// A third reading that mirrors the shell's *concatenation* of adjacent quoted
/// and escaped chunks inside one word: `r''m`, `r""m`, `"r"m` and `r\m` all
/// name `rm`. The other two readings turn a quote or backslash into a
/// separator, which splits these into `r` and `m` and misses them; here a
/// quote or backslash wedged between two command-name characters is dropped so
/// the halves rejoin. Empty quote pairs are collapsed first. Over-matching is
/// acceptable — this path runs only on already-unparseable payloads. (#1085)
fn collapse_word_concatenation(raw: &str) -> String {
    let without_empty_pairs = raw.replace("''", "").replace("\"\"", "");
    let chars: Vec<char> = without_empty_pairs.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if matches!(ch, '\'' | '"' | '\\') {
            let prev_joins = out.chars().next_back().is_some_and(is_command_name_char);
            let next_joins = chars
                .get(index + 1)
                .copied()
                .is_some_and(is_command_name_char);
            if prev_joins || next_joins {
                index += 1;
                continue;
            }
        }
        out.push(ch);
        index += 1;
    }
    out
}

/// Resolve `\u00XX` escapes to the character they denote.
///
/// A JSON encoder has no reason to escape an ASCII letter, but this probe runs
/// precisely when the payload did not come out of a well-behaved encoder, and
/// `rm` would otherwise tokenize to `0072`/`006d` and hide a
/// removal. Only ASCII is decoded; anything else is left as written, since no
/// command name this looks for lives outside it.
fn decode_short_unicode_escapes(raw: &str) -> String {
    if !raw.contains("\\u") {
        return raw.to_string();
    }
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0usize;
    while i < chars.len() {
        let is_escape = chars[i] == '\\' && chars.get(i + 1) == Some(&'u') && i + 5 < chars.len();
        if is_escape {
            let digits: String = chars[i + 2..i + 6].iter().collect();
            if let Some(decoded) = u32::from_str_radix(&digits, 16)
                .ok()
                .filter(|code| *code < 0x80)
                .and_then(char::from_u32)
            {
                out.push(decoded);
                i += 6;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Replace each backslash with a space, keeping the character behind it.
///
/// The literal reading: `\rm` becomes ` rm`, so an alias-bypassing removal is
/// still seen as the word `rm`.
fn strip_backslashes(raw: &str) -> String {
    raw.replace('\\', " ")
}

/// Replace every `\<char>` pair with two spaces.
///
/// JSON escapes (`\n`, `\t`, `\"`, `\\`) and shell line continuations both
/// become separators, which is all this probe needs from them, and Windows
/// path separators fall away so `\usr\bin\rm.exe` tokenizes to `rm.exe`.
/// Both characters are dropped, not just the backslash: `\n` must not leave an
/// `n` behind, or `cd /tmp\nrm -rf …` reads as the word `nrm` and the removal
/// hides. The literal reading of the same bytes is covered separately by
/// [`strip_backslashes`].
fn flatten_escape_sequences(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            out.push(' ');
            if chars.next().is_some() {
                out.push(' ');
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Characters that can appear inside a command name as written on a command
/// line. `/` is included so a path-qualified removal stays one token; `-` and
/// `.` are included so `--rm` and `alarm.sh` stay whole and do *not* match.
fn is_command_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/')
}

fn is_removal_program_word(word: &str) -> bool {
    let word = word.strip_suffix(".exe").unwrap_or(word);
    matches!(word, "rm" | "rmdir") || word.ends_with("/rm") || word.ends_with("/rmdir")
}

fn contains_removal_program_text(command: &str) -> bool {
    command.match_indices("rm").any(|(start, program)| {
        let end = start + program.len();
        let previous = command[..start].chars().next_back();
        let next = command[end..].chars().next();
        previous.is_none_or(|ch| ch.is_ascii_whitespace() || ";|&(){}'\"`\\".contains(ch))
            && next.is_some_and(is_word_close)
    }) || command.match_indices("rmdir").any(|(start, program)| {
        let end = start + program.len();
        let previous = command[..start].chars().next_back();
        let next = command[end..].chars().next();
        previous.is_none_or(|ch| ch.is_ascii_whitespace() || ";|&(){}'\"`\\".contains(ch))
            && next.is_some_and(is_word_close)
    })
}

/// A character that can terminate a command word. Whitespace is the usual
/// one, but a program name produced by substitution closes with `)` or a
/// backtick — `$(which rm) -rf "$V"/` runs a removal just as surely as
/// `rm -rf "$V"/` does.
fn is_word_close(ch: char) -> bool {
    ch.is_ascii_whitespace() || ";|&)}'\"`".contains(ch)
}

/// The last line of defence: a removal and a `$VAR/` operand sit in the same
/// pipeline, yet the interpreter proved nothing about it.
///
/// Reaching here means the main loop never recognized the removal's operand —
/// because the program was not spelled `rm`/`rmdir` (`busybox rm`, `nice rm`,
/// `$(echo rm)`), or because the path never reached the removal as an argument
/// at all (`echo "$V"/ | xargs rm -rf`). The interpreter's whole contract is
/// that it *proves* a removal safe before allowing it, so "I did not
/// understand this" must resolve to a denial, not to silence. Without this,
/// every construct the analyzer does not model is a bypass, and the list of
/// such constructs is open-ended.
///
/// Scoped to a pipeline rather than the whole command so that an unrelated
/// safe removal beside an unrelated variable — `rm -rf /tmp/x && echo "$V"/` —
/// is not swept up. Within one pipeline the stages share data, so a `$VAR/`
/// upstream of an `xargs rm` is exactly the hazard.
fn unproven_hazard_reason(
    statements: &[Statement<'_>],
    variables: &HashMap<String, VariableValue>,
    proven: &std::collections::HashSet<usize>,
) -> Option<&'static str> {
    for pipeline in pipeline_groups(statements) {
        // A statement the main loop proved (recognized removal, operands all
        // examined) cannot leak an unproven hazard, so it is excluded from
        // both sides of the test. Everything else is weighed conservatively:
        // a word that merely *names* `rm` (a `sudo -u rm` username) beside a
        // `$VAR/` operand is refused even when that variable is known-safe,
        // because the guard cannot prove which one will run.
        let has_removal = pipeline.iter().any(|statement| {
            !proven.contains(&statement.start) && statement_bears_a_removal(statement, variables)
        });
        let has_path_variable = pipeline.iter().any(|statement| {
            !proven.contains(&statement.start) && statement_has_hazard_operand(statement, variables)
        });
        if has_removal && has_path_variable {
            return Some(
                "a removal shares a pipeline with a `$VAR/` path that could not be proven safe",
            );
        }
    }
    None
}

/// Whether a statement carries an unprovable removal operand.
///
/// This is the union of two probes, because each catches what the other
/// misses. The text probe [`contains_path_variable_prefix`] flags any `$VAR/`
/// shape (including special parameters like `$1`, and known-safe variables that
/// were never validated because the statement's program was not a recognized
/// removal). The word probe [`operand_hazard_reason`] additionally flags shapes
/// with no trailing slash that are hazardous anyway — an unmodeled `${...}`
/// operator, a leading tilde, a root-producing brace group. When the statement
/// cannot be lexed, only the text probe applies.
fn statement_has_hazard_operand(
    statement: &Statement<'_>,
    variables: &HashMap<String, VariableValue>,
) -> bool {
    if contains_path_variable_prefix(statement.text) {
        return true;
    }
    let Ok(words) = lex_words(statement.text, statement.start) else {
        return false;
    };
    words
        .iter()
        .any(|word| operand_hazard_reason(word, variables).is_some())
}

/// Whether a statement could run a removal, judged by the words it contains.
///
/// Any word naming `rm`/`rmdir` counts, wherever it sits. An earlier version
/// tried to be cleverer by ignoring a name that followed an option — reasoning
/// that in `sudo -u rm echo …` the `rm` is a *username*, not a program. That
/// heuristic is not sound in the direction that matters: in `xargs -0 rm -rf`
/// and `find … -exec rm -rf {} +` the name also follows an option, and there
/// it really is the program about to run. Telling those apart needs a table of
/// which options take values for every launcher that exists, so the rule is
/// dropped in favour of the safe direction. The cost is that a contrived
/// `sudo -u rm echo "$V"/*` is refused; the alternative cost is a filesystem.
///
/// `find … -delete` is included because it *is* a recursive removal wearing a
/// different name — `find / -delete` is the same catastrophe as `rm -rf /`.
/// Other destructive verbs stay out of scope (DD-057): `shred` and friends act
/// on named files and cannot recurse into a root.
///
/// A statement that cannot be lexed falls back to the raw-text probe, since an
/// unlexable statement is exactly when guessing low is most dangerous.
///
/// Beyond a literally-spelled `rm`/`rmdir` and `find … -delete`, this also
/// recognizes: a removal whose program name is built from a variable or
/// substitution (`$R -rf …`, `$(which rm) …`) that cannot be proven to be a
/// non-removal (#1071); a removal split apart by unquoted `$IFS`
/// (`rm${IFS}-rf …`, #1075); and a curated set of well-known recursive-delete
/// idioms in other programs (#1079).
fn statement_bears_a_removal(
    statement: &Statement<'_>,
    variables: &HashMap<String, VariableValue>,
) -> bool {
    let Ok(words) = lex_words(statement.text, statement.start) else {
        return contains_removal_program_text(&statement.text.to_ascii_lowercase());
    };
    let names_a_removal = words.iter().any(|word| {
        matches!(
            program_name(&word.cooked).to_ascii_lowercase().as_str(),
            "rm" | "rmdir"
        )
    });
    let deletes_via_find = words
        .first()
        .is_some_and(|word| program_name(&word.cooked).eq_ignore_ascii_case("find"))
        && words.iter().any(|word| word.cooked == "-delete");
    // The word scan sees `$(which rm)` as one opaque word, so the raw-text
    // probe runs too: a removal built at runtime is still a removal.
    names_a_removal
        || deletes_via_find
        || words.iter().any(word_ifs_splits_to_removal)
        || first_program_word_is_potential_removal(&words, variables)
        || statement_bears_extra_deleter(&words)
        || contains_removal_program_text(&statement.text.to_ascii_lowercase())
}

/// Whether an unquoted `$IFS` word-splits a token so that its first field names
/// a removal — `rm${IFS}-rf${IFS}"$V"/` is `rm -rf "$V"/` after splitting.
/// A modeled `$IFS`/`${IFS}` renders as the literal `${IFS}` in the cooked
/// token, so splitting on that string recovers the fields. (#1075)
fn word_ifs_splits_to_removal(word: &Word) -> bool {
    if !word
        .expansions
        .iter()
        .any(|expansion| expansion.name == "IFS" && expansion.quote == QuoteContext::Unquoted)
    {
        return false;
    }
    let first = word
        .cooked
        .split("${IFS}")
        .find(|field| !field.is_empty())
        .unwrap_or_default();
    matches!(program_name(first).as_str(), "rm" | "rmdir")
}

/// Whether the program word (the first word past any leading assignments) is a
/// removal that is built dynamically and cannot be proven to be something
/// else. A dynamic program word that resolves to a known non-removal literal
/// is *not* flagged; one that resolves to `rm`/`rmdir`, or that cannot be
/// resolved at all, is. (#1071)
fn first_program_word_is_potential_removal(
    words: &[Word],
    variables: &HashMap<String, VariableValue>,
) -> bool {
    let mut index = 0usize;
    while words
        .get(index)
        .and_then(|word| assignment_parts(&word.cooked))
        .is_some()
    {
        index += 1;
    }
    let Some(first) = words.get(index) else {
        return false;
    };
    let is_dynamic_program =
        first.dynamic || first.unmodeled_expansion || !first.expansions.is_empty();
    if !is_dynamic_program {
        // An ordinary literal program name is handled by the caller's other
        // branches; nothing dynamic to reason about here.
        return false;
    }
    if !first.dynamic && !first.unmodeled_expansion {
        if let Some(resolved) = resolve_static_operand(first, variables) {
            return matches!(program_name(&resolved).as_str(), "rm" | "rmdir");
        }
    }
    // The program word is dynamic and unprovable: it cannot be shown to be a
    // non-removal, so — paired with a hazardous operand by the caller — it is
    // treated as a removal.
    true
}

/// A curated, best-effort denylist of well-known recursive-delete idioms that
/// wear a program name other than `rm`/`rmdir`/`find -delete`.
///
/// This is deliberately *not* exhaustive (see DD-057): recursive deletion is an
/// open-ended space, and the real fix is the post-expansion wrapper (#1067).
/// It fires only alongside a `$VAR/`-rooted operand, and covers the confirmed
/// incident shapes: Perl `File::Path` (`rmtree`/`remove_tree`), Python
/// `shutil.rmtree`/`os.removedirs`, `rsync … --delete`, and
/// `find … -exec <non-rm deleter>`. (#1079)
fn statement_bears_extra_deleter(words: &[Word]) -> bool {
    let names: Vec<String> = words
        .iter()
        .map(|word| program_name(&word.cooked))
        .collect();
    let joined_lower = words
        .iter()
        .map(|word| word.cooked.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let has = |name: &str| names.iter().any(|candidate| candidate == name);

    if has("perl") && (joined_lower.contains("rmtree") || joined_lower.contains("remove_tree")) {
        return true;
    }
    if (has("python") || has("python2") || has("python3"))
        && (joined_lower.contains("rmtree") || joined_lower.contains("removedirs"))
    {
        return true;
    }
    if has("rsync")
        && words
            .iter()
            .any(|word| word.cooked == "--delete" || word.cooked.starts_with("--delete-"))
    {
        return true;
    }
    if names.first().is_some_and(|first| first == "find") {
        for (index, word) in words.iter().enumerate() {
            if word.cooked == "-exec" || word.cooked == "-execdir" {
                if let Some(next) = words.get(index + 1) {
                    if matches!(
                        program_name(&next.cooked).as_str(),
                        "rm" | "rmdir"
                            | "perl"
                            | "python"
                            | "python2"
                            | "python3"
                            | "unlink"
                            | "shred"
                    ) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Whether a removal operand could expand to a filesystem root (or another
/// unprovable dangerous base), judged with the variable model. Returns a deny
/// reason, or `None` when the operand is provably safe.
///
/// Covers: a leading tilde (`~/`, home may be `/`); an unmodeled `${...}`
/// operator (can be `/`); brace expansion that can produce a root; a variable
/// whose *value* is an unprovable base (`D="$V"/`); and — after honouring
/// proven values and unquoted `$IFS` word-splitting — a value-carried root
/// (`D=/`) or an unprovable expansion at a field's start followed by `/`
/// (`$V/`, `$V$S` with `S=/`). A field that resolves to a proven-safe literal
/// returns `None`, so ordinary cleanup still rewrites.
fn operand_hazard_reason(
    word: &Word,
    variables: &HashMap<String, VariableValue>,
) -> Option<&'static str> {
    if word.tilde_at_start {
        return Some("a removal operand begins with a tilde that expands to a home directory");
    }
    if word.unmodeled_expansion {
        return Some("a removal operand uses a parameter expansion this guard cannot model");
    }
    // A dynamic operand whose path begins with a substitution or special
    // parameter (`$1/`, `$@/`, `$(cmd)/`, `` `cmd`/ ``) cannot be proven; the
    // slash after it makes it a `$VAR/`-style hazard.
    if word.dynamic
        && (word.cooked.starts_with('$') || word.cooked.starts_with('`'))
        && word.cooked.contains('/')
    {
        return Some("a removal operand uses an indirect or dynamic path expansion");
    }
    if word
        .expansions
        .iter()
        .any(|expansion| matches!(variables.get(&expansion.name), Some(VariableValue::Hazard)))
    {
        return Some(
            "a removal operand uses a variable whose value is an unprovable deletion base",
        );
    }
    if word.brace && brace_operand_can_be_root(&word.cooked) {
        return Some("a removal operand uses brace expansion that can produce a filesystem root");
    }
    for field in operand_fields(word, variables) {
        if let Some(reason) = field_hazard_reason(&field) {
            return Some(reason);
        }
    }
    None
}

/// Sentinel marking an unprovable expansion inside a reconstructed operand
/// field. It is not a valid path character, so it never collides with real
/// content.
const UNPROVABLE: char = '\u{1}';

/// Reconstruct the operand's fields as the shell would produce them: proven
/// variable values are substituted in, unprovable expansions become
/// [`UNPROVABLE`], and an unquoted `$IFS` starts a new field.
fn operand_fields(word: &Word, variables: &HashMap<String, VariableValue>) -> Vec<String> {
    let cooked = &word.cooked;
    let mut expansions: Vec<&Expansion> = word.expansions.iter().collect();
    expansions.sort_by_key(|expansion| expansion.logical_start);

    let mut fields = Vec::new();
    let mut current = String::new();
    let mut pos = 0usize;
    for expansion in expansions {
        if expansion.logical_start < pos {
            continue;
        }
        current.push_str(&cooked[pos..expansion.logical_start]);
        pos = expansion.logical_end;
        if expansion.name == "IFS" && expansion.quote == QuoteContext::Unquoted {
            fields.push(std::mem::take(&mut current));
            continue;
        }
        match variables.get(&expansion.name) {
            Some(VariableValue::Known(value)) => current.push_str(value),
            _ => current.push(UNPROVABLE),
        }
    }
    current.push_str(&cooked[pos..]);
    fields.push(current);
    fields
}

/// Whether a single reconstructed operand field could delete a root.
fn field_hazard_reason(field: &str) -> Option<&'static str> {
    if !field.contains(UNPROVABLE) {
        if field.is_empty() {
            // An empty operand deletes nothing.
            return None;
        }
        return unsafe_delete_base_reason(field);
    }
    // The field carries an unprovable expansion. The catastrophe is one at the
    // field's start immediately followed by `/`: an empty variable yields `/`,
    // a `/` value yields `//`. An unprovable expansion NOT followed by `/`
    // (`rm -rf "$V"`) expands to an empty operand and is left alone.
    let stripped = field.trim_start_matches(UNPROVABLE);
    if stripped.len() != field.len() && stripped.starts_with('/') {
        return Some("a removal operand starts with an unprovable expansion followed by a slash");
    }
    None
}

/// Whether a literal brace operand can expand to a filesystem root, e.g.
/// `{/,}` → `/`, `/{,}` → `/`, `{/,/tmp}` → `/`. Benign forms like
/// `{a,b}.txt` cannot. (#1077)
fn brace_operand_can_be_root(cooked: &str) -> bool {
    match expand_braces(cooked) {
        Some(expansions) => expansions
            .iter()
            .any(|candidate| unsafe_delete_base_reason(candidate).is_some()),
        // A brace group we could not expand (nested variables, malformed) is
        // treated conservatively as a hazard.
        None => true,
    }
}

/// Expand literal brace groups into the set of strings they produce. Returns
/// `None` when the input contains no brace group or cannot be expanded with the
/// small model here (unbalanced, or containing a dynamic expansion). Bounded to
/// keep a pathological input cheap.
fn expand_braces(input: &str) -> Option<Vec<String>> {
    if !input.contains('{') {
        return None;
    }
    // A brace group holding a dynamic expansion is beyond this literal model.
    if input.contains('$') || input.contains('`') {
        return None;
    }
    let expanded = expand_braces_recursive(input, 0)?;
    (!expanded.is_empty()).then_some(expanded)
}

fn expand_braces_recursive(input: &str, depth: usize) -> Option<Vec<String>> {
    const MAX_DEPTH: usize = 8;
    const MAX_RESULTS: usize = 256;
    if depth > MAX_DEPTH {
        return None;
    }
    let bytes = input.as_bytes();
    // Find the first top-level `{ … , … }` group.
    let open = bytes.iter().position(|&byte| byte == b'{')?;
    let mut depth_count = 0usize;
    let mut close = None;
    let mut alternatives = Vec::new();
    let mut segment_start = open + 1;
    let mut has_comma = false;
    for (index, &byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth_count += 1,
            b'}' => {
                depth_count -= 1;
                if depth_count == 0 {
                    alternatives.push(&input[segment_start..index]);
                    close = Some(index);
                    break;
                }
            }
            b',' if depth_count == 1 => {
                has_comma = true;
                alternatives.push(&input[segment_start..index]);
                segment_start = index + 1;
            }
            _ => {}
        }
    }
    let close = close?;
    if !has_comma {
        // `{x}` with no comma is not a brace expansion in bash; leave it.
        return None;
    }
    let prefix = &input[..open];
    let suffix = &input[close + 1..];
    let mut results = Vec::new();
    for alternative in alternatives {
        let candidate = format!("{prefix}{alternative}{suffix}");
        if candidate.contains('{') {
            match expand_braces_recursive(&candidate, depth + 1) {
                Some(nested) => results.extend(nested),
                None => results.push(candidate),
            }
        } else {
            results.push(candidate);
        }
        if results.len() > MAX_RESULTS {
            break;
        }
    }
    Some(results)
}

/// Split statements into pipeline groups. A `|` continues the current group;
/// every other separator starts a new one.
fn pipeline_groups<'a, 'b>(statements: &'b [Statement<'a>]) -> Vec<Vec<&'b Statement<'a>>> {
    let mut groups: Vec<Vec<&Statement<'_>>> = Vec::new();
    for statement in statements {
        if statement.separator_before == Separator::Pipe {
            if let Some(current) = groups.last_mut() {
                current.push(statement);
                continue;
            }
        }
        groups.push(vec![statement]);
    }
    groups
}

fn contains_path_variable_prefix(command: &str) -> bool {
    let bytes = command.as_bytes();
    let mut single_quoted = false;
    let mut escaped = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let byte = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if byte == b'\\' && !single_quoted {
            escaped = true;
            i += 1;
            continue;
        }
        if byte == b'\'' {
            single_quoted = !single_quoted;
            i += 1;
            continue;
        }
        if byte == b'$' && !single_quoted {
            if let Some(end) = any_expansion_end(bytes, i) {
                if slash_follows_ignoring_quotes(bytes, end) {
                    return true;
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    false
}

/// The end of *any* expansion beginning at `start`, not just one whose value
/// the interpreter can reason about.
///
/// [`parse_simple_expansion`] deliberately recognizes only `$name` and
/// `${name}`, because those are the forms it can resolve. This scanner answers
/// a different question — "is a value being substituted here at all?" — so it
/// must also accept the forms it cannot resolve: `${V:-/}`, `${V#x}`,
/// `${!V}`, `$(cmd)`. Those are *more* dangerous, not less: `${V:-/}` has `/`
/// as its literal default. Treating them as "not an expansion" is what let a
/// removal behind an unmodelled launcher or inside a subshell slip past every
/// fallback.
fn any_expansion_end(bytes: &[u8], start: usize) -> Option<usize> {
    match bytes.get(start + 1) {
        Some(b'{') => matching_close(bytes, start + 1, b'{', b'}'),
        Some(b'(') => matching_close(bytes, start + 1, b'(', b')'),
        // Positional and special parameters are one character and are not
        // valid identifiers, so the name parser rejects them. They matter
        // here: `$1` is unset in any shell that was not passed an argument,
        // so `rm -rf "$1"/` is the same catastrophe under a different name.
        Some(byte) if byte.is_ascii_digit() || b"@*#?$!-_".contains(byte) => Some(start + 2),
        _ => parse_simple_expansion(bytes, start).map(|(_, end)| end),
    }
}

/// Index just past the delimiter that closes the one at `open`, honouring
/// nesting. `None` when it is never closed, which the callers treat as "no
/// expansion here" — an unbalanced command is caught by the lexer instead.
fn matching_close(bytes: &[u8], open: usize, opener: u8, closer: u8) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = open;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == opener {
            depth += 1;
        } else if byte == closer {
            depth -= 1;
            if depth == 0 {
                return Some(index + 1);
            }
        }
        index += 1;
    }
    None
}

/// Whether a `/` follows the expansion that ended at `end`, looking past any
/// quote characters that close or reopen around it.
///
/// The shell joins adjacent quoted chunks into one word, so `"$V"/`, `"$V"'/'`
/// and `"$V""/"` all expand exactly like `$V/`. Checking only the byte
/// immediately after the expansion sees the closing `"` and concludes there is
/// no slash — which silently disabled [`looks_like_hazardous_rm`] for the most
/// common spelling of the hazard, and with it every fallback that depends on
/// it. Whitespace is *not* skipped: `echo "$V" /tmp` is two separate words and
/// carries no risk.
fn slash_follows_ignoring_quotes(bytes: &[u8], end: usize) -> bool {
    let mut index = end;
    while matches!(bytes.get(index), Some(b'"' | b'\'')) {
        index += 1;
    }
    bytes.get(index) == Some(&b'/')
}

fn unsafe_delete_base_reason(value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return Some("its value is empty");
    }
    if value.contains(['\0', '\n', '\r']) {
        return Some("its value contains control characters");
    }
    if value.contains('\\') {
        return Some("its POSIX value contains a literal backslash");
    }
    let normalized = value.to_string();
    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." || trimmed.starts_with("../") {
        return Some("its normalized value is a filesystem root or relative escape");
    }
    if normalized.starts_with("//") {
        let Some(components) = normalized_component_count(trimmed.trim_start_matches('/')) else {
            return Some("its normalized value escapes its UNC root");
        };
        if components <= 2 {
            return Some("its normalized value is a UNC root or share root");
        }
    } else if normalized.len() >= 3
        && normalized.starts_with('/')
        && normalized.as_bytes()[1].is_ascii_alphabetic()
        && normalized.as_bytes()[2] == b'/'
    {
        let Some(components) = normalized_component_count(
            normalized
                .get(3..)
                .unwrap_or_default()
                .trim_end_matches('/'),
        ) else {
            return Some("its normalized value escapes the MSYS drive root");
        };
        if components <= 1 {
            return Some("its normalized value is an MSYS drive root or top-level directory");
        }
    } else if normalized.starts_with('/') {
        let Some(components) = normalized_component_count(trimmed.trim_start_matches('/')) else {
            return Some("its normalized value escapes the filesystem root");
        };
        if components <= 1 {
            return Some("its normalized value is a filesystem root or top-level directory");
        }
    } else if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        if !normalized.as_bytes()[0].is_ascii_alphabetic() || !normalized[2..].starts_with('/') {
            return Some("its Windows drive path is not absolute");
        }
        let Some(components) = normalized_component_count(
            normalized
                .get(3..)
                .unwrap_or_default()
                .trim_end_matches('/'),
        ) else {
            return Some("its normalized value escapes the drive root");
        };
        if components <= 1 {
            return Some("its normalized value is a drive root or top-level directory");
        }
    } else if normalized_component_count(trimmed).is_none() {
        return Some("its normalized relative value escapes its working directory");
    }
    None
}

fn normalized_component_count(path: &str) -> Option<usize> {
    let mut depth = 0usize;
    for component in path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if component == ".." {
            depth = depth.checked_sub(1)?;
        } else if component.contains(['*', '?', '[']) {
            // A glob can select descendants, but it cannot prove that the
            // deletion base itself is deeper than a protected root.
            continue;
        } else {
            depth += 1;
        }
    }
    Some(depth)
}

fn escape_double_quoted(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        if matches!(ch, '\\' | '"' | '$' | '`') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn quote_posix_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn deny(detail: &str) -> RmVariableResolution {
    RmVariableResolution::Deny {
        reason: format!(
            "{UNRESOLVED_RM_REASON_PREFIX} {detail}. Retry using the validated literal path directly."
        ),
    }
}

fn deny_for_variable(name: &str, detail: &str) -> RmVariableResolution {
    RmVariableResolution::Deny {
        reason: format!(
            "Blocked unsafe removal: ${name} could not be proven to contain one nonempty literal path because {detail}. Retry using the validated literal path directly."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(command: &str) -> String {
        match resolve_posix_rm_variable_expansions(command) {
            RmVariableResolution::Rewritten(command) => command,
            other => panic!("expected rewrite, got {other:?}"),
        }
    }

    fn denied(command: &str) -> bool {
        matches!(
            resolve_posix_rm_variable_expansions(command),
            RmVariableResolution::Deny { .. }
        )
    }

    /// #1071 — the removal program is named by a variable or substitution. The
    /// analyzer even records `R=Known("rm")`; it must resolve that into the
    /// program slot and, unable to prove the first word is a non-removal while a
    /// `$VAR/` operand is present, deny.
    #[test]
    fn issue_1071_program_name_via_variable_or_substitution_is_denied() {
        for command in [
            r#"R=rm; $R -rf "$V"/"#,
            r#"X=r; Y=m; ${X}${Y} -rf "$V"/"#,
            r#"$(printf 'r''m') -rf "$V"/"#,
            r#"p=rm; "$p" -rf "$V"/"#,
            r#"`which rm` -rf "$V"/"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    /// #1072 — a variable's *value* carries the slash or the root, so the
    /// hazard is not textually adjacent to the expansion.
    #[test]
    fn issue_1072_value_carried_slash_or_root_is_denied() {
        for command in [
            r#"D=/; rm -rf "$D""#,
            r#"D="$V"/; rm -rf "$D""#,
            r#"S=/; rm -rf "$V$S""#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
        // Control: the textually-adjacent form must stay denied too.
        assert!(denied(r#"V=/; rm -rf "$V"/"#));
    }

    /// #1073 — substring/offset and other `${...}` operators this guard does
    /// not model can yield `/` from a non-root variable (`${HOME:0:1}` → `/`).
    #[test]
    fn issue_1073_unmodeled_parameter_operators_are_denied() {
        for command in [
            r#"V=/tmp/safe; rm -rf ${V:0:1}*"#,
            r#"rm -rf ${HOME:0:1}"#,
            r#"rm -rf ${HOME::1}"#,
            r#"find ${HOME:0:1} -delete"#,
            r#"echo ${HOME:0:1} | xargs rm -rf"#,
            r#"rm -rf ${HOME:0:1}etc"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    /// #1074 — ANSI-C quoting `$'...'` decodes escapes into literal bytes, so a
    /// smuggled `/` (as operand) or `rm` (as program) must be seen.
    #[test]
    fn issue_1074_ansi_c_quoting_is_decoded_and_denied() {
        for command in [
            r#"rm -rf $'\x2f'"#,
            r#"rm -rf $'\057'"#,
            r#"rm -rf $'\U0000002f'"#,
            r#"V=; rm -rf "$V"$'\x2f'"#,
            r#"$'\x72\x6d' -rf "$V"/"#,
            r#"$'\162\155' -rf "$V"/"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
        // Control: a benign ANSI-C literal in a non-removal is untouched.
        assert_eq!(
            resolve_posix_rm_variable_expansions(r#"echo $'\x41'"#),
            RmVariableResolution::Unchanged
        );
    }

    /// #1075 — an unquoted `$IFS`/`${IFS}` word-splits the token so the first
    /// field names the removal.
    #[test]
    fn issue_1075_ifs_word_splitting_is_denied() {
        for command in [r#"rm${IFS}-rf${IFS}"$V"/"#, r#"rm$IFS-rf$IFS"$V"/"#] {
            assert!(denied(command), "expected denial for {command}");
        }
        // Control: `$IFS` with the operand intact was already denied; keep it.
        assert!(denied(r#"rm -rf $IFS"$V"/"#));
    }

    /// #1076 — a leading tilde expands to a home directory, which can be `/`.
    #[test]
    fn issue_1076_tilde_expansion_is_denied() {
        for command in [
            r#"rm -rf ~/"#,
            r#"rm -rf ~/*"#,
            r#"rm -rf ~root/"#,
            r#"HOME=/; rm -rf ~/"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
        // Control: a tilde that is not at word start is an ordinary filename.
        assert!(!denied(r#"rm -rf ./~backup"#));
    }

    /// #1077 — brace expansion can produce a root (`{/,}` → `/`), but a benign
    /// brace group in the working directory must not be swept up.
    #[test]
    fn issue_1077_brace_expansion_to_root_is_denied() {
        for command in [r#"rm -rf {/,}"#, r#"rm -rf /{,}"#, r#"rm -rf {/,/tmp}"#] {
            assert!(denied(command), "expected denial for {command}");
        }
        // Control: a brace group with no root among its alternatives.
        assert!(!denied(r#"rm -rf {a,b}.txt"#));
    }

    /// #1078 — a sibling operand that WAS rewritten must not disable the check
    /// for a different, unproven removal elsewhere in the command.
    #[test]
    fn issue_1078_a_sibling_rewrite_does_not_disable_the_fallback() {
        for command in [
            r#"A=/tmp/safe/a; rm -rf "$A"/build; nice rm -rf "$V"/"#,
            r#"A=/tmp/safe/a; rm -rf "$A"/build && find "$V"/ -delete"#,
            "A=/tmp/safe/a; rm -rf \"$A\"/build\nionice rm -rf \"$V\"/",
            r#"A=/tmp/safe/a; rm -rf "$A"/build | timeout 5 rm -rf "$V"/"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    /// #1079 — a curated, best-effort denylist of well-known recursive-delete
    /// idioms in programs other than `rm`/`rmdir`/`find -delete`, fired only in
    /// the presence of a `$VAR/`-rooted operand (DD-057: best-effort).
    #[test]
    fn issue_1079_other_recursive_deleters_are_denied() {
        for command in [
            r#"perl -MFile::Path -e 'rmtree($ARGV[0])' "$V"/"#,
            r#"python3 -c 'import shutil,sys; shutil.rmtree(sys.argv[1])' "$V"/"#,
            r#"rsync -a --delete /var/empty/ "$V"/"#,
            r#"find "$V"/ -exec perl -e unlink {} +"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    /// #1085 — the raw-payload probe over unparseable bytes must recover a word
    /// the shell would concatenate: `r''m`, `r""m`, `"r"m`, `r\m` all name
    /// `rm`. `raw_payload_mentions_removal` is `pub(super)`, so this exercises
    /// it directly.
    #[test]
    fn issue_1085_raw_probe_recovers_concatenated_removal_words() {
        for payload in [
            "r''m",
            r#"r""m"#,
            r#""r"m"#,
            r"r\m",
            r"cd /tmp && r''m -rf x",
        ] {
            assert!(
                raw_payload_mentions_removal(payload),
                "raw probe missed {payload:?}"
            );
        }
        // A payload with no removal word must not trip it.
        assert!(!raw_payload_mentions_removal("echo hello world"));
        assert!(!raw_payload_mentions_removal("git commit -m done"));
    }

    /// #1088 — a hazardous removal wrapped in more `$( … )` nesting than the
    /// recursion cap allows must fail closed, not fall through to allow.
    #[test]
    fn issue_1088_recursion_cap_fails_closed() {
        let mut nested = String::from(r#"rm -rf "$V"/"#);
        for _ in 0..12 {
            nested = format!("$({nested})");
        }
        assert!(denied(&format!("echo {nested}")));
    }

    /// Every spelling of the catastrophic shape — `$VAR` at the start of an
    /// operand, immediately followed by `/` — must be denied when the value
    /// cannot be proven. This is the shape that expands to `rm -rf /`, and it
    /// is the one the interpreter exists to stop.
    #[test]
    fn every_spelling_of_the_catastrophic_shape_is_denied() {
        for command in [
            // Quoting and bracing.
            r#"rm -rf "$U"/"#,
            r#"rm -rf $U/"#,
            r#"rm -rf ${U}/"#,
            r#"rm -rf "${U}"/"#,
            // What follows the slash does not make it safer.
            r#"rm -rf "$U"/*"#,
            r#"rm -rf "$U"//"#,
            r#"rm -rf "$U"/."#,
            r#"rm -rf "$U"/.."#,
            // Flag spellings, including end-of-options.
            r#"rm -rf -- "$U"/"#,
            r#"rm --recursive --force "$U"/"#,
            r#"rm -r -f "$U"/"#,
            r#"rm -fr "$U"/"#,
            r#"rmdir "$U"/"#,
            // The program named by path, or with alias expansion suppressed.
            r#"/bin/rm -rf "$U"/"#,
            r#"\rm -rf "$U"/"#,
            // Transparent wrappers the denylist already knows how to unwrap.
            r#"command rm -rf "$U"/"#,
            r#"env rm -rf "$U"/"#,
            r#"sudo rm -rf "$U"/"#,
            r#"time rm -rf "$U"/"#,
            // Statement position must not matter.
            r#"rm -rf "$U"/ ; echo done"#,
            r#"true | rm -rf "$U"/"#,
            r#"rm -rf "$U"/ &"#,
            "cd /tmp\nrm -rf \"$U\"/",
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    /// The value's provenance has to be *provable*, not merely present. Each
    /// of these assigns the variable somewhere the interpreter cannot reduce
    /// to one literal, so none of them may pass.
    #[test]
    fn a_value_that_cannot_be_proven_literal_is_denied() {
        for command in [
            // Proven, but proven *dangerous*.
            r#"U=/; rm -rf "$U"/"#,
            r#"U=""; rm -rf "$U"/"#,
            r#"U=/tmp/../..; rm -rf "$U"/"#,
            // Two assignments disagree.
            r#"U=/tmp; U=/; rm -rf "$U"/"#,
            // Assigned on only one path.
            r#"if true; then U=/tmp; fi; rm -rf "$U"/"#,
            // Value produced at runtime.
            r#"U=$(echo /tmp); rm -rf "$U"/"#,
            r#"U=`echo /tmp`; rm -rf "$U"/"#,
            r#"read U; rm -rf "$U"/"#,
            r#"U=${OTHER}; rm -rf "$U"/"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    /// Parameter-expansion forms that could smuggle a root past a naive
    /// matcher. `${U:-/}` in particular *defaults to* `/`.
    #[test]
    fn parameter_expansion_forms_are_denied() {
        for command in [
            r#"rm -rf "${U:-/}"/"#,
            r#"rm -rf "${U:+/}"/"#,
            r#"rm -rf "${U#foo}"/"#,
            r#"rm -rf "${U%%bar}"/"#,
            r#"rm -rf "${!U}"/"#,
            r#"rm -rf "${U:=/}"/"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    /// Concatenation tricks: the `/` arrives from an adjacent quoted chunk
    /// rather than sitting directly after the expansion.
    #[test]
    fn a_slash_from_an_adjacent_chunk_still_counts() {
        for command in [r#"rm -rf "$U"'/'"#, r#"rm -rf "$U""/""#, r#"rm -rf "$U"\/"#] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    /// A nested shell is still a shell. The removal inside the string must be
    /// reached, not treated as opaque text.
    #[test]
    fn removals_inside_a_nested_interpreter_are_denied() {
        for command in [
            r#"eval "rm -rf $U/""#,
            r#"bash -c "rm -rf $U/""#,
            r#"sh -c 'rm -rf $U/'"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    /// A provable literal is rewritten rather than refused, so ordinary
    /// cleanup keeps working. Without this the guard would be unusable and
    /// would get switched off.
    #[test]
    fn a_provable_literal_is_rewritten_not_refused() {
        for command in [
            r#"U=/tmp/safe; rm -rf "$U"/"#,
            r#"U=/tmp/safe; rm -rf "$U"/*.txt"#,
        ] {
            assert!(!denied(command), "expected a rewrite for {command}");
            assert!(
                !rewritten(command).contains("$U"),
                "the rewrite must substitute the literal: {command}"
            );
        }
    }

    /// Shapes that expand to something harmless. Worth pinning so nobody
    /// "fixes" them into denials and adds friction for no safety gain.
    ///
    /// With `U` unset, `rm -rf "$U"` runs `rm -rf ""` (an error, deletes
    /// nothing) and `rm -rf $U` runs `rm -rf` with no operand (likewise). The
    /// catastrophe needs the trailing `/`, which is exactly what the
    /// interpreter keys on.
    #[test]
    fn shapes_without_a_trailing_slash_expand_to_nothing_and_are_allowed() {
        for command in [r#"rm -rf "$U""#, r#"rm -rf $U"#] {
            assert!(!denied(command), "expected allow for {command}");
        }
    }

    // ---- Stress corpus ------------------------------------------------
    //
    // The hazard is one idea with many spellings: a removal reaches an
    // operand that begins with an unprovable variable and is followed by `/`,
    // so the shell expands it to `/`. Enumerating spellings by hand always
    // trails the language, so the axes below are crossed instead — every
    // combination has to be denied.
    //
    // Nothing here executes. Each generated string is handed to
    // `resolve_posix_rm_variable_expansions`, which returns a verdict.

    /// Ways to spell the removal program itself.
    const RM_SPELLINGS: &[&str] = &[
        "rm -rf",
        "rm -fr",
        "rm -r -f",
        "rm -f -r",
        "rm --recursive --force",
        "rm -rf --",
        "rm -Rf",
        "rmdir",
        "/bin/rm -rf",
        "/usr/bin/rm -rf",
        r"\rm -rf",
        "sudo rm -rf",
        "env rm -rf",
        "command rm -rf",
        "time rm -rf",
        "nice rm -rf",
        "busybox rm -rf",
        "sudo -n rm -rf",
    ];

    /// Ways to spell an operand that expands to `/` when the variable is not
    /// set to a proven literal.
    const HAZARD_OPERANDS: &[&str] = &[
        r#""$V"/"#,
        r#"$V/"#,
        r#"${V}/"#,
        r#""${V}"/"#,
        r#""$V"/*"#,
        r#""$V"//"#,
        r#""$V"/."#,
        r#""$V"/.."#,
        r#""$V"/build"#,
        r#""$V"'/'"#,
        r#""$V""/""#,
        r#""${V:-/}"/"#,
        r#""${V:+/}"/"#,
        r#""${V#x}"/"#,
        r#""${V%x}"/"#,
        r#""${!V}"/"#,
        // Positional and special parameters: unset in any shell that was not
        // passed arguments, and not valid identifiers, so they need their own
        // handling in the hazard scanner.
        r#""$1"/"#,
        r#""${1}"/"#,
        r#""$@"/"#,
        r#"$1/"#,
    ];

    /// A named structure that wraps an inner command into a full command line.
    type Structure = (&'static str, fn(&str) -> String);

    /// Structures that wrap a statement. Each takes the inner command and
    /// returns the full command line. The analyzer must not lose the removal
    /// inside any of them.
    fn structures() -> Vec<Structure> {
        vec![
            ("bare", |c: &str| c.to_string()),
            ("trailing stmt", |c: &str| format!("{c} ; echo done")),
            ("leading stmt", |c: &str| format!("echo start ; {c}")),
            ("and-chain", |c: &str| format!("true && {c}")),
            ("or-chain", |c: &str| format!("false || {c}")),
            ("pipe target", |c: &str| format!("true | {c}")),
            ("background", |c: &str| format!("{c} &")),
            ("newline", |c: &str| format!("echo start\n{c}")),
            ("subshell", |c: &str| format!("({c})")),
            ("brace group", |c: &str| format!("{{ {c}; }}")),
            ("nested subshell", |c: &str| format!("( ( {c} ) )")),
            ("for body", |c: &str| format!("for i in 1 2; do {c}; done")),
            ("while body", |c: &str| format!("while true; do {c}; done")),
            ("until body", |c: &str| format!("until false; do {c}; done")),
            ("if body", |c: &str| format!("if true; then {c}; fi")),
            ("else body", |c: &str| {
                format!("if false; then :; else {c}; fi")
            }),
            ("case body", |c: &str| format!("case x in x) {c};; esac")),
            ("function body", |c: &str| format!("f() {{ {c}; }}; f")),
            ("eval", |c: &str| format!("eval \"{c}\"")),
            ("bash -c", |c: &str| format!("bash -c \"{c}\"")),
            ("sh -c", |c: &str| format!("sh -c '{c}'")),
            ("command subst", |c: &str| format!("echo $({c})")),
            ("backticks", |c: &str| format!("echo `{c}`")),
        ]
    }

    /// The stress test: every removal spelling, in every operand spelling, in
    /// every structure. A single miss here is a path to `rm -rf /`.
    #[test]
    fn stress_every_removal_spelling_in_every_structure_is_denied() {
        let mut escaped: Vec<String> = Vec::new();
        let mut checked = 0usize;
        for (structure_name, wrap) in structures() {
            for spelling in RM_SPELLINGS {
                for operand in HAZARD_OPERANDS {
                    let command = wrap(&format!("{spelling} {operand}"));
                    checked += 1;
                    if !denied(&command) {
                        escaped.push(format!("[{structure_name}] {command}"));
                    }
                }
            }
        }
        assert!(
            escaped.is_empty(),
            "{} of {checked} hazardous commands were NOT denied:\n{}",
            escaped.len(),
            escaped.join("\n")
        );
    }

    /// The same hazard reached through a program that consumes paths from its
    /// input rather than its argv.
    #[test]
    fn stress_removals_fed_by_another_program_are_denied() {
        let mut escaped: Vec<String> = Vec::new();
        for operand in HAZARD_OPERANDS {
            for command in [
                format!("echo {operand} | xargs rm -rf"),
                format!("printf %s {operand} | xargs -0 rm -rf"),
                format!("echo {operand} | xargs -I{{}} rm -rf {{}}"),
                format!("find {operand} -delete"),
                format!("find {operand} -exec rm -rf {{}} +"),
            ] {
                if !denied(&command) {
                    escaped.push(command);
                }
            }
        }
        assert!(
            escaped.is_empty(),
            "{} indirect removals were NOT denied:\n{}",
            escaped.len(),
            escaped.join("\n")
        );
    }

    /// Provenance stress: the variable is assigned, but never to something the
    /// interpreter can reduce to one safe literal.
    #[test]
    fn stress_unprovable_assignments_are_denied() {
        let mut escaped: Vec<String> = Vec::new();
        for assignment in [
            "V=/",
            r#"V="""#,
            "V=/tmp/../..",
            "V=$(echo /tmp)",
            "V=`echo /tmp`",
            "V=${OTHER}",
            "V=$OTHER",
            "V=/tmp; V=/",
            "V=/tmp; V=$OTHER",
            "read V",
            "if true; then V=/tmp; fi",
            "for V in / /tmp; do :; done",
            "V=/tmp && V=/",
            "export V",
            "V=~",
            "V=..",
            "V=.",
            "V=/../",
        ] {
            let command = format!(r#"{assignment}; rm -rf "$V"/"#);
            if !denied(&command) {
                escaped.push(command);
            }
        }
        assert!(
            escaped.is_empty(),
            "{} unprovable assignments were NOT denied:\n{}",
            escaped.len(),
            escaped.join("\n")
        );
    }

    /// Second tier of the corpus: axes that are easy to forget because they
    /// are punctuation, spacing, or a launcher nobody lists.
    #[test]
    fn stress_obscure_spellings_are_denied() {
        let mut escaped: Vec<String> = Vec::new();
        for command in [
            // Launchers that exec another program.
            r#"nohup rm -rf "$V"/"#,
            r#"timeout 5 rm -rf "$V"/"#,
            r#"setsid rm -rf "$V"/"#,
            r#"stdbuf -o0 rm -rf "$V"/"#,
            r#"ionice -c3 rm -rf "$V"/"#,
            r#"doas rm -rf "$V"/"#,
            r#"env -i rm -rf "$V"/"#,
            r#"command -p rm -rf "$V"/"#,
            r#"time -p rm -rf "$V"/"#,
            r#"xargs -a list rm -rf "$V"/"#,
            // The program name itself quoted or built at runtime.
            r#""rm" -rf "$V"/"#,
            r#"'rm' -rf "$V"/"#,
            r#"$(which rm) -rf "$V"/"#,
            r#"`which rm` -rf "$V"/"#,
            // Spacing and punctuation.
            "rm    -rf     \"$V\"/",
            "rm\t-rf\t\"$V\"/",
            r#"  rm -rf "$V"/  "#,
            r#"rm -rf "$V"/ ;"#,
            r#"rm -rf "$V"/ ;;"#,
            // A line continuation splices the operand back on.
            "rm -rf \\\n \"$V\"/",
            // Redirections attached to the removal.
            r#"rm -rf "$V"/ >/dev/null"#,
            r#"rm -rf "$V"/ >/dev/null 2>&1"#,
            r#"rm -rf "$V"/ 2>/dev/null &"#,
            // A trailing comment must not hide the operand.
            r#"rm -rf "$V"/ # cleanup"#,
            // The hazard is one of several operands.
            r#"rm -rf /tmp/ok "$V"/"#,
            r#"rm -rf "$V"/ /tmp/ok"#,
            // Other variables that are routinely unset in a fresh shell.
            r#"rm -rf "$1"/"#,
            r#"rm -rf "${1}"/"#,
            r#"rm -rf "$BUILD_DIR"/"#,
            // Structures combined rather than used one at a time.
            r#"f() { ( rm -rf "$V"/ ); }; f"#,
            r#"for i in 1; do ( rm -rf "$V"/ ); done"#,
            r#"if true; then f() { rm -rf "$V"/; }; f; fi"#,
            r#"bash -c 'for i in 1; do rm -rf "$V"/; done'"#,
            r#"eval 'f() { rm -rf "$V"/; }; f'"#,
        ] {
            if !denied(command) {
                escaped.push(command.to_string());
            }
        }
        assert!(
            escaped.is_empty(),
            "{} obscure spellings were NOT denied:\n{}",
            escaped.len(),
            escaped.join("\n")
        );
    }

    /// The other half of the contract. A guard that denies everything is not a
    /// guard, it is an outage — these must all stay allowed, and the list is
    /// deliberately full of near-misses.
    #[test]
    fn stress_benign_commands_are_not_swept_up() {
        let mut blocked: Vec<String> = Vec::new();
        for command in [
            // Proven-literal removals: the reason this guard is usable at all.
            r#"V=/tmp/safe; rm -rf "$V"/"#,
            r#"V=/tmp/safe; rm -rf "$V"/*.txt"#,
            r#"V=/tmp/safe/deep/dir; rm -rf "$V"/build"#,
            // Literal operands with no variable at all.
            "rm -rf /tmp/scratch",
            "rm -rf ./build",
            "rm -rf build/",
            // A variable that never reaches a removal.
            r#"echo "$V"/"#,
            r#"ls "$V"/"#,
            r#"cat "$V"/file"#,
            r#"mkdir -p "$V"/nested"#,
            // `rm` as a word that is not the program.
            "git rm -r --cached foo",
            "docker run --rm ubuntu",
            "echo 'rm -rf /'",
            // The removal and the variable are in unrelated pipelines.
            r#"rm -rf /tmp/x && echo "$V"/y"#,
            r#"echo "$V"/y ; rm -rf /tmp/x"#,
            // No trailing slash: expands to an empty operand, deletes nothing.
            r#"rm -rf "$V""#,
            r#"rm -rf $V"#,
            // Single quotes are not an expansion.
            r#"rm -rf '$V'/"#,
            // A benign ANSI-C literal in a non-removal command. (#1074)
            r#"echo $'\x41'"#,
            // A tilde that is not at word start is an ordinary filename. (#1076)
            r#"rm -rf ./~backup"#,
            // A brace group with no root among its alternatives. (#1077)
            r#"rm -rf {a,b}.txt"#,
            r#"rm -rf ./{build,dist}"#,
            // A proven-safe removal beside another proven-safe removal: the
            // unconditional fallback must not invent a hazard. (#1078)
            r#"A=/tmp/safe/a; rm -rf "$A"/build; B=/tmp/safe/b; rm -rf "$B"/dist"#,
        ] {
            if denied(command) {
                blocked.push(command.to_string());
            }
        }
        assert!(
            blocked.is_empty(),
            "{} benign commands were WRONGLY denied:\n{}",
            blocked.len(),
            blocked.join("\n")
        );
    }

    /// The one shape this guard knowingly over-refuses.
    ///
    /// `sudo -u rm echo "$V"/*` runs `echo` as a user named `rm`; nothing is
    /// deleted. Recognizing that requires knowing which options of which
    /// launcher take a value — `sudo -u` does, `xargs -0` does not — and the
    /// same shape with `xargs -0 rm -rf` or `find … -exec rm -rf {} +` really
    /// does run a removal. There is no sound text-only rule that separates
    /// them, so the tie is broken toward refusing.
    ///
    /// Pinned so the trade is visible and deliberate rather than an accident.
    #[test]
    fn a_username_spelled_rm_is_conservatively_refused() {
        assert!(denied(r#"sudo -u rm echo "$V"/*"#));
        // Without a `$VAR/` operand there is no hazard to weigh, so the same
        // shape stays allowed.
        assert!(!denied("sudo -u rm echo hello"));
    }

    #[test]
    fn issue_963_rewrites_every_rm_operand_but_not_later_reads() {
        let command = r#"git status; SP="C:/Users/test/.clud/tmp/session/scratchpad"; rm -f "$SP"/*.txt "$SP"/*.json "$SP"/*.md; ls "$SP""#;
        let output = rewritten(command);
        assert_eq!(
            output
                .matches("C:/Users/test/.clud/tmp/session/scratchpad")
                .count(),
            4
        );
        let rm = output
            .split_once("rm -f")
            .unwrap()
            .1
            .split(';')
            .next()
            .unwrap();
        assert!(!rm.contains("$SP"));
        assert!(output.ends_with("ls \"$SP\""));
    }

    #[test]
    fn undefined_empty_root_top_level_and_drive_root_values_are_denied() {
        for command in [
            r#"rm -rf "$SP"/*"#,
            r#"SP=""; rm -rf "$SP"/*"#,
            r#"SP=/; rm -rf "$SP"/*"#,
            r#"SP=/tmp; rm -rf "$SP"/*"#,
            r#"SP=C:/; rm -rf "$SP"/*"#,
            r#"SP=C:/Users; rm -rf "$SP"/*"#,
            r#"SP=//server/share; rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe/..; rm -rf "$SP"/*"#,
            r#"SP=C:/Users/safe/..; rm -rf "$SP"/*"#,
            r#"SP=../outside; rm -rf "$SP"/*"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn dynamic_reassigned_unset_conditional_and_late_values_are_denied() {
        for command in [
            r#"SP="$(pwd)/tmp"; rm -rf "$SP"/*"#,
            r#"SP=<(pwd); rm -rf "$SP"/*"#,
            r#"SP=>(cat); rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe; readonly SP=/tmp/other; rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe; SP+=/other; rm -rf "$SP"/*"#,
            r#"SP=~/safe/path; rm -rf "$SP"/*"#,
            r#"SP=/tmp/{safe,path}; rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe; SP=/tmp/other; rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe; unset SP; rm -rf "$SP"/*"#,
            r#"true || SP=/tmp/safe; rm -rf "$SP"/*"#,
            r#"true && SP=/tmp/safe; rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe | rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe & rm -rf "$SP"/*"#,
            r#"rm -rf "$SP"/*; SP=/tmp/safe"#,
            r#"if true; then SP=/tmp/safe; fi; rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe; eval 'rm -rf "$SP"/*'"#,
            r#"cleanup() { SP=/tmp/safe; rm -rf "$SP"/*; }; cleanup"#,
            r#"SP=/tmp/safe; rm -rf "${!SP}"/*"#,
            r#"echo "$(rm -rf "$SP"/*)""#,
            r#"SP=/tmp/safe; rm -rf "$SP/*"#,
            "if true; then SP=/tmp/safe; fi; rm\t\"$SP\"/*",
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn complete_operand_must_remain_static_and_away_from_roots() {
        for command in [
            r#"SP=/tmp/safe/path; rm -rf "$SP/../../.."/*"#,
            r#"SP=/tmp/safe/path; rm -rf "$SP/../.."/*"#,
            r#"SP=C:/Users/safe/path; rm -rf "$SP/../.."/*"#,
            r#"SP=/c/Users/safe/path; rm -rf "$SP/../.."/*"#,
            r#"SP=/tmp/safe/path; X=../../..; rm -rf "$SP/$X"/*"#,
            r#"SP=/tmp/safe/path; rm -rf "$SP/${MISSING}"/*"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
        assert!(matches!(
            resolve_posix_rm_variable_expansions(
                r#"SP=/tmp/safe/path; X=child; rm -rf "$SP/$X"/*.txt"#
            ),
            RmVariableResolution::Rewritten(_)
        ));
    }

    #[test]
    fn attached_redirections_cannot_disguise_operand_traversal() {
        for command in [
            r#"SP=/tmp/safe/path; rm -rf "$SP/../../..">/dev/null"#,
            r#"SP=/tmp/safe/path; rm -rf "$SP/../../..">>/dev/null"#,
            r#"SP=/tmp/safe/path; rm -rf "$SP/../../.."</dev/null"#,
            r#"SP=/tmp/safe/path; rm -rf "$SP/../../.."2>/dev/null"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn double_quoted_assignment_backslashes_follow_bash_semantics() {
        for command in [
            r#"SP="/tmp/safe\\ q/path"; rm -rf "$SP"/*"#,
            r#"SP="/tmp/safe\\q/path"; rm -rf "$SP"/*"#,
            r#"SP='/tmp/a\b/..'; rm -rf "$SP"/*"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
    }

    #[test]
    fn msys_drive_roots_and_top_level_directories_are_denied() {
        for value in ["/c/", "/c/Users"] {
            assert!(denied(&format!(r#"SP={value}; rm -rf "$SP"/*"#)));
        }
        assert!(matches!(
            resolve_posix_rm_variable_expansions(r#"SP=/c/Users/safe; rm -rf "$SP"/*"#),
            RmVariableResolution::Rewritten(_)
        ));
    }

    #[test]
    fn wrapped_and_backslash_escaped_removal_programs_are_resolved() {
        for command in [
            r#"SP=/tmp/safe/path; sudo rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe/path; r\m -rf "$SP"/*"#,
            r#"SP=/tmp/safe/path; command rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe/path; time rm -rf "$SP"/*"#,
        ] {
            assert!(matches!(
                resolve_posix_rm_variable_expansions(command),
                RmVariableResolution::Rewritten(_)
            ));
        }
        assert!(denied(r#"sudo rm -rf "$SP"/*"#));
        for command in [
            r#"SP=/tmp/safe/path; bash -c 'rm -rf "$SP"/*'"#,
            r#"SP=/tmp/safe/path; bash -c 'r\m -rf "$SP"/*'"#,
            r#"SP=/tmp/safe/path; sh -c 'rm -rf "$SP"/*'"#,
            r#"SP=/tmp/safe/path; env -S 'rm -rf "$SP"/*'"#,
            r#"SP=/tmp/safe/path; sudo time rm -rf "$SP"/*"#,
            r#"SP=/tmp/safe/path; command time rm -rf "$SP"/*"#,
        ] {
            assert!(denied(command), "expected denial for {command}");
        }
        // `sudo -u rm echo …` runs `echo` as a user named `rm`, but a
        // `$VAR/` operand in the same statement is now refused rather than
        // reasoned about — see
        // `a_username_spelled_rm_is_conservatively_refused`.
        assert!(denied(r#"SP=/tmp/safe/path; sudo -u rm echo "$SP"/*"#));
        assert_eq!(
            resolve_posix_rm_variable_expansions("SP=/tmp/safe/path; sudo -u rm echo hello"),
            RmVariableResolution::Unchanged
        );
    }

    #[test]
    fn single_quoted_variable_text_is_not_an_expansion() {
        assert_eq!(
            resolve_posix_rm_variable_expansions(r#"rm -rf '$SP'/*"#),
            RmVariableResolution::Unchanged
        );
    }

    #[test]
    fn multiple_operands_rewrite_all_or_deny_the_whole_command() {
        let output = rewritten(r#"A=/tmp/safe/a; B=/tmp/safe/b; rm -rf "$A"/* "$B"/*"#);
        assert!(!output.contains("$A"));
        assert!(!output.contains("$B"));
        assert!(denied(r#"A=/tmp/safe/a; rm -rf "$A"/* "$B"/*"#));
    }

    #[test]
    fn unrelated_and_chain_before_assignment_does_not_poison_literal_value() {
        assert!(matches!(
            resolve_posix_rm_variable_expansions(
                r#"git checkout main && git status; SP=/tmp/safe/path; rm -f "$SP"/*.txt"#
            ),
            RmVariableResolution::Rewritten(_)
        ));
    }

    #[test]
    fn unicode_literal_paths_are_preserved_by_the_rewrite() {
        let output = rewritten(r#"SP=/tmp/safe/資料; rm -f "$SP"/*.txt"#);
        assert_eq!(output, r#"SP=/tmp/safe/資料; rm -f "/tmp/safe/資料"/*.txt"#);
    }

    #[test]
    fn heredoc_data_is_never_interpreted_or_rewritten_as_a_command() {
        let data_only = "SP=/tmp/safe/path; cat <<'EOF'\nrm -rf \"$SP\"/*\nEOF";
        assert_eq!(
            resolve_posix_rm_variable_expansions(data_only),
            RmVariableResolution::Unchanged
        );

        let with_real_removal =
            "SP=/tmp/safe/path; cat <<'EOF'\nrm -rf \"$SP\"/*\nEOF\nrm -f \"$SP\"/*.txt";
        let output = rewritten(with_real_removal);
        assert!(output.contains("rm -rf \"$SP\"/*\nEOF"));
        assert!(output.ends_with("rm -f \"/tmp/safe/path\"/*.txt"));
    }
}
