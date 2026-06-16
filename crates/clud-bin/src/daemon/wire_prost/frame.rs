use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde::Serialize;

#[cfg(test)]
use super::CLUD_JSON_PAYLOAD_PROTOCOL;
use super::{WireError, CLUD_PROST_PAYLOAD_PROTOCOL};

const DAEMON_FRAME_LINE_PREFIX: &str = "CLUD-FRAME/1 ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::daemon) struct WireFrame {
    pub(in crate::daemon) payload_protocol: u32,
    pub(in crate::daemon) payload: Vec<u8>,
}

#[cfg(test)]
pub(in crate::daemon::wire_prost) fn encode_legacy_json_frame<T: Serialize>(
    value: &T,
) -> Result<WireFrame, WireError> {
    Ok(WireFrame {
        payload_protocol: CLUD_JSON_PAYLOAD_PROTOCOL,
        payload: serde_json::to_vec(value)?,
    })
}

pub(in crate::daemon::wire_prost) fn prost_frame(payload: Vec<u8>) -> WireFrame {
    WireFrame {
        payload_protocol: CLUD_PROST_PAYLOAD_PROTOCOL,
        payload,
    }
}

pub(in crate::daemon::wire_prost) fn encode_json_line<T: Serialize>(
    value: &T,
) -> Result<Vec<u8>, WireError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub(in crate::daemon::wire_prost) fn encode_wire_frame_line(frame: &WireFrame) -> Vec<u8> {
    let payload = BASE64_STANDARD.encode(&frame.payload);
    format!(
        "{DAEMON_FRAME_LINE_PREFIX}{:08x} {payload}\n",
        frame.payload_protocol
    )
    .into_bytes()
}

pub(in crate::daemon::wire_prost) fn decode_wire_frame_line(
    line: &str,
) -> Result<Option<WireFrame>, WireError> {
    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
    let Some(rest) = trimmed.strip_prefix(DAEMON_FRAME_LINE_PREFIX) else {
        return Ok(None);
    };
    let Some((protocol_hex, payload_b64)) = rest.split_once(' ') else {
        return Err(WireError::InvalidFrameLine(
            "missing protocol or payload".to_string(),
        ));
    };
    if protocol_hex.len() != 8 || payload_b64.is_empty() {
        return Err(WireError::InvalidFrameLine(
            "expected 8-digit protocol and non-empty payload".to_string(),
        ));
    }
    let payload_protocol = u32::from_str_radix(protocol_hex, 16).map_err(|_| {
        WireError::InvalidFrameLine(format!("invalid payload protocol {protocol_hex:?}"))
    })?;
    let payload = BASE64_STANDARD.decode(payload_b64)?;
    Ok(Some(WireFrame {
        payload_protocol,
        payload,
    }))
}
