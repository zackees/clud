#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_span;

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{LazyLock, Mutex};

use rustc_ast::token::TokenKind;
use rustc_ast::tokenstream::{TokenStream, TokenTree};
use rustc_ast::visit::{self, Visitor};
use rustc_ast::{Attribute, Item, ItemKind, MacCall, MetaItem, MetaItemInner, MetaItemKind, Path};
use rustc_errors::DiagDecorator;
use rustc_lint::{EarlyContext, EarlyLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents, Span};

dylint_linting::declare_pre_expansion_lint! {
    /// ### What it does
    ///
    /// Denies host-platform selection outside `crates/clud-bin/src/platform/`:
    /// host keys (`windows`, `unix`, `target_os`, ...) in `#[cfg]`,
    /// `#[cfg_attr]`, `cfg!`, `cfg_if!` and `cfg_select!`, and paths into
    /// native API crates (`libc`, `windows_sys`, ...).
    ///
    /// ### Why is this bad?
    ///
    /// Host branches scattered through business logic are invisible to a
    /// late lint run on any other host. This is the boundary soldr, zccache
    /// and fbuild enforce (#1946). The pass runs before expansion, so
    /// inactive branches count too. Existing debt is admitted only by the
    /// per-file counts in `baseline.txt`; one occurrence more than its count
    /// fails, so the baseline can only shrink.
    pub ENFORCE_PLATFORM_BOUNDARY,
    Deny,
    "confine host-platform selection and native APIs to crates/clud-bin/src/platform"
}

const HOST_KEYS: &[&str] = &[
    "windows",
    "unix",
    "target_abi",
    "target_arch",
    "target_endian",
    "target_env",
    "target_family",
    "target_feature",
    "target_os",
    "target_pointer_width",
    "target_vendor",
];
const NATIVE_ROOTS: &[&str] = &["libc", "mach2", "nix", "winapi", "windows", "windows_sys"];
const BOUNDARY: &str = "crates/clud-bin/src/platform/";
const BASELINE_TEXT: &str = include_str!("baseline.txt");
/// Prints every in-scope occurrence to stderr instead of enforcing, to
/// regenerate `baseline.txt` (see `dylints/README.md`). Never commit `true`.
const DUMP: bool = false;

#[derive(Clone, Copy)]
enum Kind {
    AttrCfg,
    CfgMacro,
    NativeImport,
}

impl Kind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AttrCfg => "attr_cfg",
            Self::CfgMacro => "cfg_macro",
            Self::NativeImport => "native_import",
        }
    }
}

type Key = (String, String, String);

fn parse_baseline(text: &str) -> HashMap<Key, u32> {
    let mut counts = HashMap::new();
    for line in text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let fields: Vec<&str> = line.split('\t').collect();
        if let [path, kind, normalized, count] = fields[..] {
            let count = count.parse().expect("baseline count must be an integer");
            let key = (
                (*path).to_owned(),
                (*kind).to_owned(),
                (*normalized).to_owned(),
            );
            counts.insert(key, count);
        }
    }
    counts
}

static BASELINE: LazyLock<HashMap<Key, u32>> = LazyLock::new(|| parse_baseline(BASELINE_TEXT));
static COUNTS: LazyLock<Mutex<HashMap<Key, u32>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static PROCESS_IDENTITY: LazyLock<String> = LazyLock::new(|| {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nonce:x}", std::process::id())
});

/// Counts one more occurrence of `key` and reports whether it is over the
/// baseline's admitted count.
fn exceeds_baseline(
    baseline: &HashMap<Key, u32>,
    counts: &mut HashMap<Key, u32>,
    key: &Key,
) -> bool {
    let seen = counts.entry(key.clone()).or_insert(0);
    *seen += 1;
    *seen > baseline.get(key).copied().unwrap_or(0)
}

/// The repo-relative path of an in-scope file, or `None` for the boundary
/// itself and anything outside `crates/` and `testbins/`.
fn scoped_path(filename: &str) -> Option<String> {
    let normalized = filename.replace('\\', "/");
    let path = ["crates/", "testbins/"]
        .iter()
        .filter_map(|marker| normalized.rfind(marker).map(|i| &normalized[i..]))
        .min_by_key(|p| std::cmp::Reverse(p.len()))?
        .to_owned();
    (!path.starts_with(BOUNDARY)).then_some(path)
}

fn source_filename(context: &EarlyContext<'_>, span: Span) -> String {
    match context.sess().source_map().span_to_filename(span) {
        FileName::Real(real) => real
            .local_path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                real.path(RemapPathScopeComponents::DIAGNOSTICS)
                    .to_string_lossy()
                    .into_owned()
            }),
        name => name
            .display(RemapPathScopeComponents::DIAGNOSTICS)
            .to_string(),
    }
}

fn record(context: &EarlyContext<'_>, span: Span, kind: Kind, normalized: &str) {
    let Some(path) = scoped_path(&source_filename(context, span)) else {
        return;
    };
    if DUMP {
        eprintln!(
            "CLUD_PLATFORM_BOUNDARY\t{}\t{path}\t{}\t{normalized}",
            *PROCESS_IDENTITY,
            kind.as_str()
        );
        return;
    }
    let key = (path, kind.as_str().to_owned(), normalized.to_owned());
    let mut counts = COUNTS.lock().expect("platform boundary counter poisoned");
    if !exceeds_baseline(&BASELINE, &mut counts, &key) {
        return;
    }
    let message = format!(
        "{} `{normalized}` is outside {BOUNDARY}; move the host-specific code behind \
         the platform boundary (#1946)",
        kind.as_str().replace('_', " ")
    );
    context.opt_span_lint(
        ENFORCE_PLATFORM_BOUNDARY,
        Some(span),
        DiagDecorator(move |diag| {
            diag.primary_message(message);
        }),
    );
}

fn check_meta(context: &EarlyContext<'_>, meta: &MetaItem) {
    match &meta.kind {
        MetaItemKind::Word | MetaItemKind::NameValue(_) => {
            if let Some(segment) = meta.path.segments.first() {
                let name = segment.ident.name.as_str();
                if HOST_KEYS.contains(&name) {
                    record(context, meta.span, Kind::AttrCfg, name);
                }
            }
        }
        MetaItemKind::List(items) => {
            for item in items {
                if let MetaItemInner::MetaItem(nested) = item {
                    check_meta(context, nested);
                }
            }
        }
    }
}

/// Records host keys anywhere inside a `cfg!`-family macro's tokens.
fn check_cfg_tokens(context: &EarlyContext<'_>, span: Span, tokens: &TokenStream) {
    for tree in tokens.iter() {
        match tree {
            TokenTree::Token(token, _) => {
                if let TokenKind::Ident(name, _) = token.kind {
                    if HOST_KEYS.contains(&name.as_str()) {
                        record(context, span, Kind::CfgMacro, name.as_str());
                    }
                }
            }
            TokenTree::Delimited(_, _, _, inner) => check_cfg_tokens(context, span, inner),
        }
    }
}

/// Scans an unexpanded macro body for nested `cfg!`-family calls and
/// `native::path` references, which the AST does not expose before expansion.
fn check_unexpanded_tokens(context: &EarlyContext<'_>, span: Span, tokens: &TokenStream) {
    let trees: Vec<&TokenTree> = tokens.iter().collect();
    for (index, tree) in trees.iter().enumerate() {
        let token = match tree {
            TokenTree::Token(token, _) => token,
            TokenTree::Delimited(_, _, _, inner) => {
                check_unexpanded_tokens(context, span, inner);
                continue;
            }
        };
        let TokenKind::Ident(name, _) = token.kind else {
            continue;
        };
        let name = name.as_str();
        let next_is = |offset: usize, kind: TokenKind| matches!(trees.get(index + offset), Some(TokenTree::Token(t, _)) if t.kind == kind);
        if is_cfg_macro(name) && next_is(1, TokenKind::Bang) {
            if let Some(TokenTree::Delimited(_, _, _, inner)) = trees.get(index + 2) {
                check_cfg_tokens(context, span, inner);
            }
        }
        let previous_is_sep = index > 0
            && matches!(trees.get(index - 1), Some(TokenTree::Token(t, _)) if t.kind == TokenKind::PathSep);
        if !previous_is_sep && NATIVE_ROOTS.contains(&name) && next_is(1, TokenKind::PathSep) {
            record(context, span, Kind::NativeImport, name);
        }
    }
}

fn is_cfg_macro(name: &str) -> bool {
    matches!(name, "cfg" | "cfg_if" | "cfg_select")
}

struct PathScanner<'a, 'ast> {
    context: &'a EarlyContext<'a>,
    marker: PhantomData<&'ast ()>,
}

impl<'ast> Visitor<'ast> for PathScanner<'_, 'ast> {
    // Nested items get their own `check_item` call; walking them here too
    // would count each of their paths twice.
    fn visit_item(&mut self, _item: &'ast Item) {}

    fn visit_path(&mut self, path: &'ast Path) {
        if let [root, _, ..] = &path.segments[..] {
            let root = root.ident.name.as_str();
            if NATIVE_ROOTS.contains(&root) {
                record(self.context, path.span, Kind::NativeImport, root);
            }
        }
        // Generic arguments hold their own paths: `Vec<libc::c_int>`.
        visit::walk_path(self, path);
    }

    fn visit_use_tree(&mut self, use_tree: &'ast rustc_ast::UseTree) {
        // `use libc::{a, b}` keeps `libc` as a one-segment prefix with the
        // rest in the nested tree, so `visit_path` alone would miss it.
        if let [root] = &use_tree.prefix.segments[..] {
            let root = root.ident.name.as_str();
            if NATIVE_ROOTS.contains(&root) {
                record(self.context, use_tree.span(), Kind::NativeImport, root);
                return;
            }
        }
        visit::walk_use_tree(self, use_tree);
    }
}

impl EarlyLintPass for EnforcePlatformBoundary {
    fn check_attribute(&mut self, context: &EarlyContext<'_>, attribute: &Attribute) {
        if let Some(meta) = attribute.meta() {
            let name = meta.path.segments.first().map(|s| s.ident.name.as_str());
            if matches!(name, Some("cfg" | "cfg_attr")) {
                check_meta(context, &meta);
            }
        }
    }

    fn check_item(&mut self, context: &EarlyContext<'_>, item: &Item) {
        if let ItemKind::ExternCrate(original, identifier) = &item.kind {
            let name = original.unwrap_or(identifier.name);
            if NATIVE_ROOTS.contains(&name.as_str()) {
                record(context, item.span, Kind::NativeImport, name.as_str());
            }
        }
        visit::walk_item(
            &mut PathScanner {
                context,
                marker: PhantomData,
            },
            item,
        );
    }

    fn check_mac(&mut self, context: &EarlyContext<'_>, mac: &MacCall) {
        let Some(first) = mac.path.segments.first() else {
            return;
        };
        if is_cfg_macro(first.ident.name.as_str()) {
            check_cfg_tokens(context, mac.span(), &mac.args.tokens);
        } else {
            check_unexpanded_tokens(context, mac.span(), &mac.args.tokens);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Key {
        (
            "crates/clud-bin/src/example.rs".to_owned(),
            "attr_cfg".to_owned(),
            "windows".to_owned(),
        )
    }

    #[test]
    fn occurrence_past_the_baseline_count_is_reported() {
        let baseline = HashMap::from([(key(), 1)]);
        let mut counts = HashMap::new();
        assert!(!exceeds_baseline(&baseline, &mut counts, &key()));
        assert!(exceeds_baseline(&baseline, &mut counts, &key()));
    }

    #[test]
    fn unlisted_occurrence_is_reported() {
        let mut counts = HashMap::new();
        assert!(exceeds_baseline(&HashMap::new(), &mut counts, &key()));
    }

    #[test]
    fn baseline_rows_parse_with_counts() {
        let parsed =
            parse_baseline("# header\ncrates/clud-bin/src/example.rs\tattr_cfg\twindows\t3\n");
        assert_eq!(parsed.get(&key()), Some(&3));
    }

    #[test]
    fn boundary_and_out_of_scope_files_are_exempt() {
        assert_eq!(scoped_path("/w/crates/clud-bin/src/platform/unix.rs"), None);
        assert_eq!(scoped_path("/w/dylints/x/src/lib.rs"), None);
        assert_eq!(
            scoped_path(r"C:\w\crates\clud-bin\src\pty.rs").as_deref(),
            Some("crates/clud-bin/src/pty.rs")
        );
        assert_eq!(
            scoped_path("/w/testbins/mock-agent/src/main.rs").as_deref(),
            Some("testbins/mock-agent/src/main.rs")
        );
    }

    #[test]
    fn committed_baseline_parses() {
        let rows = BASELINE_TEXT
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .count();
        assert_eq!(parse_baseline(BASELINE_TEXT).len(), rows);
        assert!(!DUMP, "DUMP mode must never be committed");
    }
}
