#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_span;

use std::marker::PhantomData;

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
    /// Denies host-platform selection outside a platform boundary
    /// (`<crate>/src/platform/`, or a `platform/` directory in a test suite):
    /// host keys (`windows`, `unix`, `target_os`, ...) in `#[cfg]`,
    /// `#[cfg_attr]`, `cfg!`, `cfg_if!` and `cfg_select!`, and paths into
    /// native API crates (`libc`, `windows_sys`, ...).
    ///
    /// ### Why is this bad?
    ///
    /// Host branches scattered through business logic are invisible to a
    /// late lint run on any other host. This is the boundary soldr, zccache
    /// and fbuild enforce (#1946). The pass runs before expansion, so
    /// inactive branches count too, and it has no baseline or allowlist
    /// (#1953): the selector `platform/mod.rs` and the concrete trees under
    /// it (`windows/`, `unix/`, `linux/`, `macos/`) are the only places a
    /// host may be named.
    pub ENFORCE_PLATFORM_BOUNDARY,
    Deny,
    "confine host-platform selection and native APIs to a platform boundary"
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
const CONCRETE_TREES: &[&str] = &["windows", "unix", "linux", "macos"];

#[derive(Clone, Copy)]
enum Kind {
    AttrCfg,
    CfgMacro,
    NativeImport,
}

impl Kind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AttrCfg => "attr cfg",
            Self::CfgMacro => "cfg macro",
            Self::NativeImport => "native import",
        }
    }
}

/// The repo-relative path of a file under `crates/` or `testbins/`, taken
/// from the last such component so a checkout's own parent directories
/// cannot shift it.
fn repo_relative(filename: &str) -> Option<String> {
    let normalized = filename.replace('\\', "/");
    ["crates/", "testbins/"]
        .iter()
        .filter_map(|marker| {
            normalized
                .rfind(marker)
                .filter(|&i| i == 0 || normalized[..i].ends_with('/'))
                .map(|i| &normalized[i..])
        })
        .min_by_key(|path| path.len())
        .map(str::to_owned)
}

/// Whether `path` (repo-relative) is inside a platform boundary: the
/// selector `platform/mod.rs` or a concrete host tree under it.
///
/// A crate's boundary is `src/platform/`, directly under `src`. A test
/// suite may hold one at any depth under `tests/`, so a test category can
/// keep its host-specific cases next to its neutral ones. Any other file in
/// a `platform/` directory is a facade and must stay neutral.
fn is_boundary(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    let [_root @ ("crates" | "testbins"), _crate, area @ ("src" | "tests"), rest @ ..] = &parts[..]
    else {
        return false;
    };
    let Some(index) = rest.iter().position(|part| *part == "platform") else {
        return false;
    };
    if *area == "src" && index != 0 {
        return false;
    }
    match &rest[index + 1..] {
        ["mod.rs"] => true,
        [tree, _, ..] => CONCRETE_TREES.contains(tree),
        _ => false,
    }
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
    let Some(path) = repo_relative(&source_filename(context, span)) else {
        return;
    };
    if is_boundary(&path) {
        return;
    }
    let message = format!(
        "{} `{normalized}` is outside a platform boundary; move the host-specific code \
         into `platform/{{windows,unix}}/` behind a neutral API (#1953)",
        kind.as_str()
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
    use super::{is_boundary, repo_relative};

    #[test]
    fn selector_and_concrete_trees_are_the_boundary() {
        assert!(is_boundary("crates/clud-bin/src/platform/mod.rs"));
        assert!(is_boundary(
            "crates/clud-bin/src/platform/windows/subprocess.rs"
        ));
        assert!(is_boundary("crates/clud-bin/src/platform/unix/dnd/mod.rs"));
        assert!(is_boundary(
            "testbins/probe-dll/src/platform/windows/lib.rs"
        ));
        assert!(is_boundary(
            "crates/clud-bin/tests/integration/reaper/platform/windows/x.rs"
        ));
        assert!(is_boundary("crates/tap/tests/platform/unix/cli.rs"));
    }

    #[test]
    fn everything_else_is_outside_the_boundary() {
        // Ordinary sources, and facade files beside the selector.
        assert!(!is_boundary("crates/clud-bin/src/subprocess.rs"));
        assert!(!is_boundary("crates/clud-bin/src/platform/host.rs"));
        assert!(!is_boundary("crates/clud-bin/src/platform/windows.rs"));
        // A crate gets one boundary, directly under `src`.
        assert!(!is_boundary(
            "crates/clud-bin/src/daemon/platform/windows/x.rs"
        ));
        assert!(!is_boundary("crates/clud-bin/src/platform/other/x.rs"));
        assert!(!is_boundary("dylints/x/src/platform/windows/x.rs"));
    }

    #[test]
    fn paths_resolve_relative_to_the_repo_on_every_host() {
        assert_eq!(
            repo_relative(r"C:\w\crates\clud-bin\src\pty.rs").as_deref(),
            Some("crates/clud-bin/src/pty.rs")
        );
        assert_eq!(
            repo_relative("/w/testbins/mock-agent/src/main.rs").as_deref(),
            Some("testbins/mock-agent/src/main.rs")
        );
        assert_eq!(repo_relative("/w/dylints/x/src/lib.rs"), None);
    }
}
