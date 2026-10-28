//! Windows capture remains closed until private artifact verification exists.

use std::fs::File;
use std::io;
use std::os::windows::io::OwnedHandle;
use std::path::Path;

/// Refuse before launching a child until #2246 supplies verified private files.
pub fn verify_root(_root: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows private artifacts are unavailable",
    ))
}

/// Fail closed until Windows artifact verification is implemented.
pub fn verify_file(_file: &File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows private artifacts are unavailable",
    ))
}

/// Windows capture is disabled until a cancellable private implementation exists.
pub fn read_pipe(_reader: &mut os_pipe::PipeReader, _data: &mut [u8]) -> io::Result<Option<usize>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows capture is unavailable",
    ))
}

/// Convert the writer to the file type accepted by running-process.
pub fn pipe_writer_file(writer: os_pipe::PipeWriter) -> File {
    let handle: OwnedHandle = writer.into();
    File::from(handle)
}
