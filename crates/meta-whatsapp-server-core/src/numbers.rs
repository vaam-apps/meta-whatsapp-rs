//! Numbers, their business profile, and disconnecting a WABA
//! (docs/design/server.md, section 4.2), as services an API adapter
//! calls, with the tenant's WABA token:
//!
//! | Operation | Graph calls |
//! | --- | --- |
//! | [`Numbers::details`] | `GET /{pn}?fields=display_phone_number,verified_name,quality_rating,name_status,throughput` ([`NUMBER_FIELDS`]) |
//! | [`Numbers::profile`] | `GET /{pn}/whatsapp_business_profile?fields=about,address,description,email,profile_picture_url,websites,vertical` ([`PROFILE_FIELDS`]) |
//! | [`Numbers::update_profile`] | `POST /{pn}/whatsapp_business_profile` (none for an empty change; a picture's `profile_picture_handle` from [`ProfilePicture`], roadmap M5c2), then the `GET` above |
//! | [`Waba::disconnect`] | `DELETE /{waba_id}/subscribed_apps`; then `number_disconnected` for each of its numbers ([`crate::number_events`]), then the token and bindings are deleted |
//!
//! Pages: `business-profiles`, `solution-providers/manage-phone-numbers`,
//! `reference/whatsapp-business-account/subscribed-apps-api`.
//!
//! The services open on a capability their own [`Authorizer`] made, and
//! admit it first ([`Authorizer::admit_number`], [`Authorizer::admit_waba`]):
//! ownership and the vault were checked when it was made (steps 4 and 5 of
//! [`crate::authz`]). They answer domain values ([`NumberDetails`],
//! [`BusinessProfile`]) or a [`ServiceError`], with Meta's `details`; a
//! failed Graph call is told to the [`Observer`] (the adapter's metrics),
//! and a `190` marks the WABA's numbers `reconnect_required`. The
//! tenant's bindings (its WABAs and numbers, listed) are the records'
//! own, read by the adapter for the admitted caller.

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::business_profile::{Profile, ProfileField, ProfileUpdate, Vertical};
use meta_whatsapp_rs::client::phone_numbers::PhoneNumberInfo;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};

use crate::audit::{Subject, tenant_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber, OwnedWaba};
use crate::display_names::is_invisible_or_bidi;
use crate::error::ServiceError;
use crate::model::NumberStatus;
use crate::number_events::{DisconnectReason, NumberEvent};
use crate::observe::Observer;
use crate::request::wire;
use crate::uploads::ProfilePicture;

/// The fields [`Numbers::details`] asks Meta for.
pub const NUMBER_FIELDS: [&str; 5] = [
    "display_phone_number",
    "verified_name",
    "quality_rating",
    "name_status",
    "throughput",
];

/// The profile fields the service reads and writes.
pub const PROFILE_FIELDS: [ProfileField; 7] = [
    ProfileField::About,
    ProfileField::Address,
    ProfileField::Description,
    ProfileField::Email,
    ProfileField::ProfilePictureUrl,
    ProfileField::Websites,
    ProfileField::Vertical,
];

// ─── The answers ─────────────────────────────────────────────────────────

/// A number's throughput.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Throughput {
    /// Throughput level, as Meta reports it.
    pub level: Option<String>,
}

/// A number's live details, from Meta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberDetails {
    /// The number.
    pub phone_number_id: PhoneNumberId,
    /// Its WABA.
    pub waba_id: WabaId,
    /// The service's connection status: [`NumberStatus::Connected`], as
    /// the details come from Meta with the number's token.
    pub status: NumberStatus,
    /// The number as WhatsApp displays it.
    pub display_phone_number: Option<String>,
    /// Approved display name.
    pub verified_name: Option<String>,
    /// Quality rating, as Meta names it (`GREEN`, `YELLOW`, `RED`, …).
    pub quality_rating: Option<String>,
    /// Display name review status, as Meta names it.
    pub name_status: Option<String>,
    /// Throughput.
    pub throughput: Option<Throughput>,
}

/// A business profile, as the service answers it: [`PROFILE_FIELDS`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BusinessProfile {
    /// "About" text.
    pub about: Option<String>,
    /// Address.
    pub address: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Contact email.
    pub email: Option<String>,
    /// The picture's URL, as Meta answers it (roadmap M5c2).
    pub profile_picture_url: Option<String>,
    /// Websites.
    pub websites: Vec<String>,
    /// Industry, as Meta names it (`RETAIL`, `OTHER`, …).
    pub vertical: Option<String>,
}

impl From<Profile> for BusinessProfile {
    fn from(profile: Profile) -> Self {
        Self {
            vertical: wire(profile.vertical.as_ref()),
            about: profile.about,
            address: profile.address,
            description: profile.description,
            email: profile.email,
            profile_picture_url: profile.profile_picture_url,
            websites: profile.websites,
        }
    }
}

/// A change to a business profile, as the request carried it: absent
/// fields stay as they are. The vertical is Meta's name for it, read by
/// [`Numbers::update_profile`] (`UNDEFINED` and `NOT_A_BIZ` cannot be
/// set).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileChange {
    /// "About" text.
    pub about: Option<String>,
    /// Address (at most 256 characters).
    pub address: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Contact email.
    pub email: Option<String>,
    /// Websites.
    pub websites: Option<Vec<String>>,
    /// Industry, as Meta names it.
    pub vertical: Option<String>,
    /// The picture: the handle of one of the number's own completed
    /// uploads ([`crate::uploads::Uploads::profile_picture`]).
    pub picture: Option<ProfilePicture>,
}

// ─── What customers read ─────────────────────────────────────────────────

/// The longest website the service sends, in characters: the service's
/// own (Meta's pages state only that a website is a URI).
pub const WEBSITE_MAX_CHARS: usize = 2048;

/// The longest email the service sends: RFC 5321's longest address.
pub const EMAIL_MAX_CHARS: usize = 254;

/// A profile text (`about`, `address`, `description`) as the customer
/// reads it: no control character (but a newline in `address` and
/// `description`, when `newlines`), no bidi override and no zero-width
/// character ([`is_invisible_or_bidi`]).
fn profile_text(field: &'static str, text: &str, newlines: bool) -> Result<(), ServiceError> {
    let refused = |c: char| {
        (c.is_control() && !(newlines && matches!(c, '\n' | '\r' | '\t')))
            || is_invisible_or_bidi(c)
    };
    if text.chars().any(refused) {
        return Err(ServiceError::invalid(field));
    }
    Ok(())
}

/// An email: empty (clears it), or one `@` between two non-empty parts,
/// at most [`EMAIL_MAX_CHARS`] characters, with no whitespace, control,
/// bidi or zero-width character. Meta judges the rest.
fn profile_email(email: &str) -> Result<(), ServiceError> {
    if email.is_empty() {
        return Ok(());
    }
    let shaped = email.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty() && !domain.is_empty() && !domain.contains('@')
    });
    if !shaped
        || email.chars().count() > EMAIL_MAX_CHARS
        || email
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || is_invisible_or_bidi(c))
    {
        return Err(ServiceError::invalid("email"));
    }
    Ok(())
}

/// A website: an `http` or `https` URL with a host and no credentials, at
/// most [`WEBSITE_MAX_CHARS`] characters, no whitespace, control, bidi or
/// zero-width character as written (the URL parser would strip some).
/// Nothing else is sent as a customer-facing link: not `javascript:`,
/// `data:`, `ftp:` nor `https://good.example@evil.example/`.
fn profile_website(website: &str) -> Result<(), ServiceError> {
    let bad = || ServiceError::invalid("websites");
    if website.chars().count() > WEBSITE_MAX_CHARS
        || website
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || is_invisible_or_bidi(c))
    {
        return Err(bad());
    }
    let url = url::Url::parse(website).map_err(|_| bad())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(bad());
    }
    Ok(())
}

// ─── The operations ──────────────────────────────────────────────────────

/// The operations on a number the caller's tenant owns: an
/// [`OwnedNumber`] its [`Authorizer`] made, and admits, with that
/// number's token. Cheap: a few references.
pub struct Numbers<'a> {
    authz: &'a Authorizer,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Numbers<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Numbers")
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> Numbers<'a> {
    /// The operations on `number`, once `authz` admits it
    /// ([`Authorizer::admit_number`]): a failed call marks through `authz`
    /// ([`OwnedNumber::failed`]). Failed Graph calls are reported to nobody
    /// until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `number` another [`Authorizer`] made: `403 forbidden`, logged at
    /// `warn`, and nothing is asked of Meta.
    pub fn new(authz: &'a Authorizer, number: &'a OwnedNumber) -> Result<Self, ServiceError> {
        authz.admit_number(number)?;
        Ok(Self {
            authz,
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

    /// The error of a failed Graph call: the number's `failed` (a `190`
    /// marks its WABA's numbers), counted when Meta answered with a Graph
    /// error, with Meta's `details`.
    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self.number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api.with_details(error)
    }

    /// The number's live details, from Meta ([`NUMBER_FIELDS`]).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn details(&self) -> Result<NumberDetails, ServiceError> {
        let info = self
            .number
            .client()
            .phone_number(self.number.phone_number_id().clone())
            .get(&NUMBER_FIELDS)
            .await;
        match info {
            Ok(info) => Ok(self.described(info)),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    fn described(&self, info: PhoneNumberInfo) -> NumberDetails {
        NumberDetails {
            phone_number_id: self.number.phone_number_id().clone(),
            waba_id: self.number.waba_id().clone(),
            status: NumberStatus::Connected,
            quality_rating: wire(info.quality_rating.as_ref()),
            name_status: wire(info.name_status.as_ref()),
            throughput: info.throughput.map(|t| Throughput { level: t.level }),
            display_phone_number: info.display_phone_number,
            verified_name: info.verified_name,
        }
    }

    /// The number's business profile ([`PROFILE_FIELDS`]).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn profile(&self) -> Result<BusinessProfile, ServiceError> {
        let profile = self
            .number
            .client()
            .business_profile(self.number.phone_number_id().clone())
            .get(&PROFILE_FIELDS)
            .await;
        match profile {
            Ok(profile) => Ok(BusinessProfile::from(profile)),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Change the fields `change` gives, then read the whole profile. A
    /// change of nothing writes nothing: only the profile is read.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `vertical` when it does not read as the
    /// library's `Vertical` (whose catch-all takes a name it does not
    /// know, which Meta then judges); the library's checks of the change
    /// (an address over 256 characters, a vertical that can be read but
    /// not set, `NOT_A_BIZ` or `UNDEFINED`: `invalid_request` on its
    /// field), before any request; Meta's refusal or failure, with its
    /// `details`. The picture's handle is already checked
    /// ([`ProfilePicture`]): nothing here reads an upload.
    pub async fn update_profile(
        &self,
        change: ProfileChange,
    ) -> Result<BusinessProfile, ServiceError> {
        let ProfileChange {
            about,
            address,
            description,
            email,
            websites,
            vertical,
            picture,
        } = change;
        // What customers read is checked before anything is asked of Meta
        // (roadmap M5c2, D165).
        for (field, text, newlines) in [
            ("about", about.as_deref(), false),
            ("address", address.as_deref(), true),
            ("description", description.as_deref(), true),
        ] {
            if let Some(text) = text {
                profile_text(field, text, newlines)?;
            }
        }
        if let Some(email) = &email {
            profile_email(email)?;
        }
        if let Some(websites) = &websites {
            for website in websites {
                profile_website(website)?;
            }
        }
        let vertical = vertical
            .map(|v| serde_json::from_value::<Vertical>(serde_json::Value::String(v)))
            .transpose()
            .map_err(|_| ServiceError::invalid("vertical"))?;
        let update = ProfileUpdate {
            about,
            address,
            description,
            email,
            websites,
            vertical,
            profile_picture_handle: picture.map(ProfilePicture::into_handle),
        };
        // Nothing to change: no write, just the profile.
        if update != ProfileUpdate::default() {
            let updated = self
                .number
                .client()
                .business_profile(self.number.phone_number_id().clone())
                .update(&update)
                .await;
            if let Err(error) = updated {
                return Err(self.failed(&error).await);
            }
        }
        self.profile().await
    }
}

/// The operations on a WABA the caller's tenant owns, its templates aside
/// ([`crate::templates`]): an [`OwnedWaba`] its [`Authorizer`] made, and
/// admits, with the WABA's token. Cheap: a few references.
pub struct Waba<'a> {
    authz: &'a Authorizer,
    owned: &'a OwnedWaba,
    observer: &'a dyn Observer,
    /// The deployment's signup, whose Solution Partner mode offboards a
    /// disconnected WABA (roadmap M3a), and the caller it audits the
    /// revocation for.
    signups: Option<(&'a crate::signup::Signups, &'a Caller)>,
}

impl std::fmt::Debug for Waba<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waba")
            .field("waba", self.owned)
            .finish_non_exhaustive()
    }
}

impl<'a> Waba<'a> {
    /// The operations on `waba`, once `authz` admits it
    /// ([`Authorizer::admit_waba`]). Failed Graph calls are reported to
    /// nobody until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `waba` another [`Authorizer`] made: `403 forbidden`, logged at
    /// `warn`, and nothing is unsubscribed or deleted.
    pub fn new(authz: &'a Authorizer, waba: &'a OwnedWaba) -> Result<Self, ServiceError> {
        authz.admit_waba(waba)?;
        Ok(Self {
            authz,
            owned: waba,
            observer: &(),
            signups: None,
        })
    }

    /// Disconnect as `signups`' deployment does (roadmap M3a): in Solution
    /// Partner mode the credit line is revoked before the token and the
    /// bindings are deleted ([`crate::signup::Signups::offboard`]), a
    /// tenant audit event of `caller`'s (`credit_line_revoked`).
    #[must_use]
    pub fn offboarded_by(
        mut self,
        signups: &'a crate::signup::Signups,
        caller: &'a Caller,
    ) -> Self {
        self.signups = Some((signups, caller));
        self
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// Disconnect the WABA: unsubscribe the app from it with its token;
    /// then record `number_disconnected` (`reason`: `tenant_disconnected`)
    /// for each of its numbers, while the binding still holds them for the
    /// tenant (roadmap M2d: after it, the outbox would keep the rows
    /// operator-only); only then delete the token and the bindings, the
    /// WABA's and its numbers' ([`OwnedWaba::forget_or_busy`]). When Meta
    /// refuses or fails, nothing is recorded or deleted. In Solution
    /// Partner mode ([`Self::offboarded_by`]), the credit line is revoked
    /// between the events and the deletion, and a revocation that fails
    /// deletes nothing either (roadmap M3a, design acceptance test M3.4).
    ///
    /// # Errors
    ///
    /// The WABA moved since the capability was made (unbound, maybe bound
    /// to another tenant: its token must not unsubscribe the new holder's
    /// app), or its token or binding moved while it was being forgotten:
    /// `503 storage_unavailable`, retryable (the repeat finds what the
    /// tenant holds now). Meta's refusal or failure, with its `details`
    /// (a timeout `may_have_been_sent`), and nothing deleted. The events
    /// not recorded (the outbox failing): `503 storage_unavailable`, with
    /// nothing deleted; a repeat unsubscribes again and records each event
    /// once.
    pub async fn disconnect(&self) -> Result<(), ServiceError> {
        if !self.owned.still_bound(self.authz).await? {
            return Err(ServiceError::from(StorageError::Busy));
        }
        let unsubscribed = self
            .owned
            .client()
            .waba(self.owned.waba_id().clone())
            .unsubscribe_app()
            .await;
        if let Err(error) = unsubscribed {
            let api = self.owned.failed(self.authz, &error).await;
            if error.graph().is_some() {
                self.observer.graph_error(api.code());
            }
            return Err(api.with_details(&error));
        }
        self.authz
            .record_number_event(
                self.owned.epoch(),
                NumberEvent::Disconnected(DisconnectReason::TenantDisconnected),
            )
            .await?;
        if let Some((signups, caller)) = self.signups {
            // The caller the revocation is audited for: the capability's.
            self.authz.admit(caller)?;
            if caller.tenant() != self.owned.tenant() {
                return Err(ServiceError::forbidden());
            }
            if signups
                .offboard(self.authz, self.owned, self.observer)
                .await?
            {
                tenant_audit(
                    "credit_line_revoked",
                    caller.key_id(),
                    &Subject {
                        tenant: Some(caller.tenant().as_str()),
                        waba_id: Some(self.owned.waba_id().as_str()),
                        ..Subject::default()
                    },
                );
                return Ok(());
            }
        }
        self.owned.clone().forget_or_busy(self.authz).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A website is an `http` or `https` URL with a host and no
    /// credentials. Decisive: the scheme, the host, the credentials and the
    /// character arms of `profile_website`.
    #[test]
    fn a_website_is_a_plain_http_or_https_url() {
        for ok in [
            "https://www.luckyshrub.com/",
            "http://luckyshrub.com",
            "HTTPS://Lucky.example/a?b=c#d",
            "https://bücher.example/",
        ] {
            assert!(profile_website(ok).is_ok(), "{ok}");
        }
        let too_long = format!("https://a.example/{}", "a".repeat(WEBSITE_MAX_CHARS));
        for bad in [
            "javascript:alert(1)",
            "data:text/html,x",
            "ftp://luckyshrub.com/",
            "file:///etc/passwd",
            "mailto:a@b.example",
            "//luckyshrub.com",
            "luckyshrub.com",
            "https://",
            "https://user:pw@luckyshrub.com/",
            "https://:pw@luckyshrub.com/",
            "https://good.example@evil.example/",
            "https://lucky shrub.example/",
            "https://luckyshrub.example/\n",
            "https://lucky\u{200B}shrub.example/",
            "https://luckyshrub.example/\u{202E}",
            "",
            too_long.as_str(),
        ] {
            let refused = profile_website(bad).expect_err(bad);
            assert_eq!(refused.field(), Some("websites"), "{bad:?}");
        }
    }

    /// An email is one `@` between two parts, short, without whitespace,
    /// control or invisible characters; empty clears it.
    #[test]
    fn an_email_has_the_shape_and_the_bounds() {
        for ok in ["", "lucky@luckyshrub.com", "a@b"] {
            assert!(profile_email(ok).is_ok(), "{ok:?}");
        }
        let longest = format!("{}@b.example", "a".repeat(EMAIL_MAX_CHARS - 10));
        assert_eq!(longest.chars().count(), EMAIL_MAX_CHARS);
        assert!(profile_email(&longest).is_ok());
        let too_long = format!("a{longest}");
        for bad in [
            "lucky",
            "@luckyshrub.com",
            "lucky@",
            "a@b@c",
            "lucky @luckyshrub.com",
            "lucky@luckyshrub.com\n",
            "lucky\u{200B}@luckyshrub.com",
            "lucky@luckyshrub.com\u{202E}",
            too_long.as_str(),
        ] {
            let refused = profile_email(bad).unwrap_err();
            assert_eq!(refused.field(), Some("email"), "{bad:?}");
        }
    }

    /// A text carries no control, bidi or zero-width character; a newline
    /// only where the field allows it.
    #[test]
    fn a_profile_text_carries_no_control_bidi_or_zero_width_character() {
        assert!(profile_text("about", "Succulent specialists! 🌵", false).is_ok());
        assert!(profile_text("about", "שלום\u{200F} Shop", false).is_ok());
        assert!(profile_text("description", "Line\nTwo\tcolumns", true).is_ok());
        for (text, newlines) in [
            ("a\nb", false),
            ("a\u{0}b", true),
            ("a\u{7f}b", true),
            ("a\u{202E}b", true),
            ("a\u{2067}b", false),
            ("a\u{200B}b", true),
            ("a\u{2028}b", true),
        ] {
            let refused = profile_text("about", text, newlines).unwrap_err();
            assert_eq!(refused.field(), Some("about"), "{text:?}");
        }
    }
}
