//! Windows half of `crate::block_bad_cmd`'s bounded stdin reader. A Windows
//! pipe has no `O_NONBLOCK`, so the reader always uses its threaded path.

/// Never constructed on Windows: [`NonblockingStdin::enable`] always declines.
pub struct NonblockingStdin(());

impl NonblockingStdin {
    pub fn enable(_stdin: &std::io::StdinLock<'_>) -> Option<Self> {
        None
    }

    pub fn restore(self) {}
}
