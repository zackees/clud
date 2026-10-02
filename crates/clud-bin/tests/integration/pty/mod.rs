//! PTY substrate integration tests: terminal behaviour, the pump, the
//! Shift+Enter dual reader, and the Windows UTF-8 codepage contract. Test IDs
//! are `pty::<module>::<test_name>`. `ci/run_bundle.py` runs each of them in
//! its own pseudo-terminal.

mod input_corpus;
mod pty_behavior;
mod pty_pump;
mod shift_enter_dual_reader;
mod toast_pty;
mod utf8_codepage;
