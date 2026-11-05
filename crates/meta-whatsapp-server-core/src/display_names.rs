//! A number's display name (docs/design/server.md, section 4.2; roadmap
//! M5c2), as services an API adapter calls, with the tenant's WABA token,
//! over the library's `PhoneNumber`:
//!
//! | Operation | Graph call |
//! | --- | --- |
//! | [`DisplayNames::request`] | `POST /{pn}?new_display_name=…` → `{"success": true}`, not replayed on a timeout |
//! | [`DisplayNames::status`] | `GET /{pn}?fields=verified_name,name_status,new_display_name,new_name_status` ([`STATUS_FIELDS`]) |
//!
//! Page: `display-names` (the request, the status fields, the review, "You
//! can change a display name 10 times per 30-day period", the 14 days to
//! register the number again once approved).
//!
//! **What a request does.** It asks Meta to review a new display name; the
//! business's name changes only once Meta approves it **and** the number
//! is registered again (the existing `POST /v1/numbers/{pn}/register`; the
//! page: "Re-registering before approval has no effect", and the approval
//! comes as a `phone_number_name_update` webhook, `phone_number_name_updated`
//! in the service's events). The service does not register the number: that
//! needs the two-step PIN, which only its caller holds. Meta's own limit,
//! ten changes in 30 days, is Meta's to enforce: the service keeps no
//! count, and Meta's refusal is passed on.
//!
//! **Audited** ([`display_name_audit`], action [`REQUESTED`]): a request
//! that Meta answered, and one that may have gone through unanswered (a
//! timeout: outcome `unknown`), with the tenant, the WABA, the number, the
//! key and the length of the name; never the name.
//!
//! **Bounds**, before any request ([`new_display_name`]): the name trimmed,
//! not empty, at most [`NAME_MAX_CHARS`] characters, no control characters and
//! no bidi override or zero-width character ([`is_invisible_or_bidi`]).
//! Meta's pages state no length for a display name (the guidelines are a
//! Help Center article); the limit is the service's own, the one length
//! Meta documents for a display name anywhere (the in-app signup's).
//!
//! The services open on a capability their own [`Authorizer`] made, and
//! admit it first. A failed Graph call is told to the [`Observer`], a `190`
//! marks the WABA's numbers `reconnect_required`, and Meta's `details` are
//! kept.

use meta_whatsapp_rs::Error;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

use crate::audit::{Outcome, Subject, display_name_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::error::ServiceError;
use crate::observe::Observer;
use crate::request::wire;

/// The fields [`DisplayNames::status`] asks Meta for.
pub const STATUS_FIELDS: [&str; 4] = [
    "verified_name",
    "name_status",
    "new_display_name",
    "new_name_status",
];

/// The audit action of [`DisplayNames::request`]: a stored name in the
/// logs, pending the owner's confirmation before the first release.
pub const REQUESTED: &str = "display_name_change_requested";

/// The longest display name the service sends: Meta's pages state none
/// for a display name; 256 is the length of the in-app signup's
/// `display_name`.
pub const NAME_MAX_CHARS: usize = 256;

/// A number's display name and the review of a requested one
/// ([`STATUS_FIELDS`]), as Meta names each value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisplayNameStatus {
    /// The display name WhatsApp holds (it does not say it is approved:
    /// [`Self::name_status`] does).
    pub verified_name: Option<String>,
    /// Its status: `APPROVED`, `AVAILABLE_WITHOUT_REVIEW`, `DECLINED`,
    /// `EXPIRED`, `PENDING_REVIEW` or `NONE`.
    pub name_status: Option<String>,
    /// The display name last requested.
    pub new_display_name: Option<String>,
    /// The review of [`Self::new_display_name`], with the same values.
    pub new_name_status: Option<String>,
}

/// The format characters a text may keep (roadmap M5c2, D165): the
/// joiners the Persian and Indic scripts and emoji sequences need (U+200C,
/// U+200D) and the direction *marks* mixed-direction text needs (U+200E,
/// U+200F, U+061C). Every other format character is refused, and so are
/// the line and paragraph separators: they are how a name is spoofed
/// (bidi overrides, embeddings and isolates reorder what a customer reads;
/// zero-width spaces, the word joiner, the byte-order mark, the soft
/// hyphen, the invisible operators and the tag characters make two names
/// that look equal differ).
pub const ALLOWED_FORMAT_CHARS: [char; 5] =
    ['\u{200C}', '\u{200D}', '\u{200E}', '\u{200F}', '\u{061C}'];

/// Whether `c` is a character a customer-facing text must not carry: a
/// format, line separator or paragraph separator character that is not in
/// [`ALLOWED_FORMAT_CHARS`].
pub fn is_invisible_or_bidi(c: char) -> bool {
    matches!(
        c.general_category(),
        GeneralCategory::Format
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
    ) && !ALLOWED_FORMAT_CHARS.contains(&c)
}

/// The name a request asks for, checked: trimmed, not empty, at most
/// [`NAME_MAX_CHARS`] characters, no control character, and no character
/// of [`is_invisible_or_bidi`].
///
/// # Errors
///
/// `422 invalid_request` on `new_display_name`.
pub fn new_display_name(name: &str) -> Result<String, ServiceError> {
    let name = name.trim();
    if name.is_empty()
        // Nothing to read: only joiners and direction marks.
        || name
            .chars()
            .all(|c| c.is_whitespace() || ALLOWED_FORMAT_CHARS.contains(&c))
        || name.chars().count() > NAME_MAX_CHARS
        || name
            .chars()
            .any(|c| c.is_control() || is_invisible_or_bidi(c))
    {
        return Err(ServiceError::invalid("new_display_name"));
    }
    Ok(name.to_owned())
}

/// The display name operations on a number the caller's tenant owns.
/// Cheap: a few references.
pub struct DisplayNames<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for DisplayNames<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplayNames")
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> DisplayNames<'a> {
    /// The operations on `number`, once `authz` admits it
    /// ([`Authorizer::admit_number`]) and `caller` (whom a request is
    /// audited for).
    ///
    /// # Errors
    ///
    /// `number` another [`Authorizer`] made: `403 forbidden`, and nothing
    /// is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        number: &'a OwnedNumber,
    ) -> Result<Self, ServiceError> {
        authz.admit_number(number)?;
        Ok(Self {
            authz,
            caller,
            number,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self.number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api.with_details(error)
    }

    fn audit(&self, name_chars: usize, outcome: Outcome) {
        display_name_audit(
            REQUESTED,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.number.waba_id().as_str()),
                phone_number_id: Some(self.number.phone_number_id().as_str()),
                ..Subject::default()
            },
            name_chars as u64,
            outcome,
        );
    }

    /// Ask Meta to review `name` as the number's display name (checked by
    /// [`new_display_name`], and answered as it was sent):
    /// `POST /{pn}?new_display_name=…`, not replayed on a timeout. Audited
    /// as [`REQUESTED`].
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `new_display_name`, before any request;
    /// Meta's refusal or failure, with its `details` (a timeout
    /// `may_have_been_sent`: read [`Self::status`]).
    pub async fn request(&self, name: &str) -> Result<String, ServiceError> {
        let name = new_display_name(name)?;
        let chars = name.chars().count();
        let asked = self
            .number
            .client()
            .phone_number(self.number.phone_number_id().clone())
            .request_display_name_change(&name)
            .await;
        match asked {
            Ok(()) => {
                self.audit(chars, Outcome::Answered);
                Ok(name)
            }
            Err(error) => {
                let failed = self.failed(&error).await;
                if failed.may_have_been_sent() {
                    self.audit(chars, Outcome::Unanswered);
                }
                Err(failed)
            }
        }
    }

    /// The number's display name and the review of the one requested
    /// ([`STATUS_FIELDS`]).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn status(&self) -> Result<DisplayNameStatus, ServiceError> {
        let info = self
            .number
            .client()
            .phone_number(self.number.phone_number_id().clone())
            .get(&STATUS_FIELDS)
            .await;
        match info {
            Ok(info) => Ok(DisplayNameStatus {
                verified_name: info.verified_name,
                name_status: wire(info.name_status.as_ref()),
                new_display_name: info.new_display_name,
                new_name_status: wire(info.new_name_status.as_ref()),
            }),
            Err(error) => Err(self.failed(&error).await),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name's bounds: trimmed, 1 to 256 characters (counted as
    /// characters, not bytes), no control character. Decisive: each arm of
    /// [`new_display_name`].
    #[test]
    fn a_name_is_trimmed_and_bounded() {
        assert_eq!(new_display_name("  Lucky Shrub ").unwrap(), "Lucky Shrub");
        assert_eq!(new_display_name("é").unwrap(), "é");
        // The joiners and direction marks scripts need are kept.
        for kept in [
            "می\u{200C}خواهم",
            "שלום\u{200F} Shop",
            "Lucky \u{200E}Shrub",
        ] {
            assert_eq!(new_display_name(kept).unwrap(), kept);
        }
        let longest = "é".repeat(NAME_MAX_CHARS);
        assert_eq!(new_display_name(&longest).unwrap(), longest);
        for bad in [
            String::new(),
            "   ".to_owned(),
            "é".repeat(NAME_MAX_CHARS + 1),
            "Lucky\nShrub".to_owned(),
            "Lucky\u{0}Shrub".to_owned(),
            "\tLucky\u{7f}".to_owned(),
            // Bidi overrides, embeddings and isolates; zero-width and
            // invisible characters; the separators.
            "Lucky\u{202E}Shrub".to_owned(),
            "\u{202A}Lucky".to_owned(),
            "Lucky\u{2066}Shrub\u{2069}".to_owned(),
            "Lucky\u{200B}Shrub".to_owned(),
            "Lucky\u{2060}Shrub".to_owned(),
            "Lucky\u{FEFF}Shrub".to_owned(),
            "Lucky\u{00AD}Shrub".to_owned(),
            "Lucky\u{2062}Shrub".to_owned(),
            "Lucky\u{E0041}Shrub".to_owned(),
            "Lucky\u{2028}Shrub".to_owned(),
            "Lucky\u{2029}Shrub".to_owned(),
            // Only invisible: nothing is left of the name to read.
            "\u{200B}".to_owned(),
            "\u{200F}\u{200D}".to_owned(),
        ] {
            let refused = new_display_name(&bad).unwrap_err();
            assert_eq!(refused.code(), "invalid_request", "{bad:?}");
            assert_eq!(refused.field(), Some("new_display_name"), "{bad:?}");
        }
    }

    /// The audit action is a stored name.
    #[test]
    fn the_action_and_the_fields_are_pinned() {
        assert_eq!(REQUESTED, "display_name_change_requested");
        assert_eq!(
            STATUS_FIELDS.join(","),
            "verified_name,name_status,new_display_name,new_name_status"
        );
    }
}
