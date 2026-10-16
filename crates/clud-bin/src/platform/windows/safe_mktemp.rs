//! Windows half of `crate::safe_mktemp`: the creation ledger needs a
//! device/inode identity, which clud cannot read here, so `safe-mktemp`
//! refuses before creating anything.

use std::path::PathBuf;

use crate::safe_mktemp::{fail, Failure, LedgerWriter, Request};

pub(crate) fn create(
    raw: &str,
    request: &Request,
    writer: &dyn LedgerWriter,
) -> Result<PathBuf, Failure> {
    let _ = (raw, request, writer);
    Err(fail(
        2,
        "not supported on Windows: the creation ledger needs a device/inode identity, \
         which clud cannot read there, so safe-rm would refuse the directory; nothing \
         created. Use the session temp directory for scratch data instead.",
    ))
}

#[cfg(test)]
mod tests {
    use crate::safe_mktemp::create;
    use crate::safe_mktemp::tests::{base, request, FakeWriter};

    #[test]
    fn windows_refuses_before_creating_anything() {
        let (_tmp, base) = base();
        let writer = FakeWriter::default();
        let failure = create("scratch", &request(&base, Some("s1")), &writer).unwrap_err();
        assert_eq!(failure.code, 2);
        assert!(failure.message.contains("Windows"), "{failure:?}");
        assert!(!base.join("scratch").exists());
        assert!(writer.rows.borrow().is_empty());
    }
}
