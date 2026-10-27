//! Serde helpers for values Meta prints inconsistently.
//!
//! Error messages here name the JSON *type* they found, never the value:
//! they end up in [`crate::Change::parse_error`] and, redacted, in logs,
//! and a value from a webhook body can be personal data.

use meta_whatsapp_core::GraphApiError;
use serde::de::{DeserializeOwned, Error};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// An id newtype holding `id` as sent, through the type's `Deserialize`
/// (a string always deserializes into one).
fn as_sent<T: DeserializeOwned, E: Error>(id: String) -> Result<T, E> {
    T::deserialize(Value::String(id)).map_err(|_| E::custom("expected an id"))
}

/// `"an object"`, `"a string"`, … for error messages.
fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Ids that Meta sends as a JSON number on some fields (template ids are
/// `1689556908129832`, not `"1689556908129832"`) and as a string elsewhere.
/// The id newtypes are strings, so a number is kept as its decimal text.
/// Either is kept as sent, through the newtype's own `Deserialize`, which
/// never checks a numeric id's shape (`meta_whatsapp_core::ids`): an id of
/// a shape Meta never documented still parses.
pub(crate) mod id {
    use super::{Deserialize, DeserializeOwned, Deserializer, Error, Value, as_sent, kind};

    /// `#[serde(deserialize_with = "crate::serde_ext::id::deserialize")]`.
    pub(crate) fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: DeserializeOwned,
    {
        match Value::deserialize(deserializer)? {
            Value::String(s) => as_sent(s),
            Value::Number(n) => as_sent(n.to_string()),
            other => Err(D::Error::custom(format!(
                "expected an id (string or number), found {}",
                kind(&other)
            ))),
        }
    }
}

/// Optional variant of [`id`]; `null` and a missing key are both `None`.
pub(crate) mod id_option {
    use super::{Deserialize, DeserializeOwned, Deserializer, Error, Value, as_sent, kind};

    /// `#[serde(default, deserialize_with = "crate::serde_ext::id_option::deserialize")]`.
    pub(crate) fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: DeserializeOwned,
    {
        match Value::deserialize(deserializer)? {
            Value::Null => Ok(None),
            Value::String(s) => as_sent(s).map(Some),
            Value::Number(n) => as_sent(n.to_string()).map(Some),
            other => Err(D::Error::custom(format!(
                "expected an id (string or number), found {}",
                kind(&other)
            ))),
        }
    }
}

/// `errors[]` arrays of Graph error objects, accepting a `code` printed as
/// a numeric string (`groups/webhooks` and `groups/groups-messaging` quote
/// it; every other page prints an integer). Without this one quoted code
/// would turn the whole change, statuses and all, into `Unknown`.
/// `null` is an empty list.
pub(crate) mod graph_errors {
    use super::{Deserialize, Deserializer, Error, GraphApiError, Value, kind};

    /// `#[serde(default, deserialize_with = "crate::serde_ext::graph_errors::deserialize")]`.
    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Vec<GraphApiError>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let items = match Value::deserialize(deserializer)? {
            Value::Null => return Ok(Vec::new()),
            Value::Array(items) => items,
            other => {
                return Err(D::Error::custom(format!(
                    "expected an array of errors, found {}",
                    kind(&other)
                )));
            }
        };
        items
            .into_iter()
            .map(|mut item| {
                if let Some(code) = item.get_mut("code")
                    && let Some(n) = code.as_str().and_then(|s| s.trim().parse::<i64>().ok())
                {
                    *code = Value::from(n);
                }
                GraphApiError::deserialize(item).map_err(D::Error::custom)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_core::GraphApiError;
    use meta_whatsapp_core::ids::TemplateId;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct T {
        #[serde(deserialize_with = "super::id::deserialize")]
        a: TemplateId,
        #[serde(default, deserialize_with = "super::id_option::deserialize")]
        b: Option<TemplateId>,
    }

    #[test]
    fn numbers_and_strings_become_the_same_id() {
        let n: T = serde_json::from_str(r#"{"a":1689556908129832,"b":null}"#).unwrap();
        let s: T = serde_json::from_str(r#"{"a":"1689556908129832"}"#).unwrap();
        assert_eq!(n.a, s.a);
        assert_eq!(n.a.as_str(), "1689556908129832");
        assert!(n.b.is_none() && s.b.is_none());
        assert!(serde_json::from_str::<T>(r#"{"a":true}"#).is_err());
    }

    /// A numeric id of a shape Meta never documented still parses, as sent
    /// (roadmap L20c: only the checked constructors refuse it).
    #[test]
    fn an_id_of_an_unexpected_shape_is_kept_as_sent() {
        let t: T = serde_json::from_str(r#"{"a":"HSM-1/x","b":"<TEMPLATE_ID>"}"#).unwrap();
        assert_eq!(t.a.as_str(), "HSM-1/x");
        assert_eq!(t.b.as_ref().map(TemplateId::as_str), Some("<TEMPLATE_ID>"));
        assert!(TemplateId::new("HSM-1/x").is_err());
    }

    #[test]
    fn id_errors_name_the_type_not_the_value() {
        let e = serde_json::from_str::<T>(r#"{"a":{"name":"Jane Doe"}}"#)
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("found an object"), "{e}");
        assert!(!e.contains("Jane"), "{e}");
    }

    #[derive(Deserialize)]
    struct E {
        #[serde(default, deserialize_with = "super::graph_errors::deserialize")]
        errors: Vec<GraphApiError>,
    }

    #[test]
    fn graph_error_codes_may_be_numeric_strings() {
        let e: E = serde_json::from_str(
            r#"{"errors":[{"code":"131049","title":"t"},{"code":131050,"title":"u"}]}"#,
        )
        .unwrap();
        let codes: Vec<i64> = e.errors.iter().map(|e| e.code).collect();
        assert_eq!(codes, [131049, 131050]);
        assert!(
            serde_json::from_str::<E>(r#"{"errors":null}"#)
                .unwrap()
                .errors
                .is_empty()
        );
        assert!(serde_json::from_str::<E>("{}").unwrap().errors.is_empty());
        // A code that is not a number at all still fails, as before.
        assert!(serde_json::from_str::<E>(r#"{"errors":[{"code":"ERROR_CODE"}]}"#).is_err());
    }
}
