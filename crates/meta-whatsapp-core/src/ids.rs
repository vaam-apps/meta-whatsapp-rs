//! Strongly-typed identifiers.
//!
//! Every Graph object id is an opaque string (numeric ids exceed `i64` in
//! places and Meta reserves the right to change formats). Newtypes stop a
//! phone number id being passed where a WABA id was meant — the single most
//! common integration bug, since both are long digit strings.

use std::fmt;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap a raw id.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// Borrow the raw id.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Take the raw id.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }

        impl std::str::FromStr for $name {
            type Err = std::convert::Infallible;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(s.to_owned()))
            }
        }
    };
}

id_type!(
    /// Meta app id.
    AppId
);
id_type!(
    /// Business portfolio (formerly Business Manager) id.
    BusinessId
);
id_type!(
    /// WhatsApp Business Account id.
    WabaId
);
id_type!(
    /// Business phone number id (not the phone number itself).
    PhoneNumberId
);
id_type!(
    /// WhatsApp message id (`wamid.…`).
    MessageId
);
id_type!(
    /// Uploaded media id.
    MediaId
);
id_type!(
    /// Message template id.
    TemplateId
);
id_type!(
    /// Template group id (`template_group_ids` of template group analytics).
    TemplateGroupId
);
id_type!(
    /// WhatsApp Flow id.
    FlowId
);
id_type!(
    /// A file a user uploaded through a Flow's `PhotoPicker` or
    /// `DocumentPicker`, as the Flow data endpoint receives it (`media_id`,
    /// a UUID in Meta's example; the key of a per-file `error-message`).
    /// Not a [`MediaId`]: the file is fetched from its `cdn_url`, and the
    /// Flow's response message carries a separate media `id`.
    FlowMediaId
);
id_type!(
    /// WhatsApp Business Profile id: the profile node itself
    /// (`GET /{whatsapp-business-profile-id}`), not the phone number id.
    BusinessProfileId
);
id_type!(
    /// Commerce catalog id.
    CatalogId
);
id_type!(
    /// Business-scoped user id (BSUID), e.g. `US.13491208655302741918`, or a
    /// parent BSUID (`US.ENT.…`). Stable per business portfolio; survives a
    /// username change but not a phone number change.
    UserId
);
id_type!(
    /// A WhatsApp user's phone number as WhatsApp reports it (`wa_id`),
    /// digits only, no `+`. May be absent once users adopt usernames.
    WaId
);
id_type!(
    /// Group id (Groups API).
    GroupId
);
id_type!(
    /// In-App Signup entity id.
    SignupId
);
id_type!(
    /// QR code / prefilled message code.
    QrCodeId
);
id_type!(
    /// Calling API call id.
    CallId
);
id_type!(
    /// Resumable Upload API session id, as returned (`upload:…`).
    UploadSessionId
);
id_type!(
    /// Resumable Upload API file handle (`h`), used as a template
    /// `header_handle` or a `profile_picture_handle`.
    UploadHandle
);
id_type!(
    /// A Solution Partner's extended credit line id ("credit line ID",
    /// `GET /{BUSINESS_ID}/extendedcredits`).
    CreditLineId
);
id_type!(
    /// An extended credit allocation configuration id: the record of one
    /// credit line shared with one customer business
    /// (`allocation_config_id`).
    AllocationConfigId
);
id_type!(
    /// A payment credential that funds a WABA: the WABA's
    /// `primary_funding_id`, and a credit allocation's
    /// `receiving_credential.id`. The two are equal once a Solution
    /// Partner's credit line is attached to the WABA.
    FundingId
);
id_type!(
    /// A business system user id (`GET /{WABA_ID}/system_users`), e.g. the
    /// Solution Partner's system user added to each customer's WABA. Not a
    /// [`UserId`] (a WhatsApp user).
    SystemUserId
);
id_type!(
    /// A partner-led business verification submission id (`id` of a
    /// `GET /{BUSINESS_ID}/self_certified_whatsapp_business_submissions`
    /// item).
    VerificationSubmissionId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_transparent_in_json_and_readable_in_debug() {
        let id = WabaId::new("1234567890");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"1234567890\"");
        assert_eq!(format!("{id:?}"), "WabaId(1234567890)");
        let back: WabaId = serde_json::from_str("\"1234567890\"").unwrap();
        assert_eq!(back, id);
    }
}
