//! Who a message goes to.
//!
//! Since the 2026 usernames rollout a user can be addressed by phone number
//! (`to`), by business-scoped user id (`recipient`), or both — in which case
//! Meta uses the phone number. Groups use `recipient_type: "group"` with the
//! group id in `to`. Source:
//! <https://developers.facebook.com/documentation/business-messaging/whatsapp/business-scoped-user-ids>.

use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::ids::{GroupId, UserId};

/// Message addressee. Serializes to the `recipient_type`/`to`/`recipient`
/// fields of a send request, so it is meant to be `#[serde(flatten)]`ed into
/// the request body.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Recipient {
    /// A phone number, E.164 with or without `+`.
    Phone(String),
    /// A business-scoped user id (or parent BSUID). Cannot receive one-tap,
    /// zero-tap or copy-code authentication templates.
    User(UserId),
    /// Both; Meta uses the phone number and ignores the BSUID.
    PhoneAndUser {
        /// Phone number, E.164.
        phone: String,
        /// Business-scoped user id.
        user: UserId,
    },
    /// A group created with the Groups API.
    Group(GroupId),
}

impl Recipient {
    /// Address by phone number.
    pub fn phone(number: impl Into<String>) -> Self {
        Self::Phone(number.into())
    }

    /// Address by business-scoped user id.
    pub fn user(id: impl Into<UserId>) -> Self {
        Self::User(id.into())
    }

    /// Address a group.
    pub fn group(id: impl Into<GroupId>) -> Self {
        Self::Group(id.into())
    }

    /// Whether this addressee can receive an authentication template with an
    /// OTP button (Meta requires a phone number for those).
    pub fn supports_otp_buttons(&self) -> bool {
        matches!(self, Self::Phone(_) | Self::PhoneAndUser { .. })
    }
}

impl From<UserId> for Recipient {
    fn from(id: UserId) -> Self {
        Self::User(id)
    }
}

impl From<GroupId> for Recipient {
    fn from(id: GroupId) -> Self {
        Self::Group(id)
    }
}

impl Serialize for Recipient {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        match self {
            Self::Phone(phone) => {
                map.serialize_entry("recipient_type", "individual")?;
                map.serialize_entry("to", phone)?;
            }
            Self::User(user) => {
                map.serialize_entry("recipient_type", "individual")?;
                map.serialize_entry("recipient", user)?;
            }
            Self::PhoneAndUser { phone, user } => {
                map.serialize_entry("recipient_type", "individual")?;
                map.serialize_entry("to", phone)?;
                map.serialize_entry("recipient", user)?;
            }
            Self::Group(group) => {
                map.serialize_entry("recipient_type", "group")?;
                map.serialize_entry("to", group)?;
            }
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(serde::Serialize)]
    struct Body<'a> {
        messaging_product: &'a str,
        #[serde(flatten)]
        to: &'a Recipient,
    }

    fn body(r: &Recipient) -> serde_json::Value {
        serde_json::to_value(Body {
            messaging_product: "whatsapp",
            to: r,
        })
        .unwrap()
    }

    #[test]
    fn serializes_each_addressing_mode() {
        assert_eq!(
            body(&Recipient::phone("+16505551234")),
            json!({"messaging_product":"whatsapp","recipient_type":"individual","to":"+16505551234"})
        );
        assert_eq!(
            body(&Recipient::user("US.13491208655302741918")),
            json!({"messaging_product":"whatsapp","recipient_type":"individual","recipient":"US.13491208655302741918"})
        );
        assert_eq!(
            body(&Recipient::PhoneAndUser {
                phone: "+16505551234".into(),
                user: "US.1".into()
            }),
            json!({"messaging_product":"whatsapp","recipient_type":"individual","to":"+16505551234","recipient":"US.1"})
        );
        assert_eq!(
            body(&Recipient::group("Y2FwaV9ncm91cDox")),
            json!({"messaging_product":"whatsapp","recipient_type":"group","to":"Y2FwaV9ncm91cDox"})
        );
    }

    #[test]
    fn otp_buttons_need_a_phone_number() {
        assert!(Recipient::phone("1").supports_otp_buttons());
        assert!(!Recipient::user("US.1").supports_otp_buttons());
        assert!(!Recipient::group("g").supports_otp_buttons());
    }
}
