#[test]
fn ui() {
    // SAFETY: this integration-test binary contains only this test, so no peer
    // observes this process-wide target override.
    unsafe {
        std::env::remove_var("CARGO_BUILD_TARGET");
    }
    dylint_testing::ui_test(env!("CARGO_PKG_NAME"), "./ui");
}
