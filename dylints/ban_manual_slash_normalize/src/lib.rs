#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_ast::LitKind;
use rustc_errors::DiagDecorator;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans hand-rolled `.replace('\\', "/")` calls outside the explicit
    /// allowlist. `clud::path_norm` owns slash-normalized path rendering and
    /// path-like strings that may have come from a different OS.
    ///
    /// ### Why is this bad?
    ///
    /// `std::path::Path` parses using the host OS rules. A Windows path string
    /// such as `C:\Tools\python.exe` is one filename on Unix, which caused
    /// `clud-shim` to derive the wrong executable name in Linux/macOS CI. Keep
    /// this policy centralized so future fixes do not grow ad hoc separator
    /// rewrites.
    pub BAN_MANUAL_SLASH_NORMALIZE,
    Deny,
    "ban hand-rolled '.replace('\\\\', \"/\")' path separator rewrites"
}

const ALLOWLIST: &str = include_str!("allowlist.txt");

impl<'tcx> LateLintPass<'tcx> for BanManualSlashNormalize {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if is_allowlisted(cx, expr.span) {
            return;
        }

        if let ExprKind::MethodCall(path_segment, _receiver, args, _) = expr.kind {
            if path_segment.ident.name.as_str() != "replace" || args.len() != 2 {
                return;
            }
            if is_backslash_char_lit(&args[0]) && is_forward_slash_str_lit(&args[1]) {
                emit_lint(cx, expr.span);
            }
        }
    }
}

fn is_backslash_char_lit(expr: &Expr<'_>) -> bool {
    matches!(
        expr.kind,
        ExprKind::Lit(lit) if matches!(lit.node, LitKind::Char('\\'))
    )
}

fn is_forward_slash_str_lit(expr: &Expr<'_>) -> bool {
    if let ExprKind::Lit(lit) = expr.kind {
        if let LitKind::Str(sym, _) = lit.node {
            return sym.as_str() == "/";
        }
    }
    false
}

fn emit_lint(cx: &LateContext<'_>, span: rustc_span::Span) {
    cx.opt_span_lint(
        BAN_MANUAL_SLASH_NORMALIZE,
        Some(span),
        DiagDecorator(|diag| {
            diag.primary_message(
                "use clud::path_norm helpers instead of hand-rolled '.replace('\\\\', \"/\")'",
            );
        }),
    );
}

fn is_allowlisted(cx: &LateContext<'_>, span: rustc_span::Span) -> bool {
    let filename = match cx.sess().source_map().span_to_filename(span) {
        FileName::Real(real_filename) => real_filename
            .local_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                real_filename
                    .path(RemapPathScopeComponents::DIAGNOSTICS)
                    .to_string_lossy()
                    .into_owned()
            }),
        filename => filename
            .display(RemapPathScopeComponents::DIAGNOSTICS)
            .to_string(),
    };
    let normalized = normalize_slashes(&filename);
    ALLOWLIST
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .any(|allowed| normalized.ends_with(allowed))
}

fn normalize_slashes(path: &str) -> String {
    path.chars()
        .map(|ch| if ch == '\\' { '/' } else { ch })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_matches_tail() {
        assert!(ALLOWLIST.contains("crates/clud-bin/src/path_norm.rs"));
    }

    #[test]
    fn normalize_slashes_handles_windows_paths() {
        assert_eq!(
            normalize_slashes(r"C:\Users\niteris\dev\clud"),
            "C:/Users/niteris/dev/clud"
        );
    }
}
