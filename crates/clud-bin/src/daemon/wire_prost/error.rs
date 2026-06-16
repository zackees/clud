use std::fmt;

use super::ENV_DAEMON_WIRE;

#[derive(Debug)]
pub(in crate::daemon) enum WireError {
    MissingPayload(&'static str),
    UnknownPayloadProtocol(u32),
    InvalidDaemonWire(String),
    InvalidFrameLine(String),
    Json(serde_json::Error),
    Base64(base64::DecodeError),
    Decode(prost::DecodeError),
    InvalidSessionKind(i32),
    U16OutOfRange { field: &'static str, value: u32 },
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPayload(kind) => write!(f, "missing {kind} prost payload"),
            Self::UnknownPayloadProtocol(protocol) => {
                write!(f, "unsupported clud payload_protocol 0x{protocol:08x}")
            }
            Self::InvalidDaemonWire(value) => {
                write!(
                    f,
                    "unsupported {ENV_DAEMON_WIRE} value {value:?}; expected json or prost"
                )
            }
            Self::InvalidFrameLine(line) => {
                write!(f, "invalid clud daemon frame line: {line}")
            }
            Self::Json(err) => write!(f, "json conversion failed: {err}"),
            Self::Base64(err) => write!(f, "frame payload base64 decode failed: {err}"),
            Self::Decode(err) => write!(f, "prost decode failed: {err}"),
            Self::InvalidSessionKind(value) => {
                write!(f, "invalid session kind enum value {value}")
            }
            Self::U16OutOfRange { field, value } => {
                write!(f, "{field} value {value} exceeds u16::MAX")
            }
        }
    }
}

impl std::error::Error for WireError {}

impl From<serde_json::Error> for WireError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<base64::DecodeError> for WireError {
    fn from(value: base64::DecodeError) -> Self {
        Self::Base64(value)
    }
}

impl From<prost::DecodeError> for WireError {
    fn from(value: prost::DecodeError) -> Self {
        Self::Decode(value)
    }
}
