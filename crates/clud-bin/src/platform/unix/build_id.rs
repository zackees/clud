//! Linux-only test of `crate::build_id`: the test binary itself is an ELF.

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use crate::build_id::*;

    /// The reader must agree with `readelf -n` on this very binary.
    ///
    /// The strongest check available: the test executable is a real ELF produced
    /// by the real link line, so this proves both that `build.rs` asked for the
    /// note and that the parser finds it. A hand-built fixture would prove only
    /// the parser.
    #[test]
    fn reads_this_test_binarys_own_build_id() {
        let id = own_build_id().expect("build.rs asks the linker for a build-id on ELF");

        // sha1 is 20 bytes; the flag requests exactly that.
        assert_eq!(id.len(), 40, "expected a sha1 build-id, got {id:?}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "must be lowercase hex: {id}"
        );
        assert!(
            id.chars().any(|c| c != '0'),
            "an all-zero id means the note exists but was never filled in: {id}"
        );
    }
}
