//! Read this binary's own build identity (#1016 item 3).
//!
//! Debug info now ships *beside* the release as a per-triple `.dwp`, so a
//! crash report has to prove which build a sidecar belongs to. Version plus
//! triple identifies a *release*, not a build: a rebuilt or patched binary at
//! the same version would pair with the wrong DWARF and mis-symbolicate — and
//! wrong line numbers look authoritative, which is worse than none.
//!
//! ELF has the field for exactly this. `build.rs` asks the linker for it
//! (`-Wl,--build-id=sha1`); before that clud emitted none at all, and
//! `readelf -n` on a built binary showed only `.note.ABI-tag`.
//!
//! **Scope is ELF, deliberately.** The identity is only load-bearing where a
//! sidecar exists to be matched, and `ci/xbuild.py::collect_debuginfo` stages
//! a `.dwp` for ELF triples only — verified against release 2.7.9, where the
//! Windows asset 404s. Windows (PDB GUID/age) and macOS (`LC_UUID`) carry
//! their own identity and can be read the same way if a sidecar is ever
//! published for them; until then a parser for each would be untested code
//! guarding nothing.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// `p_type` of a note segment.
const PT_NOTE: u32 = 4;

/// `n_type` of the GNU build-id note.
const NT_GNU_BUILD_ID: u32 = 3;

/// Cap on a note segment we will read. Real note segments are a few hundred
/// bytes; anything vastly larger means the header is not what we think it is,
/// and a crash reporter must not allocate on a number it read from a file it
/// failed to understand.
const MAX_NOTE_BYTES: u64 = 64 * 1024;

/// This binary's build id as lowercase hex, or `None` when it has none.
///
/// Computed once and cached. That is not an optimisation: the native crash
/// handler runs in **signal context**, where opening and reading a file is
/// not async-signal-safe. Priming the cache from [`prime`] during normal
/// startup means the crash path only reads an already-resolved value.
///
/// `None` is a normal answer, not an error: non-ELF platforms have no such
/// note, and a build whose linker was not asked for one has none either.
/// Every failure mode here is `None` — a crash reporter that panicked while
/// describing a crash would be worse than one that reported less.
#[must_use]
pub fn own_build_id() -> Option<&'static str> {
    static CACHED: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            read_elf_build_id(&exe)
        })
        .as_deref()
}

/// Resolve the build id now, so the crash path never has to.
///
/// Called from crash-reporter installation, which happens during ordinary
/// startup. Without this the first caller would be whichever handler fired,
/// and on the native path that is a signal handler.
pub fn prime() {
    let _ = own_build_id();
}

/// Parse `path` as ELF and return its `NT_GNU_BUILD_ID` note as hex.
pub fn read_elf_build_id(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut ident = [0u8; 16];
    file.read_exact(&mut ident).ok()?;
    if &ident[..4] != b"\x7fELF" {
        return None;
    }
    // 1 = ELFCLASS32, 2 = ELFCLASS64. Only these two exist.
    let is_64 = match ident[4] {
        1 => false,
        2 => true,
        _ => return None,
    };
    // 1 = little, 2 = big. clud targets no big-endian ELF today, and guessing
    // would silently byte-swap every field.
    if ident[5] != 1 {
        return None;
    }

    // e_phoff, e_phentsize, e_phnum live at class-dependent offsets.
    let (phoff_at, phentsize_at, phnum_at) = if is_64 {
        (0x20u64, 0x36u64, 0x38u64)
    } else {
        (0x1c, 0x2a, 0x2c)
    };
    let phoff = if is_64 {
        read_u64(&mut file, phoff_at)?
    } else {
        u64::from(read_u32(&mut file, phoff_at)?)
    };
    let phentsize = u64::from(read_u16(&mut file, phentsize_at)?);
    let phnum = u64::from(read_u16(&mut file, phnum_at)?);
    if phentsize == 0 {
        return None;
    }

    for index in 0..phnum {
        let entry = phoff.checked_add(index.checked_mul(phentsize)?)?;
        if read_u32(&mut file, entry)? != PT_NOTE {
            continue;
        }
        // p_offset / p_filesz sit at different places in the two classes.
        let (offset, size) = if is_64 {
            (
                read_u64(&mut file, entry + 8)?,
                read_u64(&mut file, entry + 32)?,
            )
        } else {
            (
                u64::from(read_u32(&mut file, entry + 4)?),
                u64::from(read_u32(&mut file, entry + 16)?),
            )
        };
        if size == 0 || size > MAX_NOTE_BYTES {
            continue;
        }
        let mut notes = vec![0u8; usize::try_from(size).ok()?];
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.read_exact(&mut notes).ok()?;
        if let Some(id) = find_build_id_note(&notes) {
            return Some(id);
        }
    }
    None
}

/// Walk a note section for the GNU build-id.
///
/// Note layout is `n_namesz`, `n_descsz`, `n_type`, then the name and the
/// descriptor, each padded to 4 bytes. Every arithmetic step is checked: this
/// runs against whatever bytes are on disk, and a corrupt or truncated file
/// must yield `None` rather than an index panic.
fn find_build_id_note(notes: &[u8]) -> Option<String> {
    let mut pos = 0usize;
    while pos + 12 <= notes.len() {
        let namesz = u32::from_le_bytes(notes.get(pos..pos + 4)?.try_into().ok()?) as usize;
        let descsz = u32::from_le_bytes(notes.get(pos + 4..pos + 8)?.try_into().ok()?) as usize;
        let ntype = u32::from_le_bytes(notes.get(pos + 8..pos + 12)?.try_into().ok()?);

        let name_at = pos.checked_add(12)?;
        let desc_at = name_at.checked_add(align4(namesz)?)?;
        let next = desc_at.checked_add(align4(descsz)?)?;

        if ntype == NT_GNU_BUILD_ID && notes.get(name_at..name_at + namesz)? == b"GNU\0" {
            let desc = notes.get(desc_at..desc_at.checked_add(descsz)?)?;
            if desc.is_empty() {
                return None;
            }
            let mut hex = String::with_capacity(desc.len() * 2);
            for byte in desc {
                use std::fmt::Write as _;
                let _ = write!(hex, "{byte:02x}");
            }
            return Some(hex);
        }
        if next <= pos {
            // A zero-length record would spin forever on a malformed file.
            return None;
        }
        pos = next;
    }
    None
}

fn align4(value: usize) -> Option<usize> {
    value.checked_add(3).map(|v| v & !3)
}

fn read_u16(file: &mut File, at: u64) -> Option<u16> {
    let mut buf = [0u8; 2];
    file.seek(SeekFrom::Start(at)).ok()?;
    file.read_exact(&mut buf).ok()?;
    Some(u16::from_le_bytes(buf))
}

fn read_u32(file: &mut File, at: u64) -> Option<u32> {
    let mut buf = [0u8; 4];
    file.seek(SeekFrom::Start(at)).ok()?;
    file.read_exact(&mut buf).ok()?;
    Some(u32::from_le_bytes(buf))
}

fn read_u64(file: &mut File, at: u64) -> Option<u64> {
    let mut buf = [0u8; 8];
    file.seek(SeekFrom::Start(at)).ok()?;
    file.read_exact(&mut buf).ok()?;
    Some(u64::from_le_bytes(buf))
}

#[cfg(test)]
#[path = "build_id_tests.rs"]
mod tests;
