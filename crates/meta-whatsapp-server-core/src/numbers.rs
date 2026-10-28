//! Numbers, their business profile, and disconnecting a WABA
//! (docs/design/server.md, section 4.2), as services an API adapter
//! calls, with the tenant's WABA token:
//!
//! | Operation | Graph calls |
//! | --- | --- |
//! | [`Numbers::details`] | `GET /{pn}?fields=display_phone_number,verified_name,quality_rating,name_status,throughput` ([`NUMBER_FIELDS`]) |
//! | [`Numbers::profile`] | `GET /{pn}/whatsapp_business_profile?fields=about,address,description,email,websites,vertical` ([`PROFILE_FIELDS`]) |
//! | [`Numbers::update_profile`] | `POST /{pn}/whatsapp_business_profile` (none for an empty change), then the `GET` above |
//! | [`Waba::disconnect`] | `DELETE /{waba_id}/subscribed_apps`; then the token and bindings are deleted |
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

use crate::authz::{Authorizer, OwnedNumber, OwnedWaba};
use crate::error::ServiceError;
use crate::model::NumberStatus;
use crate::observe::Observer;
use crate::request::wire;

/// The fields [`Numbers::details`] asks Meta for.
pub const NUMBER_FIELDS: [&str; 5] = [
    "display_phone_number",
    "verified_name",
    "quality_rating",
    "name_status",
    "throughput",
];

/// The profile fields the service reads and writes.
pub const PROFILE_FIELDS: [ProfileField; 6] = [
    ProfileField::About,
    ProfileField::Address,
    ProfileField::Description,
    ProfileField::Email,
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
    /// `details`.
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
        } = change;
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
            ..ProfileUpdate::default()
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
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// Disconnect the WABA: unsubscribe the app from it with its token;
    /// only then delete the token and the bindings, the WABA's and its
    /// numbers' ([`OwnedWaba::forget_or_busy`]). When Meta refuses or
    /// fails, nothing is deleted.
    ///
    /// # Errors
    ///
    /// The WABA moved since the capability was made (unbound, maybe bound
    /// to another tenant: its token must not unsubscribe the new holder's
    /// app), or its token or binding moved while it was being forgotten:
    /// `503 storage_unavailable`, retryable (the repeat finds what the
    /// tenant holds now). Meta's refusal or failure, with its `details`
    /// (a timeout `may_have_been_sent`), and nothing deleted.
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
        self.owned.clone().forget_or_busy(self.authz).await
    }
}
