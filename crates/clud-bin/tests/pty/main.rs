//! PTY substrate integration tests (#1056).
//!
//! Terminal behaviour, the pump, the Shift+Enter dual reader, and the
//! Windows UTF-8 codepage contract. Each former top-level `tests/*.rs`
//! file is a module here, so the category links one test executable
//! instead of four. Test IDs are `pty::<module>::<test_name>`.

// `#[macro_use]` because `common/mod.rs` defines `require_pty_or_skip!`.
// Each of these files used to be its own crate root, where the
// `#[macro_export]`ed macro resolved unqualified; as modules of one target
// they need the macro pulled into textual scope before the modules that use
// it are declared.
#[macro_use]
#[path = "../common/mod.rs"]
mod common;

mod pty_behavior;
mod pty_pump;
mod shift_enter_dual_reader;
mod utf8_codepage;
