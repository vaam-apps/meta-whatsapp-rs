//! Serde helpers for Meta's timestamps, which arrive as unix seconds —
//! sometimes a JSON string (`"1739321024"`, webhooks), sometimes a number.

use serde::{Deserialize, Deserializer, Serializer};
use time::OffsetDateTime;

#[derive(Deserialize)]
#[serde(untagged)]
enum Raw {
    Int(i64),
    Str(String),
}

fn parse<E: serde::de::Error>(raw: Raw) -> Result<OffsetDateTime, E> {
    let secs = match raw {
        Raw::Int(i) => i,
        Raw::Str(s) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| E::custom(format!("invalid unix timestamp `{s}`")))?,
    };
    OffsetDateTime::from_unix_timestamp(secs).map_err(E::custom)
}

/// `#[serde(with = "meta_whatsapp_core::timestamp::unix")]` for a required timestamp.
/// Serializes back as a string, the webhook form.
pub mod unix {
    use super::{Deserialize, Deserializer, OffsetDateTime, Raw, Serializer, parse};

    /// Deserialize a string or integer of unix seconds.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<OffsetDateTime, D::Error> {
        parse(Raw::deserialize(d)?)
    }

    /// Serialize as a string of unix seconds.
    pub fn serialize<S: Serializer>(t: &OffsetDateTime, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&t.unix_timestamp().to_string())
    }
}

/// `#[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]`.
pub mod unix_option {
    use super::{Deserialize, Deserializer, OffsetDateTime, Raw, Serializer, parse};

    /// Deserialize an optional string or integer of unix seconds.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<OffsetDateTime>, D::Error> {
        Option::<Raw>::deserialize(d)?.map(parse).transpose()
    }

    /// Serialize as an optional string of unix seconds.
    #[allow(clippy::ref_option)] // serde's `with` contract passes `&Option<T>`
    pub fn serialize<S: Serializer>(t: &Option<OffsetDateTime>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            Some(t) => s.serialize_str(&t.unix_timestamp().to_string()),
            None => s.serialize_none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use time::OffsetDateTime;

    #[derive(Serialize, Deserialize)]
    struct T {
        #[serde(with = "super::unix")]
        at: OffsetDateTime,
        #[serde(default, with = "super::unix_option")]
        maybe: Option<OffsetDateTime>,
    }

    #[test]
    fn accepts_strings_and_numbers() {
        let a: T = serde_json::from_str(r#"{"at":"1739321024"}"#).unwrap();
        let b: T = serde_json::from_str(r#"{"at":1739321024,"maybe":"1"}"#).unwrap();
        assert_eq!(a.at, b.at);
        assert!(a.maybe.is_none());
        assert_eq!(b.maybe.map(OffsetDateTime::unix_timestamp), Some(1));
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            r#"{"at":"1739321024","maybe":null}"#
        );
        assert!(serde_json::from_str::<T>(r#"{"at":"soon"}"#).is_err());
    }
}
