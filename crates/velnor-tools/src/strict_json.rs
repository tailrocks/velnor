//! Strict JSON parsing for typed repository contracts.
//!
//! `serde_json` normally keeps the last value when an object repeats a key.
//! That is unsafe for authority-bearing manifests: equal duplicates can hide
//! accidental generation, and conflicting duplicates can select whichever
//! parser happens to win.  This scanner walks the JSON token stream first,
//! rejecting duplicates at every object depth, then lets serde deserialize the
//! original bytes into the typed contract.

use anyhow::{Context, Result};
use serde::de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::Deserializer as _;
use std::collections::BTreeSet;
use std::fmt;

/// Deserialize a JSON document only after proving that every object has
/// unique keys.  The first pass never materializes `serde_json::Value`, so a
/// duplicate cannot be erased before it is checked.
pub(crate) fn from_str<T>(text: &str) -> Result<T>
where
    T: DeserializeOwned,
{
    let mut deserializer = serde_json::Deserializer::from_str(text);
    deserializer
        .deserialize_any(UniqueKeysVisitor)
        .context("reject duplicate JSON object keys")?;
    deserializer
        .end()
        .context("parse trailing data after JSON document")?;
    serde_json::from_str(text).context("deserialize strict JSON document")
}

struct UniqueKeysSeed;

impl<'de> DeserializeSeed<'de> for UniqueKeysSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueKeysVisitor)
    }
}

struct UniqueKeysVisitor;

impl<'de> Visitor<'de> for UniqueKeysVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value with unique object keys")
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(())
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        UniqueKeysSeed.deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element_seed(UniqueKeysSeed)?.is_some() {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key `{key}`"
                )));
            }
            map.next_value_seed(UniqueKeysSeed)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::from_str;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Control {
        version: u32,
        scope: Scope,
        rows: Vec<Row>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Scope {
        role: String,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Row {
        name: String,
    }

    #[test]
    fn rejects_equal_and_conflicting_root_duplicates() {
        for text in [
            r#"{"version":2,"version":2,"scope":{"role":"auxiliary"},"rows":[]}"#,
            r#"{"version":2,"version":3,"scope":{"role":"auxiliary"},"rows":[]}"#,
        ] {
            assert!(from_str::<Control>(text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn rejects_equal_and_conflicting_nested_duplicates() {
        for text in [
            r#"{"version":2,"scope":{"role":"auxiliary","role":"auxiliary"},"rows":[]}"#,
            r#"{"version":2,"scope":{"role":"auxiliary","role":"other"},"rows":[]}"#,
            r#"{"version":2,"scope":{"role":"auxiliary"},"rows":[{"name":"one","name":"one"}]}"#,
        ] {
            assert!(from_str::<Control>(text).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn accepts_valid_nested_control() {
        let parsed = from_str::<Control>(
            r#"{"version":2,"scope":{"role":"auxiliary"},"rows":[{"name":"one"}]}"#,
        );
        assert_eq!(
            parsed.ok(),
            Some(Control {
                version: 2,
                scope: Scope {
                    role: "auxiliary".to_owned(),
                },
                rows: vec![Row {
                    name: "one".to_owned(),
                }],
            })
        );
    }
}
