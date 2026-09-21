//! Kitty graphics protocol commands for the toast image (#1189).
//!
//! Spec: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>. The properties
//! the toast relies on:
//!
//! - A placement with `z` > 0 is alpha-blended **over** text, and erasing text
//!   never touches it, so the child's text model is untouched.
//! - Deleting the placement reveals the cells underneath as they are now — the
//!   toast needs no repaint to go away.
//! - Re-sending a placement with the same `i`/`p` moves it without flicker.
//! - `C=1` leaves the cursor where it is.
//!
//! Every command carries `q=2`. Without it the terminal answers on stdin, and
//! that answer would reach the child as if the user had typed it.

use base64::Engine as _;

/// Placement id for the toast. One toast is visible at a time.
pub const TOAST_PLACEMENT_ID: u32 = 1;
/// Placement id reserved for the persistent usage HUD.
pub const USAGE_PLACEMENT_ID: u32 = 2;

/// Above anything a TUI is likely to place, below `i32::MAX` so arithmetic by
/// other programs cannot overflow into it.
pub const TOAST_Z_INDEX: i32 = 1_000_000_000;

/// Base64 bytes per transmission chunk. The spec caps chunks at 4096.
const CHUNK: usize = 4096;

/// Image id derived from the process id so two clud sessions sharing one
/// terminal (tmux panes, split windows) do not delete each other's toast.
pub fn image_id_for_process(pid: u32) -> u32 {
    // High bits spell "CL"; the low 16 bits disambiguate sessions. Kitty ids
    // are u32 and must be non-zero, which the high bits guarantee.
    0x434c_0000 | (pid & 0xffff)
}

/// Upload a PNG as image `image_id` without displaying it.
pub fn transmit_png(image_id: u32, png: &[u8]) -> Vec<u8> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + 64);
    let chunks: Vec<&[u8]> = if bytes.is_empty() {
        vec![&[][..]]
    } else {
        bytes.chunks(CHUNK).collect()
    };
    let last = chunks.len() - 1;
    for (index, chunk) in chunks.into_iter().enumerate() {
        let more = u8::from(index != last);
        if index == 0 {
            out.extend_from_slice(
                format!("\x1b_Ga=t,f=100,t=d,i={image_id},q=2,m={more};").as_bytes(),
            );
        } else {
            out.extend_from_slice(format!("\x1b_Gq=2,m={more};").as_bytes());
        }
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// Display `image_id` at the cursor cell, scaled to `cols` x `rows` cells.
pub fn place(image_id: u32, placement_id: u32, cols: u16, rows: u16) -> Vec<u8> {
    format!(
        "\x1b_Ga=p,i={image_id},p={placement_id},c={cols},r={rows},C=1,z={TOAST_Z_INDEX},q=2\x1b\\"
    )
    .into_bytes()
}

/// Remove one placement, keeping the image data for a later re-placement.
pub fn delete_placement(image_id: u32, placement_id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=i,i={image_id},p={placement_id},q=2\x1b\\").into_bytes()
}

/// Remove every placement of `image_id` and free its data.
pub fn delete_image(image_id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(bytes: &[u8]) -> Vec<String> {
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        text.split("\x1b\\")
            .filter(|part| !part.is_empty())
            .map(|part| {
                assert!(
                    part.starts_with("\x1b_G"),
                    "not an APC graphics command: {part:?}"
                );
                part["\x1b_G".len()..].to_string()
            })
            .collect()
    }

    #[test]
    fn every_command_suppresses_terminal_replies() {
        let png = vec![7u8; 10_000];
        let mut all = transmit_png(9, &png);
        all.extend(place(9, 1, 20, 2));
        all.extend(delete_placement(9, 1));
        all.extend(delete_image(9));
        for command in commands(&all) {
            let control = command.split(';').next().unwrap();
            assert!(
                control.split(',').any(|kv| kv == "q=2"),
                "missing q=2 in {control}"
            );
        }
    }

    #[test]
    fn transmission_chunks_reassemble_to_the_original_png() {
        let png: Vec<u8> = (0..20_000u32).map(|n| (n % 251) as u8).collect();
        let cmds = commands(&transmit_png(0x434c_0001, &png));
        assert!(cmds.len() > 1, "large payload must be chunked");
        let mut payload = String::new();
        for (index, cmd) in cmds.iter().enumerate() {
            let (control, data) = cmd.split_once(';').unwrap();
            assert!(data.len() <= CHUNK);
            let last = index == cmds.len() - 1;
            assert!(control.contains(if last { "m=0" } else { "m=1" }));
            if index == 0 {
                assert!(control.contains("a=t") && control.contains("f=100"));
                assert!(control.contains("i=1129054209"));
            } else {
                assert!(!control.contains("a="), "continuations carry only m and q");
            }
            payload.push_str(data);
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        assert_eq!(decoded, png);
    }

    #[test]
    fn placement_keeps_the_cursor_and_draws_over_text() {
        let cmd = String::from_utf8(place(5, 1, 30, 2)).unwrap();
        assert!(cmd.contains("a=p"));
        assert!(cmd.contains("c=30") && cmd.contains("r=2"));
        assert!(cmd.contains("C=1"));
        assert!(cmd.contains(&format!("z={TOAST_Z_INDEX}")));
    }

    #[test]
    fn deleting_a_placement_keeps_data_and_deleting_the_image_frees_it() {
        assert!(String::from_utf8(delete_placement(5, 1))
            .unwrap()
            .contains("d=i,i=5,p=1"));
        assert!(String::from_utf8(delete_image(5))
            .unwrap()
            .contains("d=I,i=5"));
    }

    #[test]
    fn image_ids_are_nonzero_and_session_scoped() {
        assert_ne!(image_id_for_process(0), 0);
        assert_ne!(image_id_for_process(1), image_id_for_process(2));
    }
}
