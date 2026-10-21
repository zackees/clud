#![feature(rustc_private)]

extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_span;

use rustc_errors::DiagDecorator;
use rustc_hir::def::Res;
use rustc_hir::{Expr, ExprKind};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_span::{FileName, RemapPathScopeComponents};

dylint_linting::declare_late_lint! {
    /// ### What it does
    ///
    /// Bans `dirs::home_dir`, `dirs_next::home_dir`, `home::home_dir` and
    /// `std::env::home_dir`, called or passed as a function, outside
    /// `crates/clud-bin/src/home.rs`. Use `clud::home::user_home` instead.
    ///
    /// ### Why is this bad?
    ///
    /// Each library answers "where is home" differently: `dirs::home_dir`
    /// returns the Windows Known Folder profile, while clud's settings and
    /// backend discovery honor `USERPROFILE`/`HOME` so isolated homes work.
    /// In #1829 the managed DeepSeek Harness installed under one answer and
    /// discovery searched the other, and only native CI noticed. Resolving
    /// the path rather than matching text also catches renamed imports.
    pub BAN_DIRS_HOME_DIR,
    Deny,
    "resolve the user's home with clud::home::user_home, not a library lookup"
}

const ALLOWLIST: &str = include_str!("allowlist.txt");

/// Crates whose `home_dir` is banned, matched with the item's own name.
const BANNED_CRATES: &[&str] = &["dirs", "dirs_next", "home", "std"];

impl<'tcx> LateLintPass<'tcx> for BanDirsHomeDir {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let ExprKind::Path(ref qpath) = expr.kind else {
            return;
        };
        let Res::Def(_, def_id) = cx.qpath_res(qpath, expr.hir_id) else {
            return;
        };
        if cx.tcx.item_name(def_id).as_str() != "home_dir" {
            return;
        }
        let krate = cx.tcx.crate_name(def_id.krate);
        if !BANNED_CRATES.contains(&krate.as_str()) || is_allowlisted(cx, expr.span) {
            return;
        }
        cx.opt_span_lint(
            BAN_DIRS_HOME_DIR,
            Some(expr.span),
            DiagDecorator(|diag| {
                diag.primary_message(
                    "use clud::home::user_home instead of a library home_dir lookup (#1836)",
                );
            }),
        );
    }
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
    let normalized: String = filename
        .chars()
        .map(|ch| if ch == '\\' { '/' } else { ch })
        .collect();
    ALLOWLIST
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .any(|allowed| normalized.ends_with(allowed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_names_only_the_resolver() {
        let entries: Vec<&str> = ALLOWLIST
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        assert_eq!(entries, ["crates/clud-bin/src/home.rs"]);
    }

    #[test]
    fn std_and_the_dirs_family_are_banned() {
        for krate in ["dirs", "dirs_next", "home", "std"] {
            assert!(BANNED_CRATES.contains(&krate));
        }
    }
}
