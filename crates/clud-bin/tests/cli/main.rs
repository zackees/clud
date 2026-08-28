//! CLI surface integration tests (#1056).
//!
//! Provider selection and the shell-completion guard. Each former
//! top-level `tests/*.rs` file is a module here, so the category links
//! one test executable instead of two. Test IDs are
//! `cli::<module>::<test_name>`.

#[path = "../common/exe.rs"]
mod exe;

mod provider_selection_cli;
mod shell_completion_guard;
