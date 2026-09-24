//! Serde helpers for values Meta prints inconsistently.

use serde::de::Error;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Ids that Meta sends as a JSON number on some fields (template ids are
/// `1689556908129832`, not `"1689556908129832"`) and as a string elsewhere.
/// The id newtypes are strings, so a number is kept as its decimal text.
pub(crate) mod id {
    use super::{Deserialize, Deserializer, Error, Value};

    /// `#[serde(deserialize_with = "crate::serde_ext::id::deserialize")]`.
    pub(crate) fn deserialize<'de, D, T>(deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
        T: From<String>,
    {
        match Value::deserialize(deserializer)? {
            Value::String(s) => Ok(T::from(s)),
            Value::Number(n) => Ok(T::from(n.to_string())),
            other => Err(D::Error::custom(format!(
                "expected an id (string or number), found {other}"
            ))),
        }
    }
}

/// Optional variant of [`id`]; `null` and a missing key are both `None`.
pub(crate) mod id_option {
    use super::{Deserialize, Deserializer, Error, Value};

    /// `#[serde(default, deserialize_with = "crate::serde_ext::id_option::deserialize")]`.
    pub(crate) fn deserialize<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
    where
        D: Deserializer<'de>,
        T: From<String>,
    {
        match Value::deserialize(deserializer)? {
            Value::Null => Ok(None),
            Value::String(s) => Ok(Some(T::from(s))),
            Value::Number(n) => Ok(Some(T::from(n.to_string()))),
            other => Err(D::Error::custom(format!(
                "expected an id (string or number), found {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use wa_core::ids::TemplateId;

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
}
