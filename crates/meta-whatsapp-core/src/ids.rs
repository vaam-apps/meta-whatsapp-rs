//! Strongly-typed identifiers.
//!
//! Newtypes stop a phone number id being passed where a WABA id was meant
//! — the single most common integration bug, since both are long digit
//! strings. Every id is kept as a string (numeric ids exceed `i64` in
//! places).
//!
//! # Two shapes
//!
//! Each id is checked against the shape Meta documents for it, where Meta
//! documents one (roadmap L20c):
//!
//! - **Numeric ids** ([`AppId`], [`BusinessId`], [`WabaId`],
//!   [`PhoneNumberId`], [`MediaId`], [`TemplateId`]): Meta's ids for these
//!   objects are numeric strings (the WABA reference's own error example:
//!   "must be a valid numeric string"). Their constructors
//!   ([`WabaId::new`], [`FromStr`](std::str::FromStr), [`TryFrom`]) refuse
//!   anything but 1 to [`MAX_NUMERIC_ID_LEN`] ASCII digits with a
//!   [`ValidationError`] on the id's field (`waba_id`, `phone_number_id`,
//!   …), so an id holding `/`, `?` or `..` is refused before any request is
//!   built.
//! - **Opaque ids** (every other type here: BSUIDs, group ids, message
//!   ids, QR codes, …) wrap any string: Meta documents no shape that would
//!   rule out a legitimate one (a group id may be base64). The client keeps
//!   each one a single percent-encoded path segment instead, so a `/` in a
//!   group id is sent as `%2F`, never as a separator.
//!
//! **Deserializing never checks.** A numeric id read from Meta's response
//! or webhook is kept as Meta sent it (`Deserialize`, and
//! [`WabaId::new_unchecked`] for one that arrives as a plain string, wrap
//! it as is), so a new id format cannot break parsing; used in a path, it
//! is still one percent-encoded segment. Read an id from an *untrusted*
//! source (a form, a URL, your own API's request body) with the checked
//! constructor, not with `serde`.
//!
//! Swapping one kind's rule is one line: the shape a type is declared with
//! below (`numeric_id_type!` or `id_type!`).

use std::fmt;

use crate::error::ValidationError;

/// Longest numeric id accepted, in digits. Meta states no limit; 64 is the
/// service's (`meta-whatsapp-server`) cap on a Graph id in a path or a
/// body, far above the 15 to 17 digits of Meta's ids today.
pub const MAX_NUMERIC_ID_LEN: usize = 64;

/// Whether `id` has the shape of a numeric id: 1 to
/// [`MAX_NUMERIC_ID_LEN`] ASCII digits and nothing else (no sign, no
/// whitespace, no non-ASCII digit). The check every numeric id type's
/// constructor runs.
pub fn is_numeric_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_NUMERIC_ID_LEN && id.bytes().all(|b| b.is_ascii_digit())
}

/// What both kinds of id share: accessors and formatting.
macro_rules! id_common {
    ($name:ident) => {
        impl $name {
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
    };
}

/// An opaque id: any string is wrapped.
macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap a raw id. Any string is accepted: this id is opaque
            /// (see the [module docs](self)), and a path carries it as one
            /// percent-encoded segment.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }
        }

        id_common!($name);

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

/// A numeric id: digits only, checked at construction, deserialized as is.
macro_rules! numeric_id_type {
    ($(#[$meta:meta])* $name:ident, $field:literal) => {
        $(#[$meta])*
        ///
        /// A numeric id (see the [module docs](self)): [`Self::new`],
        /// [`FromStr`](std::str::FromStr) and [`TryFrom`] refuse anything
        /// but 1 to [`MAX_NUMERIC_ID_LEN`] ASCII digits, with a
        #[doc = concat!("[`ValidationError`] on `", $field, "`.")]
        /// `Deserialize` does not check: an id Meta sends is kept as sent.
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// The field a refused id is reported on.
            pub const FIELD: &'static str = $field;

            /// Check and wrap a raw id.
            ///
            /// # Errors
            ///
            /// A [`ValidationError`] on [`Self::FIELD`] unless `id` is 1 to
            /// [`MAX_NUMERIC_ID_LEN`] ASCII digits ([`is_numeric_id`]). The
            /// refused value is not repeated in the error.
            pub fn new(id: impl Into<String>) -> Result<Self, ValidationError> {
                let id = id.into();
                if is_numeric_id(&id) {
                    Ok(Self(id))
                } else {
                    Err(ValidationError::new(
                        Self::FIELD,
                        format!("expected a numeric id: 1 to {MAX_NUMERIC_ID_LEN} ASCII digits"),
                    ))
                }
            }

            /// Wrap an id **Meta sent** (a response, a webhook) or one read
            /// back from your own store, as sent, without the shape check:
            /// what `Deserialize` does, for an id that arrives as a plain
            /// string. Never for an id from an untrusted source (a form, a
            /// URL, a request body): use [`Self::new`] there. Used in a path,
            /// the id is still one percent-encoded segment.
            pub fn new_unchecked(id: impl Into<String>) -> Self {
                Self(id.into())
            }
        }

        id_common!($name);

        impl TryFrom<String> for $name {
            type Error = ValidationError;
            fn try_from(s: String) -> Result<Self, Self::Error> {
                Self::new(s)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = ValidationError;
            fn try_from(s: &str) -> Result<Self, Self::Error> {
                Self::new(s)
            }
        }

        impl std::str::FromStr for $name {
            type Err = ValidationError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }
    };
}

numeric_id_type!(
    /// Meta app id.
    AppId,
    "app_id"
);
numeric_id_type!(
    /// Business portfolio (formerly Business Manager) id.
    BusinessId,
    "business_id"
);
numeric_id_type!(
    /// WhatsApp Business Account id.
    WabaId,
    "waba_id"
);
numeric_id_type!(
    /// Business phone number id (not the phone number itself).
    PhoneNumberId,
    "phone_number_id"
);
id_type!(
    /// WhatsApp message id (`wamid.…`).
    MessageId
);
numeric_id_type!(
    /// Uploaded media id (`POST /{PHONE_NUMBER_ID}/media`'s `id`, the `id`
    /// of a received media message).
    MediaId,
    "media_id"
);
numeric_id_type!(
    /// Message template id.
    TemplateId,
    "template_id"
);
id_type!(
    /// Template group id: the group `POST /{WABA_ID}/template_groups`
    /// returns and `GET`, `POST` and `DELETE /{TEMPLATE_GROUP_ID}` address
    /// (`templates/template-groups`), and the `template_group_ids` of
    /// template group analytics.
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
    ///
    /// Anything can be wrapped, as with every opaque id here:
    /// [`Self::is_bsuid`] says whether a value has a standard BSUID's shape.
    UserId
);

/// Longest BSUID suffix, after the country code and the period
/// (`business-scoped-user-ids`: "up to 128 alphanumeric characters").
const MAX_BSUID_SUFFIX_CHARS: usize = 128;

impl UserId {
    /// Whether this is a standard BSUID, in the shape Meta documents
    /// (`business-scoped-user-ids`): a two-letter uppercase ISO 3166
    /// country code, a period, then 1 to 128 English letters or digits,
    /// e.g. `US.13491208655302741918`.
    ///
    /// A parent BSUID (`US.ENT.11815799212886844830`) is not one: its
    /// second period fails the shape, and the calls that refuse parent
    /// BSUIDs (the contact book's deletion) rely on that. Nor is a phone
    /// number, a group id or anything else a conversation is keyed by, so
    /// this picks the BSUIDs out of a mixed list of identities
    /// (`Inbox::identities` in the facade). It checks the shape only, not
    /// that Meta issued the id.
    pub fn is_bsuid(&self) -> bool {
        self.0.split_once('.').is_some_and(|(country, rest)| {
            country.len() == 2
                && country.bytes().all(|b| b.is_ascii_uppercase())
                && !rest.is_empty()
                && rest.len() <= MAX_BSUID_SUFFIX_CHARS
                && rest.bytes().all(|b| b.is_ascii_alphanumeric())
        })
    }
}

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
id_type!(
    /// A WhatsApp Business Bot id (`GET /{WABA-Bot-ID}`,
    /// `reference/whatsapp-business-bot/bot-details-api`): the node holding
    /// a bot's prompts, commands and welcome message flag. Not a
    /// [`WabaId`] nor a [`PhoneNumberId`].
    WabaBotId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_transparent_in_json_and_readable_in_debug() {
        let id = WabaId::new("1234567890").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"1234567890\"");
        assert_eq!(format!("{id:?}"), "WabaId(1234567890)");
        let back: WabaId = serde_json::from_str("\"1234567890\"").unwrap();
        assert_eq!(back, id);
    }

    /// Every numeric id type, through each checked constructor: digits pass
    /// (up to the cap), anything else is refused on the type's own field.
    /// Decisive for L20c: a numeric id holding `/` cannot be built.
    #[test]
    fn a_numeric_id_is_one_to_sixty_four_ascii_digits() {
        fn check<T>(field: &str)
        where
            T: std::str::FromStr<Err = ValidationError>
                + TryFrom<String, Error = ValidationError>
                + for<'a> TryFrom<&'a str, Error = ValidationError>
                + AsRef<str>
                + fmt::Debug,
        {
            let longest = "9".repeat(MAX_NUMERIC_ID_LEN);
            for good in ["1", "0", "0123", "106540352242922", longest.as_str()] {
                assert_eq!(good.parse::<T>().unwrap().as_ref(), good);
                assert_eq!(T::try_from(good).unwrap().as_ref(), good);
                assert_eq!(T::try_from(good.to_owned()).unwrap().as_ref(), good);
            }
            let too_long = "9".repeat(MAX_NUMERIC_ID_LEN + 1);
            for bad in [
                "",
                "123/messages",
                "1/../../OTHER",
                "%2F",
                "123?fields=x",
                "123#x",
                "..",
                "-1",
                "+1",
                " 1",
                "1 ",
                "1.0",
                "12a",
                "WABA_ID",
                "１２", // non-ASCII digits
                too_long.as_str(),
            ] {
                for refused in [
                    bad.parse::<T>().unwrap_err(),
                    T::try_from(bad).unwrap_err(),
                    T::try_from(bad.to_owned()).unwrap_err(),
                ] {
                    assert_eq!(refused.field, field, "{bad:?}");
                    // The refused value is not echoed back.
                    assert_eq!(
                        refused.reason, "expected a numeric id: 1 to 64 ASCII digits",
                        "{bad:?}"
                    );
                }
            }
        }
        check::<AppId>("app_id");
        check::<BusinessId>("business_id");
        check::<WabaId>("waba_id");
        check::<PhoneNumberId>("phone_number_id");
        check::<MediaId>("media_id");
        check::<TemplateId>("template_id");
        assert_eq!(WabaId::new("1/2").unwrap_err().field, WabaId::FIELD);
        assert!(is_numeric_id("1") && !is_numeric_id("1/2"));
    }

    /// A response or webhook must never fail to parse on an id's shape: a
    /// numeric id deserializes as sent, whatever it holds.
    #[test]
    fn a_numeric_id_from_meta_is_deserialized_as_sent() {
        for sent in ["123/messages", "WABA_ID", "", "wamid.x"] {
            let json = serde_json::to_string(sent).unwrap();
            let waba: WabaId = serde_json::from_str(&json).unwrap();
            assert_eq!(waba.as_str(), sent);
            let media: MediaId = serde_json::from_str(&json).unwrap();
            assert_eq!(media.as_str(), sent);
        }
    }

    /// Opaque ids keep wrapping anything: Meta documents no shape that
    /// would refuse a legitimate one.
    #[test]
    fn an_opaque_id_wraps_any_string() {
        for raw in ["abc/def==", "US.ENT.1", "", "wamid.HBg/x"] {
            assert_eq!(GroupId::new(raw).as_str(), raw);
            assert_eq!(raw.parse::<UserId>().unwrap().as_str(), raw);
            assert_eq!(MessageId::from(raw).as_str(), raw);
        }
    }

    /// `business-scoped-user-ids`: the page's own BSUID and parent BSUID
    /// examples, then each rule of the shape broken once.
    #[test]
    fn a_bsuid_has_the_documented_shape_and_a_parent_bsuid_does_not() {
        let longest = format!("BR.{}", "a1".repeat(64));
        for good in ["US.13491208655302741918", "BR.a", "DE.abcXYZ019", &longest] {
            assert!(UserId::new(good).is_bsuid(), "{good:?}");
        }
        let too_long = format!("US.{}", "1".repeat(129));
        for bad in [
            "US.ENT.11815799212886844830", // parent BSUID
            "13491208655302741918",        // a phone number (`wa_id`)
            "+16505551234",
            "us.13491208655302741918", // country code in lower case
            "USA.1",
            "U.1",
            "1S.1",
            "US.",
            ".1",
            "US.1_2",
            "US.12&bsuid=x",
            "US.１２", // non-ASCII digits
            "",
            too_long.as_str(),
        ] {
            assert!(!UserId::new(bad).is_bsuid(), "{bad:?}");
        }
    }
}
