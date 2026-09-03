//! Backend-shell selection plumbing for issue #447 (disable PowerShell on Windows).
//!
//! Currently exposes the [`git_bash_resolver`] sub-module which lazily fetches a
//! pinned, portable Git Bash bundle so callers can point a child process at a
//! known-good `bash.exe` without depending on the user's PATH containing
//! "C:\\Program Files\\Git\\usr\\bin".
//!
//! The runner-side wiring that consumes the resolver (sets
//! `CLAUDE_CODE_GIT_BASH_PATH` + `CLAUDE_CODE_USE_POWERSHELL_TOOL=0`) lands in
//! a follow-up PR; this module is the storage layer.

//! [`completion_guard`] is unrelated to shell *selection* — it keeps Git-Bash
//! completion functions out of the backend agent's shell snapshot (issue
//! #753), which is a property of the shell's login environment rather than of
//! which shell gets picked.

pub mod completion_guard;
pub mod git_bash_resolver;
pub mod nounset;
