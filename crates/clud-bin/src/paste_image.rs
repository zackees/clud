//! Clipboard paste helpers for PTY-mode Ctrl+V interception (#328).

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

pub fn handle_clipboard() -> io::Result<Option<Vec<u8>>> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|err| io::Error::other(format!("open clipboard: {err}")))?;
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(_) => return Ok(None),
    };
    let path = write_clipboard_png(image.width, image.height, image.bytes.as_ref())?;
    Ok(Some(path_to_pty_bytes(&path)))
}

pub fn expand_ctrl_v_bytes<F>(chunk: &[u8], mut handle_clipboard: F) -> Cow<'_, [u8]>
where
    F: FnMut() -> Option<Vec<u8>>,
{
    if !chunk.contains(&CTRL_V) {
        return Cow::Borrowed(chunk);
    }
    let mut out = Vec::with_capacity(chunk.len());
    for &byte in chunk {
        if byte == CTRL_V {
            if let Some(bytes) = handle_clipboard() {
                out.extend_from_slice(&bytes);
            } else {
                out.push(CTRL_V);
            }
        } else {
            out.push(byte);
        }
    }
    Cow::Owned(out)
}

pub fn write_clipboard_png(width: usize, height: usize, rgba: &[u8]) -> io::Result<PathBuf> {
    let width_u32 = u32::try_from(width)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "clipboard image too wide"))?;
    let height_u32 = u32::try_from(height)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "clipboard image too tall"))?;
    let image =
        image::RgbaImage::from_raw(width_u32, height_u32, rgba.to_vec()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "clipboard image byte count does not match dimensions",
            )
        })?;
    let dir = std::env::temp_dir().join("clud-clipboard");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(unique_png_name());
    image
        .save_with_format(&path, image::ImageFormat::Png)
        .map_err(|err| io::Error::other(format!("write clipboard png: {err}")))?;
    Ok(path)
}

fn path_to_pty_bytes(path: &Path) -> Vec<u8> {
    let mut bytes = path.to_string_lossy().into_owned().into_bytes();
    bytes.push(b'\n');
    bytes
}

fn unique_png_name() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("paste-{}-{nanos}.png", std::process::id())
}

const CTRL_V: u8 = 0x16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_ctrl_v_replaces_marker_when_clipboard_has_bytes() {
        let expanded = expand_ctrl_v_bytes(b"a\x16b", || Some(b"image.png\n".to_vec()));
        assert_eq!(expanded.as_ref(), b"aimage.png\nb");
    }

    #[test]
    fn expand_ctrl_v_falls_through_when_clipboard_unavailable() {
        let expanded = expand_ctrl_v_bytes(b"a\x16b", || None);
        assert_eq!(expanded.as_ref(), b"a\x16b");
    }

    #[test]
    fn expand_ctrl_v_borrows_chunks_without_marker() {
        let expanded = expand_ctrl_v_bytes(b"abc", || Some(b"unused".to_vec()));
        assert!(matches!(expanded, Cow::Borrowed(_)));
        assert_eq!(expanded.as_ref(), b"abc");
    }

    #[test]
    fn write_clipboard_png_rejects_bad_rgba_len() {
        let err = write_clipboard_png(2, 2, &[0, 0, 0]).expect_err("bad len");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
