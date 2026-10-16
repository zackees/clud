//! PowerShell walker over `tree-sitter-powershell`.

use tree_sitter::{Node, Parser};

use super::env::Env;
use super::words::{self, Collector, Ctx, Found, Parts};
use super::{
    paths, Construct, Dialect, LoopKind, Origin, PipelineSlot, Redirect, UnknownReason, Value,
};

/// A `foreach` over more literal items than this is walked once with the
/// loop variable unknown.
const MAX_UNROLL: usize = 16;

/// Commands whose script-block argument runs once per pipeline item.
const PER_ITEM_COMMANDS: &[&str] = &[
    "foreach-object",
    "%",
    "foreach",
    "where-object",
    "?",
    "where",
];

/// Automatic variables: set by PowerShell itself.
const AUTOMATIC_VARIABLES: &[&str] = &[
    "_",
    "psitem",
    "args",
    "input",
    "this",
    "matches",
    "lastexitcode",
    "?",
    "^",
    "$",
    "error",
    "true",
    "false",
    "null",
    "pwd",
    "pscommandpath",
    "psscriptroot",
    "myinvocation",
];

pub(super) fn walk_text(text: &str, env: &mut Env, ctx: Ctx, out: &mut Collector) {
    let mut parser = Parser::new();
    if parser
        .set_language(&tree_sitter_powershell::LANGUAGE.into())
        .is_err()
    {
        out.has_error = true;
        return;
    }
    let Some(tree) = parser.parse(text, None) else {
        out.has_error = true;
        return;
    };
    let root = tree.root_node();
    if root.has_error() {
        out.syntax_error(words::first_syntax_error(root, text));
    }
    let mut walker = Walker {
        src: text,
        out,
        nesting: 0,
    };
    walker.walk(root, env, ctx);
}

/// Where a command's elements accumulate.
struct Sink<'a, 't> {
    words: &'a mut Vec<Value>,
    raw: &'a mut Vec<String>,
    redirects: &'a mut Vec<Redirect>,
    blocks: &'a mut Vec<Node<'t>>,
}

struct Walker<'s, 'o> {
    src: &'s str,
    out: &'o mut Collector,
    nesting: usize,
}

impl<'s> Walker<'s, '_> {
    /// Source text of `node`, borrowed from the script rather than `self`.
    fn text(&self, node: Node<'_>) -> &'s str {
        let src: &'s str = self.src;
        node.utf8_text(src.as_bytes()).unwrap_or_default()
    }

    fn walk(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) {
        if self.nesting >= words::MAX_NESTING {
            self.out.depth_capped = true;
            return;
        }
        self.nesting += 1;
        self.walk_node(node, env, ctx);
        self.nesting -= 1;
    }

    fn walk_node(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) {
        match node.kind() {
            "pipeline" => self.pipeline(node, env, ctx),
            "pipeline_chain" => self.pipeline_chain(node, env, ctx),
            "command" => self.command(node, env, ctx),
            "assignment_expression" => self.assignment(node, env, ctx),
            "while_statement" => self.looped(node, env, ctx, LoopKind::While, "while"),
            "do_statement" => self.looped(node, env, ctx, LoopKind::Do, "do"),
            "for_statement" => self.for_statement(node, env, ctx),
            "foreach_statement" => self.foreach_statement(node, env, ctx),
            "if_statement" => self.branches(node, env, ctx, "if"),
            "switch_statement" => self.branches(node, env, ctx, "switch"),
            "try_statement" => self.branches(node, env, ctx, "try"),
            "function_statement" | "class_statement" => self.function(node, env, ctx),
            "sub_expression" => {
                let _ = self.substitution(node, env, ctx);
            }
            "ERROR" => self.walk_children(node, env, ctx.errored()),
            "comment" => {}
            _ => self.walk_children(node, env, ctx),
        }
    }

    fn walk_children(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            self.walk(child, env, ctx);
        }
    }

    /// `a | b && c || d`: chains run in order; the stages of one chain
    /// share a pipeline slot id.
    fn pipeline(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) {
        self.walk_children(node, env, ctx);
    }

    fn pipeline_chain(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) {
        let mut cursor = node.walk();
        let stages: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|child| child.kind() != "comment")
            .collect();
        if stages.len() < 2 {
            for stage in stages {
                self.walk(stage, env, ctx);
            }
            return;
        }
        let id = self.out.pipeline_id();
        let count = stages.len();
        for (stage, child) in stages.into_iter().enumerate() {
            let slot = PipelineSlot {
                id,
                stage,
                stages: count,
            };
            self.walk(child, env, ctx.in_pipeline(slot));
        }
    }

    fn command(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) {
        let mut words = Vec::new();
        let mut raw = Vec::new();
        let mut redirects = Vec::new();
        let mut blocks = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "command_name" | "path_command_name" => {
                    words.push(self.bare_token(self.text(child), env));
                    raw.push(self.text(child).to_string());
                }
                "command_name_expr" => {
                    let inner = child.named_child(0).unwrap_or(child);
                    let values = self.expression(inner, env, ctx);
                    raw.extend(std::iter::repeat_n(
                        self.text(inner).to_string(),
                        values.len(),
                    ));
                    words.extend(values);
                }
                "command_elements" => {
                    let mut sink = Sink {
                        words: &mut words,
                        raw: &mut raw,
                        redirects: &mut redirects,
                        blocks: &mut blocks,
                    };
                    self.elements(child, env, ctx, &mut sink);
                }
                _ => {}
            }
        }
        let program = words
            .first()
            .and_then(Value::as_literal)
            .map(paths::program_name);
        let found = Found {
            words: words.clone(),
            raw,
            assignments: Vec::new(),
            redirects,
            source: self.text(node),
        };
        let emitted = self.out.emit(found, env, ctx);
        let per_item = program
            .as_deref()
            .is_some_and(|name| PER_ITEM_COMMANDS.contains(&name));
        if per_item && !blocks.is_empty() {
            self.out.loop_seen(LoopKind::ForEach);
        }
        for block in blocks {
            self.out.construct(ctx, Construct::BraceGroup);
            let block_ctx = if per_item {
                ctx.in_loop(LoopKind::ForEach)
            } else {
                ctx
            };
            let mut inner = env.clone();
            self.walk(block, &mut inner, block_ctx.with_origin(ctx.origin));
        }
        if !emitted {
            return;
        }
        if let Some(program) = program {
            if !words::apply_directory_builtin(Dialect::PowerShell, &program, &words[1..], env) {
                words::walk_nested(&words, env, ctx, self.out);
            }
        }
    }

    fn elements<'t>(&mut self, node: Node<'t>, env: &mut Env, ctx: Ctx, sink: &mut Sink<'_, 't>) {
        let mut cursor = node.walk();
        let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
        for child in children {
            let text = self.text(child).to_string();
            let values = match child.kind() {
                "command_argument_sep" | "comment" => continue,
                "redirection" | "redirections" => {
                    self.redirections(child, env, sink.redirects);
                    continue;
                }
                "generic_token" => vec![self.bare_token(&text, env)],
                "command_parameter" => vec![Value::literal(text.clone())],
                _ => match script_block(child) {
                    Some(block) => {
                        sink.blocks.push(block);
                        vec![Value::unknown(text.clone(), UnknownReason::Unsupported)]
                    }
                    None => self.expression(child, env, ctx),
                },
            };
            sink.raw.extend(std::iter::repeat_n(text, values.len()));
            sink.words.extend(values);
        }
    }

    /// `> out`, `2>&1`, `*> log`: operators and targets under `node`.
    fn redirections(&self, node: Node<'_>, env: &Env, out: &mut Vec<Redirect>) {
        if node.kind() == "redirections" {
            let mut cursor = node.walk();
            let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
            for child in children {
                self.redirections(child, env, out);
            }
            return;
        }
        let mut operator = String::new();
        let mut target = None;
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            match child.kind() {
                "file_redirection_operator" | "merging_redirection_operator" => {
                    operator.push_str(self.text(child).trim());
                }
                _ => target = Some(self.bare_token(self.text(child).trim(), env)),
            }
        }
        // A merge (`2>&1`) names its target inside the operator.
        let target = target.unwrap_or_else(|| {
            Value::literal(operator.rsplit('&').next().unwrap_or_default().to_string())
        });
        out.push(Redirect { operator, target });
    }

    /// An expression used as an argument or assigned value. Arrays expand
    /// to one value per element.
    fn expression(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) -> Vec<Value> {
        if self.nesting >= words::MAX_NESTING {
            self.out.depth_capped = true;
            return vec![Value::unknown(self.text(node), UnknownReason::Unsupported)];
        }
        self.nesting += 1;
        let values = self.expression_node(node, env, ctx);
        self.nesting -= 1;
        values
    }

    fn expression_node(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) -> Vec<Value> {
        let node = unwrap_expression(node);
        match node.kind() {
            "array_literal_expression" => {
                let mut cursor = node.walk();
                let items: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
                items
                    .into_iter()
                    .flat_map(|item| self.expression(item, env, ctx))
                    .collect()
            }
            "array_expression" => self.array_expression(node, env, ctx),
            _ => vec![self.scalar(node, env, ctx)],
        }
    }

    fn array_expression(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) -> Vec<Value> {
        let mut values = Vec::new();
        let mut cursor = node.walk();
        let lists: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for list in lists {
            let mut inner = list.walk();
            let statements: Vec<Node<'_>> = list.named_children(&mut inner).collect();
            for statement in statements {
                values.extend(self.expression(statement, env, ctx));
            }
        }
        values
    }

    fn scalar(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) -> Value {
        match node.kind() {
            "string_literal" => match node.named_child(0) {
                Some(inner) => self.scalar(inner, env, ctx),
                None => Value::literal(""),
            },
            "verbatim_string_characters" => Value::literal(verbatim(self.text(node))),
            "verbatim_here_string_characters" => Value::literal(here_string_body(self.text(node))),
            "expandable_string_literal" | "expandable_here_string_literal" => {
                self.expandable(node, env, ctx)
            }
            "variable" | "braced_variable" => variable(self.text(node), env),
            "integer_literal"
            | "decimal_integer_literal"
            | "hexadecimal_integer_literal"
            | "real_literal"
            | "generic_token"
            | "simple_name" => self.bare_token(self.text(node), env),
            "sub_expression" => self.substitution(node, env, ctx),
            "parenthesized_expression" | "pipeline" | "pipeline_chain" | "command" => {
                self.nested_value(node, env, ctx)
            }
            _ => {
                self.walk_children(node, &mut env.clone(), ctx);
                Value::unknown(self.text(node), UnknownReason::Unsupported)
            }
        }
    }

    /// A parenthesized pipeline: a literal if it is one, otherwise the
    /// commands inside run and the value is unknown.
    fn nested_value(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) -> Value {
        if let Some(value) = literal_only(node).map(|inner| self.scalar(inner, env, ctx)) {
            return value;
        }
        self.substitution(node, env, ctx)
    }

    fn substitution(&mut self, node: Node<'_>, env: &Env, ctx: Ctx) -> Value {
        self.out.construct(ctx, Construct::CommandSubstitution);
        if ctx.depth < words::MAX_DEPTH {
            let mut inner = env.clone();
            self.walk_children(node, &mut inner, ctx.nested(Origin::Substitution));
        } else {
            self.out.depth_capped = true;
        }
        Value::unknown(self.text(node), UnknownReason::CommandSubstitution)
    }

    /// `"text $x $(cmd) $env:Y"`: splice each embedded value into the text
    /// between them.
    fn expandable(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) -> Value {
        let full = self.text(node);
        let quote_len = if full.starts_with("@\"") { 2 } else { 1 };
        let start = node.start_byte() + quote_len;
        let end = node.end_byte().saturating_sub(quote_len).max(start);
        let mut parts = Parts::default();
        let mut at = start;
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            if child.start_byte() < at || child.end_byte() > end {
                continue;
            }
            parts.push_text(&unescape_backtick(&self.src[at..child.start_byte()]));
            let value = self.scalar(child, env, ctx);
            parts.push(&value);
            at = child.end_byte();
        }
        parts.push_text(&unescape_backtick(&self.src[at..end]));
        parts.finish()
    }

    /// A bare token: backtick escapes, `~`, and `$name` references inside.
    fn bare_token(&self, text: &str, env: &Env) -> Value {
        if text.starts_with('~') {
            if let Some(expanded) = paths::expand_tilde(text, env.home().as_deref()) {
                return Value::literal(expanded);
            }
        }
        if !text.contains('$') {
            return Value::literal(unescape_backtick(text));
        }
        expand_inline(text, env)
    }

    fn assignment(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        let target = children
            .first()
            .and_then(|left| find_kind(*left, "variable"))
            .map(|var| self.text(var).to_string());
        let operator = children
            .iter()
            .find(|child| child.kind() == "assignement_operator")
            .map(|op| self.text(*op).trim().to_string());
        let value_node = children.last().copied();
        let value = match value_node {
            Some(value) if children.len() > 1 => {
                let values = self.expression(value, env, ctx);
                single(values, self.text(value))
            }
            _ => Value::unknown(self.text(node), UnknownReason::Unsupported),
        };
        let Some(target) = target else {
            return;
        };
        if let Some((_, name)) = variable_name(&target) {
            self.out.assigned(&name);
        }
        let value = if operator.as_deref() == Some("=") {
            value
        } else {
            Value::unknown(target.clone(), UnknownReason::RuntimeValue)
        };
        assign(&target, value, env);
    }

    fn looped(
        &mut self,
        node: Node<'_>,
        env: &mut Env,
        ctx: Ctx,
        kind: LoopKind,
        keyword: &'static str,
    ) {
        self.out.construct(ctx, Construct::ControlFlow(keyword));
        self.out.loop_seen(kind);
        let before = env.clone();
        self.walk_children(node, env, ctx.in_loop(kind));
        let after = env.clone();
        env.join(&[before, after]);
    }

    fn for_statement(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) {
        let mut cursor = node.walk();
        let infinite = !node
            .named_children(&mut cursor)
            .any(|child| child.kind() == "for_condition");
        self.looped(node, env, ctx, LoopKind::CStyle { infinite }, "for");
    }

    fn foreach_statement(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        let Some(var_at) = children.iter().position(|child| child.kind() == "variable") else {
            self.looped(node, env, ctx, LoopKind::ForEach, "foreach");
            return;
        };
        self.out.construct(ctx, Construct::ControlFlow("foreach"));
        self.out.loop_seen(LoopKind::ForEach);
        let name = self.text(children[var_at]).to_string();
        let body = children
            .iter()
            .copied()
            .find(|child| child.kind() == "statement_block");
        let collection = children
            .iter()
            .skip(var_at + 1)
            .copied()
            .find(|child| Some(*child) != body && child.kind() != "foreach_parameter");
        let items = match collection {
            Some(collection) => self.expression(collection, env, ctx),
            None => Vec::new(),
        };
        let loop_ctx = ctx.in_loop(LoopKind::ForEach);
        let known =
            !items.is_empty() && items.len() <= MAX_UNROLL && items.iter().all(|i| !i.is_unknown());
        if known {
            for item in items {
                assign(&name, item, env);
                if let Some(body) = body {
                    self.walk(body, env, loop_ctx);
                }
            }
            return;
        }
        let before = env.clone();
        assign(
            &name,
            Value::unknown(name.clone(), UnknownReason::RuntimeValue),
            env,
        );
        if let Some(body) = body {
            self.walk(body, env, loop_ctx);
        }
        let after = env.clone();
        env.join(&[before, after]);
    }

    /// `if`/`switch`/`try`: every block is a possible path.
    fn branches(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx, keyword: &'static str) {
        self.out.construct(ctx, Construct::ControlFlow(keyword));
        let start = env.clone();
        let mut outcomes = vec![start.clone()];
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for child in children {
            let mut branch = start.clone();
            self.walk(child, &mut branch, ctx);
            outcomes.push(branch);
        }
        env.join(&outcomes);
    }

    fn function(&mut self, node: Node<'_>, env: &mut Env, ctx: Ctx) {
        self.out.construct(ctx, Construct::FunctionDefinition);
        let mut inner = env.clone();
        self.walk_children(node, &mut inner, ctx.with_origin(Origin::FunctionBody));
    }
}

/// Collapse the grammar's single-child expression chain
/// (`logical_expression` > `bitwise_expression` > ... > `unary_expression`).
fn unwrap_expression(mut node: Node<'_>) -> Node<'_> {
    loop {
        let passthrough = matches!(
            node.kind(),
            "logical_expression"
                | "bitwise_expression"
                | "comparison_expression"
                | "additive_expression"
                | "multiplicative_expression"
                | "format_expression"
                | "range_expression"
                | "unary_expression"
                | "argument_expression"
                | "logical_argument_expression"
                | "bitwise_argument_expression"
                | "comparison_argument_expression"
                | "additive_argument_expression"
                | "multiplicative_argument_expression"
                | "format_argument_expression"
                | "range_argument_expression"
                | "pipeline"
                | "pipeline_chain"
                | "left_assignment_expression"
        ) || (node.kind() == "array_literal_expression"
            && node.named_child_count() == 1);
        if !passthrough || node.named_child_count() != 1 {
            return node;
        }
        match node.named_child(0) {
            Some(child) => node = child,
            None => return node,
        }
    }
}

/// The single literal inside a parenthesized expression, if that is all
/// it holds.
fn literal_only(node: Node<'_>) -> Option<Node<'_>> {
    let mut current = node;
    if current.kind() == "parenthesized_expression" {
        current = current.named_child(0)?;
    }
    let inner = unwrap_expression(current);
    matches!(
        inner.kind(),
        "string_literal" | "integer_literal" | "real_literal" | "variable"
    )
    .then_some(inner)
}

fn script_block(node: Node<'_>) -> Option<Node<'_>> {
    let inner = unwrap_expression(node);
    (inner.kind() == "script_block_expression").then_some(inner)
}

fn find_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
    children
        .into_iter()
        .find_map(|child| find_kind(child, kind))
}

fn single(values: Vec<Value>, raw: &str) -> Value {
    match values.len() {
        1 => values
            .into_iter()
            .next()
            .unwrap_or_else(|| Value::literal("")),
        0 => Value::literal(""),
        _ => Value::unknown(raw, UnknownReason::Unsupported),
    }
}

/// `$name`, `${name}`, `$env:NAME`, `$scope:name` → (is_env, name).
fn variable_name(text: &str) -> Option<(bool, String)> {
    let body = text.strip_prefix('$')?;
    let body = body
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .unwrap_or(body);
    if let Some((scope, name)) = body.split_once(':') {
        let is_env = scope.eq_ignore_ascii_case("env");
        let plain_scope = ["global", "script", "local", "private", "using"]
            .iter()
            .any(|known| scope.eq_ignore_ascii_case(known));
        return (is_env || plain_scope).then(|| (is_env, name.to_string()));
    }
    Some((false, body.to_string()))
}

fn variable(text: &str, env: &Env) -> Value {
    let Some((is_env, name)) = variable_name(text) else {
        return Value::unknown(text, UnknownReason::Unsupported);
    };
    let key = env_key(is_env, &name);
    if !is_env && AUTOMATIC_VARIABLES.contains(&name.to_ascii_lowercase().as_str()) {
        return Value::unknown(text, UnknownReason::SpecialParameter);
    }
    match env.get(&key) {
        Some(Value::Literal(value)) => Value::literal(value.clone()),
        Some(Value::Unknown(unknown)) => Value::unknown(text, unknown.reason),
        None => Value::unknown(text, UnknownReason::UnsetVariable),
    }
}

/// PowerShell variables and environment variables live in separate
/// namespaces; environment ones share the base environment's names.
fn env_key(is_env: bool, name: &str) -> String {
    if is_env {
        name.to_string()
    } else {
        format!("ps:{name}")
    }
}

fn assign(target: &str, value: Value, env: &mut Env) {
    if let Some((is_env, name)) = variable_name(target) {
        env.set(&env_key(is_env, &name), value, Some(is_env));
    }
}

/// Expand `$name` / `${name}` / `$env:NAME` references inside a bare token.
fn expand_inline(text: &str, env: &Env) -> Value {
    let mut parts = Parts::default();
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        parts.push_text(&unescape_backtick(&rest[..at]));
        let tail = &rest[at..];
        let len = reference_len(tail);
        if len <= 1 {
            parts.push_text("$");
            rest = &tail[1..];
            continue;
        }
        parts.push(&variable(&tail[..len], env));
        rest = &tail[len..];
    }
    parts.push_text(&unescape_backtick(rest));
    parts.finish()
}

fn reference_len(tail: &str) -> usize {
    if tail.starts_with("${") {
        return tail.find('}').map_or(tail.len(), |end| end + 1);
    }
    let body = &tail[1..];
    let mut len = body
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == ':'))
        .unwrap_or(body.len());
    // A trailing `:` is a drive-style separator, not part of the name.
    while len > 0 && body.as_bytes()[len - 1] == b':' {
        len -= 1;
    }
    1 + len
}

fn verbatim(text: &str) -> String {
    let inner = text
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or(text);
    inner.replace("''", "'")
}

fn here_string_body(text: &str) -> String {
    let inner = text.trim_start_matches("@'").trim_end_matches("'@");
    inner.trim_matches(['\r', '\n']).to_string()
}

/// `` `n `` and friends; a backtick before any other character keeps it.
fn unescape_backtick(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '`' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out.replace("\"\"", "\"")
}
