//! Strict JSON parsing for server settings.
//!
//! `serde_json` accepts input that is ambiguous for configuration: for a
//! duplicated object key it silently keeps the last value. A document edited
//! by hand and served to every install must fail on that instead, so objects
//! are rebuilt here with duplicates rejected at any depth. Nesting depth stays
//! bounded by `serde_json`'s own recursion limit, so a nesting bomb is an error
//! rather than a stack overflow.

use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

/// Largest document accepted from the network or the cache.
pub(crate) const MAX_DOCUMENT_BYTES: usize = 64 * 1024;

/// Parse one strict JSON value: UTF-8, at most [`MAX_DOCUMENT_BYTES`], an
/// optional leading byte-order mark, no duplicate keys, no trailing content.
pub(crate) fn parse_strict(bytes: &[u8]) -> Result<Value, String> {
    parse_strict_with_limit(bytes, MAX_DOCUMENT_BYTES)
}

/// Parse strict JSON with a caller-specific size cap. The OpenRouter catalog
/// reuses these duplicate-key and trailing-content checks with a larger bound.
pub(crate) fn parse_strict_with_limit(bytes: &[u8], max_bytes: usize) -> Result<Value, String> {
    if bytes.len() > max_bytes {
        return Err(format!(
            "document is {} bytes; the limit is {max_bytes}",
            bytes.len()
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|error| format!("not UTF-8: {error}"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let StrictValue(value) =
        StrictValue::deserialize(&mut deserializer).map_err(|error| error.to_string())?;
    deserializer.end().map_err(|error| error.to_string())?;
    Ok(value)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor).map(StrictValue)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        StrictValue::deserialize(deserializer).map(|StrictValue(value)| value)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(StrictValue(item)) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom(format!("duplicate key `{key}`")));
            }
            let StrictValue(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_json_with_a_byte_order_mark_and_surrounding_whitespace() {
        let value = parse_strict(
            "\u{feff}  {\"a\": [1, -2, 3.5, true, null, \"x\"], \"b\": {\"c\": {}}}\n".as_bytes(),
        )
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"a": [1, -2, 3.5, true, null, "x"], "b": {"c": {}}})
        );
    }

    #[test]
    fn rejects_duplicate_keys_at_any_depth() {
        for body in [
            r#"{"a": 1, "a": 2}"#,
            r#"{"outer": {"inner": 1, "inner": 1}}"#,
            r#"{"list": [{"k": "v", "k": "w"}]}"#,
        ] {
            let error = parse_strict(body.as_bytes()).unwrap_err();
            assert!(error.contains("duplicate key"), "{body}: {error}");
        }
    }

    #[test]
    fn rejects_input_that_is_not_exactly_one_json_value() {
        for body in [
            "",
            "   ",
            "{} trailing",
            "{}{}",
            r#"{"a": 1,}"#,
            "[1, 2,]",
            "{\"a\": 1} // comment",
            "/* comment */ {}",
            "{'a': 1}",
            r#"{"a": NaN}"#,
            r#"{"a": Infinity}"#,
            r#"{"a": "#,
            "<html>404: Not Found</html>",
        ] {
            assert!(parse_strict(body.as_bytes()).is_err(), "accepted {body:?}");
        }
    }

    #[test]
    fn rejects_invalid_utf8_and_oversized_documents() {
        assert!(parse_strict(b"{\"a\": \"\xff\xfe\"}")
            .unwrap_err()
            .contains("not UTF-8"));
        let oversized = format!("\"{}\"", "a".repeat(MAX_DOCUMENT_BYTES));
        assert!(parse_strict(oversized.as_bytes())
            .unwrap_err()
            .contains("limit"));
    }

    #[test]
    fn a_nesting_bomb_is_an_error_not_a_stack_overflow() {
        let depth = 20_000;
        let bomb = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let bomb = &bomb[..bomb.len().min(MAX_DOCUMENT_BYTES)];
        assert!(parse_strict(bomb.as_bytes()).is_err());
    }
}
