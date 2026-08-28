//! Public API integration tests (#1056).
//!
//! Session lifecycle and the turn controller. Each former top-level
//! `tests/*.rs` file is a module here, so the category links one test
//! executable instead of two. Test IDs are `api::<module>::<test_name>`.

#[path = "../common/mod.rs"]
mod common;

mod api_session_lifecycle;
mod api_turn_controller;
