//! How the SQLite and Redis conversation stores (roadmap L8) write the
//! port's enums and content, and read them back. The names are the serde
//! names of `meta-whatsapp-core`'s types, so a variant added there needs no
//! change here, and they are the Postgres adapter's too.

use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::store::{DeliveryStatus, Direction, ThreadOwner};

pub(crate) fn direction_str(d: Direction) -> &'static str {
    match d {
        Direction::Inbound => "inbound",
        Direction::Outbound => "outbound",
    }
}

pub(crate) fn parse_direction(s: &str) -> Result<Direction, StorageError> {
    match s {
        "inbound" => Ok(Direction::Inbound),
        "outbound" => Ok(Direction::Outbound),
        other => Err(StorageError::Backend(anyhow::anyhow!(
            "unknown stored direction `{other}`"
        ))),
    }
}

/// `DeliveryStatus` ↔ its serde name, so the store holds exactly what the
/// type serializes to.
pub(crate) fn status_str(status: DeliveryStatus) -> Result<String, StorageError> {
    match serde_json::to_value(status) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        Ok(other) => Err(StorageError::Backend(anyhow::anyhow!(
            "DeliveryStatus serialized to {other}, expected a string"
        ))),
        Err(source) => Err(StorageError::Corrupt {
            key: "status".to_owned(),
            source,
        }),
    }
}

/// A stored status. The error names no message id.
pub(crate) fn parse_status(s: String) -> Result<DeliveryStatus, StorageError> {
    serde_json::from_value(serde_json::Value::String(s)).map_err(|source| StorageError::Corrupt {
        key: "message status".to_owned(),
        source,
    })
}

/// `ThreadOwner` ↔ its serde name, as for [`status_str`].
pub(crate) fn owner_str(owner: ThreadOwner) -> Result<String, StorageError> {
    match serde_json::to_value(owner) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        Ok(other) => Err(StorageError::Backend(anyhow::anyhow!(
            "ThreadOwner serialized to {other}, expected a string"
        ))),
        Err(source) => Err(StorageError::Corrupt {
            key: "thread owner".to_owned(),
            source,
        }),
    }
}

/// A stored thread owner. The error names no conversation.
pub(crate) fn parse_owner(s: String) -> Result<ThreadOwner, StorageError> {
    serde_json::from_value(serde_json::Value::String(s)).map_err(|source| StorageError::Corrupt {
        key: "thread owner".to_owned(),
        source,
    })
}

/// Content as JSON text: `serde_json` writes U+0000 as the escape `\u0000`,
/// and reads it back exactly. `what` names the field in an error, never
/// the message.
pub(crate) fn json_text(value: &serde_json::Value, what: &str) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|source| StorageError::Corrupt {
        key: what.to_owned(),
        source,
    })
}

/// JSON text back, as [`json_text`] wrote it.
pub(crate) fn parse_json(text: &[u8], what: &str) -> Result<serde_json::Value, StorageError> {
    serde_json::from_slice(text).map_err(|source| StorageError::Corrupt {
        key: what.to_owned(),
        source,
    })
}

/// Stored bytes back to the text they were written from. Our writes are
/// always UTF-8; bytes that are not (an edit by hand) are
/// [`StorageError::Corrupt`], never replaced. The error names the field,
/// never the content.
pub(crate) fn utf8(field: &str, bytes: Vec<u8>) -> Result<String, StorageError> {
    String::from_utf8(bytes).map_err(|e| StorageError::Corrupt {
        key: field.to_owned(),
        source: serde::de::Error::custom(format_args!(
            "not UTF-8 (valid up to byte {})",
            e.utf8_error().valid_up_to()
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names a store keeps: the serde names, which never change.
    #[test]
    fn stored_names_are_the_serde_names() {
        for (status, name) in [
            (DeliveryStatus::Received, "received"),
            (DeliveryStatus::Accepted, "accepted"),
            (DeliveryStatus::Sent, "sent"),
            (DeliveryStatus::Delivered, "delivered"),
            (DeliveryStatus::Read, "read"),
            (DeliveryStatus::Played, "played"),
            (DeliveryStatus::Failed, "failed"),
            (DeliveryStatus::Deleted, "deleted"),
        ] {
            assert_eq!(status_str(status).unwrap(), name);
            assert_eq!(parse_status(name.to_owned()).unwrap(), status);
        }
        for (owner, name) in [
            (ThreadOwner::ThisApp, "this_app"),
            (ThreadOwner::AnotherApp, "another_app"),
            (ThreadOwner::Idle, "idle"),
        ] {
            assert_eq!(owner_str(owner).unwrap(), name);
            assert_eq!(parse_owner(name.to_owned()).unwrap(), owner);
        }
        for d in [Direction::Inbound, Direction::Outbound] {
            assert_eq!(parse_direction(direction_str(d)).unwrap(), d);
        }
        assert!(parse_direction("sideways").is_err());
        assert!(matches!(
            utf8("text", vec![0xff]),
            Err(StorageError::Corrupt { key, .. }) if key == "text"
        ));
        let nul = serde_json::json!({"a\u{0}": "b\u{0}"});
        let text = json_text(&nul, "payload").unwrap();
        assert!(!text.contains('\0'), "escaped: {text}");
        assert_eq!(parse_json(text.as_bytes(), "payload").unwrap(), nul);
    }
}
