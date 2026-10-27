#[derive(Debug, thiserror::Error)]
pub(in crate::daemon) enum WireError {
    #[error("missing {0} prost payload")]
    MissingPayload(&'static str),
    #[error("unsupported clud payload_protocol 0x{0:08x}")]
    UnknownPayloadProtocol(u32),
    #[error("unsupported {name} value {0:?}; expected json or prost", name = super::ENV_DAEMON_WIRE)]
    InvalidDaemonWire(String),
    #[error("invalid clud daemon frame line: {0}")]
    InvalidFrameLine(String),
    #[error("json conversion failed: {0}")]
    Json(serde_json::Error),
    #[error("frame payload base64 decode failed: {0}")]
    Base64(base64::DecodeError),
    #[error("prost decode failed: {0}")]
    Decode(prost::DecodeError),
    #[error("invalid session kind enum value {0}")]
    InvalidSessionKind(i32),
    #[error("{field} value {value} exceeds u16::MAX")]
    U16OutOfRange { field: &'static str, value: u32 },
}

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
