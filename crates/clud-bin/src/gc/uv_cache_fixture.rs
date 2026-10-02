//! Test-only uv cache layout (#1711), copied from a live uv 0.12
//! `~/.clud/cache/uv`:
//!
//! ```text
//! wheels-v6/pypi/<pkg>/<key>        -> ../../../archive-v0/<id>   (symlink)
//! wheels-v6/pypi/<pkg>/<key>.http   the pointer uv's wheel index reads
//! wheels-v6/pypi/<pkg>/<key>.lock   uv's per-entry advisory lock
//! archive-v0/<id>/                  the unpacked wheel
//! ```

use std::fs;
use std::path::{Path, PathBuf};

/// One wheel entry as laid out by [`wheel`].
pub(crate) struct Entry {
    pub link: PathBuf,
    pub pointer: PathBuf,
    pub lock: PathBuf,
    pub archive: PathBuf,
    pub dist_info: PathBuf,
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// A healthy registry wheel: package files plus a `.dist-info` whose `RECORD`
/// lists every file, pointed at by an `.http` pointer.
pub(crate) fn wheel(root: &Path, pkg: &str, version: &str, id: &str) -> Entry {
    wheel_with_pointer(root, pkg, version, id, "http")
}

/// [`wheel`], with the pointer file's extension chosen (`http` for an index
/// or URL wheel, `rev` for a local one).
pub(crate) fn wheel_with_pointer(
    root: &Path,
    pkg: &str,
    version: &str,
    id: &str,
    pointer_ext: &str,
) -> Entry {
    let key = format!("{version}-py3-none-any");
    let archive = root.join("archive-v0").join(id);
    let dist_info_name = format!("{pkg}-{version}.dist-info");
    let files = [
        format!("{pkg}/__init__.py"),
        format!("{pkg}/main.py"),
        format!("{dist_info_name}/METADATA"),
        format!("{dist_info_name}/WHEEL"),
    ];
    let mut record = String::new();
    for file in &files {
        write(&archive.join(file), "x");
        record.push_str(&format!("{file},sha256=abc,1\n"));
    }
    record.push_str(&format!("{dist_info_name}/RECORD,,\n"));
    write(&archive.join(&dist_info_name).join("RECORD"), &record);

    let dir = root.join("wheels-v6").join("pypi").join(pkg);
    fs::create_dir_all(&dir).unwrap();
    let link = dir.join(&key);
    std::os::unix::fs::symlink(Path::new("../../../archive-v0").join(id), &link).unwrap();
    let pointer = dir.join(format!("{key}.{pointer_ext}"));
    write(&pointer, &format!("cache-policy archive={id}"));
    let lock = dir.join(format!("{key}.lock"));
    write(&lock, "");
    Entry {
        link,
        pointer,
        lock,
        dist_info: archive.join(dist_info_name),
        archive,
    }
}

/// True when the entry's pointer symlink still exists (dangling or not).
pub(crate) fn link_exists(entry: &Entry) -> bool {
    fs::symlink_metadata(&entry.link).is_ok()
}
