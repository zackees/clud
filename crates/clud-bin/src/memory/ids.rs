use serde::{Deserialize, Serialize};
use std::fmt;

use crate::memory::error::MemoryError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemoryId(String);

impl MemoryId {
    pub fn new_v7() -> Self {
        Self(uuid::Uuid::now_v7().to_string())
    }

    pub fn parse(s: &str) -> Result<Self, MemoryError> {
        let parsed = uuid::Uuid::parse_str(s)
            .map_err(|e| MemoryError::Migration(format!("invalid memory id `{s}`: {e}")))?;
        Ok(Self(parsed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_v7_is_uuidv7_format() {
        let id = MemoryId::new_v7();
        let parsed = uuid::Uuid::parse_str(id.as_str()).expect("uuid parse");
        assert_eq!(parsed.get_version_num(), 7, "must be v7");
    }

    #[test]
    fn parse_rejects_non_uuid() {
        let err = MemoryId::parse("not-a-uuid").unwrap_err();
        assert!(matches!(err, MemoryError::Migration(_)), "got {err:?}");
    }

    #[test]
    fn parse_accepts_uuidv7() {
        let id = MemoryId::new_v7();
        let round = MemoryId::parse(id.as_str()).expect("parse round-trip");
        assert_eq!(round, id);
    }
}
