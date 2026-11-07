//! Calling (docs/design/server.md, section 4.2; roadmap M5e), as services an
//! API adapter calls, with the tenant's WABA token, over the library's
//! `client::calling`:
//!
//! | Operation | Graph call |
//! | --- | --- |
//! | [`Calls::settings`] | `GET /{pn}/settings` (the `calling` object; **never** with `include_sip_credentials`) |
//! | [`Calls::sip_credentials`] | `GET /{pn}/settings?include_sip_credentials=true`: the one place it is sent (scope `call_credentials` and the deployment's setting; roadmap M5e2) |
//! | [`Calls::upload_voicemail_announcement`] | `POST /{pn}/media` with `use_case=call_voicemail_announcement` (roadmap M5e2) |
//! | [`Calls::update_settings`] | `POST /{pn}/settings` with `{"calling": …}` (a voicemail announcement's media looked up first, as the number's own) |
//! | [`Calls::permissions`] | `GET /{pn}/call_permissions?user_wa_id=…` or `?recipient=…` |
//! | [`Calls::connect`] | `POST /{pn}/calls` with `action: connect` and the caller's SDP offer |
//! | [`Calls::pre_accept`], [`Calls::accept`], [`Calls::reject`], [`Calls::terminate`] | `POST /{pn}/calls` with `action` and the `call_id` |
//!
//! Pages: `calling`, `calling/call-settings`,
//! `calling/user-initiated-calls`, `calling/business-initiated-calls`,
//! `calling/user-call-permissions`, `calling/call-recording`,
//! `calling/call-transcription` and `reference/whatsapp-business-phone-number/calling-api`.
//! Call analytics are the WABA's, with the other analytics
//! ([`crate::analytics::Analytics::calls`]).
//!
//! **A call is the number's, or it does not exist** (the design's M5.2).
//! Meta documents no way to read a call by its id, and a call id says
//! nothing of the tenant that may act on it (one token may reach several
//! tenants' numbers). So, as for groups (D150), the service keeps its own
//! records of the calls a number holds ([`CallRecords`], a typed store on
//! the bundle's `KvStore`, namespace [`NAMESPACE`], pending the owner's
//! confirmation), and decides **before the tenant's token is opened**:
//!
//! - **written** by the webhook pipeline for every `calls` event routed to
//!   a tenant's number (a call that rings, a status, a termination), and by
//!   [`Calls::connect`] for the call it places;
//! - **read** by [`CallRecords::owned`], which runs step 4 of the
//!   authorization order for the number (no token) and then looks the call
//!   up under that number: another tenant's call, another number's, an
//!   unrecorded one and an id no call has are all `404 not_found`, with
//!   nothing asked of Meta and **no vault read**.
//!
//! A record holds the tenant and the number, is keyed by a SHA-256 of the
//! two ids (the call id is not stored in clear) and **expires**
//! ([`RECORD_TTL`], renewed by each event): a call lasts hours at most, so
//! no purge is ever needed.
//!
//! **Secrets.** An SDP offer or answer carries the endpoint's addresses and
//! its ICE credentials, and a SIP server's password authenticates the
//! business to Meta. Neither reaches a log, an error, an audit event or
//! `Debug`; the SIP password is asked of Meta (`include_sip_credentials`) and
//! answered only by [`Calls::sip_credentials`], never by [`Calls::settings`];
//! a failed call answers Meta's code and
//! the service's kind, never Meta's text (it may quote an SDP line).
//!
//! **Recording and transcription are the caller's per-call choice** and the
//! service decides nothing about them: they are sent when the request sets
//! them and never otherwise (Meta then plays an announcement to both
//! participants, and a participant who does not consent ends the call).
//! The audit event says whether either was enabled.

use std::fmt;
use std::sync::Arc;

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::calling::settings::{
    AudioCodec, CallIconVisibility, CallingSettings, DayOfWeek, FeatureStatus, VoicemailTrigger,
};
use meta_whatsapp_rs::client::calling::{
    AcceptOptions, CallPermissions, CallRecording, CallTranscription, ConnectCall,
};
use meta_whatsapp_rs::client::media::UploadOptions;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{CallId, MediaId, PhoneNumberId, UserId};
use meta_whatsapp_rs::core::recipient::Recipient;
use meta_whatsapp_rs::core::store::{Expiry, JsonStore, KvStore};
use meta_whatsapp_rs::webhooks::WebhookEvent;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

use time::OffsetDateTime;

use crate::audit::{CallFacts, Outcome, Subject, calls_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber, object_refused};
use crate::error::ServiceError;
use crate::messages::is_e164;
use crate::model::TenantId;
use crate::observe::Observer;
use crate::ratelimit::{Slot, Slots};
use crate::request::typed_graph_id;

/// The target of this module's log lines.
const LOG_TARGET: &str = "meta_whatsapp_server::api::calls";

/// The `KvStore` namespace of the calls' records: a stored name, pending
/// the owner's confirmation before the first release.
pub const NAMESPACE: &str = "wa.server.call";

/// How long a call's record lives, renewed by each event of the call: the
/// service's own bound (D212). Meta documents no longest call; a call
/// action on a call whose last event is older than this is `404`.
pub const RECORD_TTL: Duration = Duration::from_hours(24);

/// The audit actions (docs/design/server.md, D216); pending the owner's
/// confirmation before the first release.
const SETTINGS_UPDATED: &str = "call_settings_updated";
const CALL_REQUESTED: &str = "call_connect_requested";
const CALL_PRE_ACCEPTED: &str = "call_pre_accepted";
const CALL_ACCEPTED: &str = "call_accepted";
const CALL_REJECTED: &str = "call_rejected";
const CALL_TERMINATED: &str = "call_terminated";
/// Roadmap M5e2, design D410 and D411.
const SIP_CREDENTIALS_READ: &str = "call_sip_credentials_read";
const VOICEMAIL_UPLOADED: &str = "call_voicemail_announcement_uploaded";

/// Longest a call id may be: the service's own bound (Meta's ids are
/// `wacid.` and about thirty characters; none is documented).
const MAX_ID_CHARS: usize = 512;

/// Longest SDP offer or answer taken, in bytes: the service's own bound, half the request body limit
/// (D213). A WebRTC offer is a few KiB; Meta documents no limit.
pub const SDP_MAX_BYTES: usize = 32 * 1024;

// ─── The calls a number holds ────────────────────────────────────────────

/// What the service keeps of a call: whose it is. No call id (the key is a
/// hash of it).
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    tenant: String,
    phone_number_id: String,
}

/// The records of the calls each number holds, over the bundle's `KvStore`
/// (a typed store on the port, never a port of its own): what decides,
/// before any token is opened, whether a call is a number's
/// ([`Self::owned`]). Cheap to clone.
#[derive(Clone)]
pub struct CallRecords {
    store: JsonStore<Record>,
}

impl fmt::Debug for CallRecords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallRecords").finish_non_exhaustive()
    }
}

/// A call's id as the service takes one: 1 to [`MAX_ID_CHARS`] visible
/// ASCII characters. Anything else cannot be a call the service holds.
fn call_id(raw: &str) -> Option<CallId> {
    (!raw.is_empty() && raw.len() <= MAX_ID_CHARS && raw.bytes().all(|b| b.is_ascii_graphic()))
        .then(|| CallId::new(raw))
}

/// A number's id as a record key may hold it: digits only, so that the
/// key's `.` always separates the number from the hash.
fn keyable(number: &PhoneNumberId) -> bool {
    !number.as_str().is_empty() && number.as_str().bytes().all(|b| b.is_ascii_digit())
}

impl CallRecords {
    /// The records in `kv`.
    pub fn new(kv: Arc<dyn KvStore>) -> Self {
        Self {
            store: JsonStore::new(kv, NAMESPACE),
        }
    }

    /// The record's key: the number, and a hash of the call's id.
    fn key(number: &PhoneNumberId, call: &CallId) -> String {
        let mut hash = Sha256::new();
        hash.update(b"meta-whatsapp-server/call/v1\0");
        hash.update(number.as_str().as_bytes());
        hash.update(b"\0");
        hash.update(call.as_str().as_bytes());
        format!("{}.{}", number.as_str(), hex::encode(hash.finalize()))
    }

    /// Keep `call` as `tenant`'s number `number`'s for [`RECORD_TTL`].
    /// Crate-private: only the webhook pipeline and [`Calls::connect`]
    /// write the records. Written again by each event, which renews the
    /// expiry.
    pub(crate) async fn record(
        &self,
        tenant: &TenantId,
        number: &PhoneNumberId,
        call: &CallId,
    ) -> Result<(), StorageError> {
        if !keyable(number) || call_id(call.as_str()).is_none() {
            return Ok(());
        }
        let record = Record {
            tenant: tenant.as_str().to_owned(),
            phone_number_id: number.as_str().to_owned(),
        };
        self.store
            .put(&Self::key(number, call), &record, Expiry::After(RECORD_TTL))
            .await?;
        Ok(())
    }

    /// What a recorded webhook event says about the calls of a number held
    /// by `tenant`: every call event and every call status names a call
    /// the number holds. Crate-private: the webhook pipeline's, once the
    /// event is routed to the tenant owning its number.
    pub(crate) async fn on_event(
        &self,
        tenant: &TenantId,
        event: &WebhookEvent,
    ) -> Result<(), StorageError> {
        match event {
            WebhookEvent::CallUpdated {
                phone_number_id,
                call,
                ..
            } => self.record(tenant, phone_number_id, &call.id).await,
            WebhookEvent::CallStatusUpdated {
                phone_number_id,
                status,
                ..
            } => self.record(tenant, phone_number_id, &status.id).await,
            _ => Ok(()),
        }
    }

    /// The call `call` of the number `pn`, for `caller`: step 4 of the
    /// authorization order for the number (bound to the caller's tenant:
    /// **no token, no vault read**), then the call among the number's
    /// ([the module](self)). The [`OwnedCall`] is what lets the caller open
    /// the token (step 5) and act on that call only.
    ///
    /// # Errors
    ///
    /// `403 forbidden` (a `caller` another [`Authorizer`] made); `404
    /// not_found`: the number is not the caller's tenant's, or the call is
    /// not among the number's (another tenant's, another number's, none,
    /// an expired one, or an id no call has), the same answer for each,
    /// with nothing asked of Meta; `503 storage_unavailable`.
    pub async fn owned(
        &self,
        authz: &Authorizer,
        caller: &Caller,
        pn: &PhoneNumberId,
        call: &str,
    ) -> Result<OwnedCall, ServiceError> {
        authz.admit(caller)?;
        authz.tenant_number(caller, pn).await?;
        let Some(call) = call_id(call).filter(|_| keyable(pn)) else {
            return Err(ServiceError::not_found());
        };
        let held = self.store.get(&Self::key(pn, &call)).await?;
        match held {
            Some((record, _, _))
                if record.tenant == caller.tenant().as_str()
                    && record.phone_number_id == pn.as_str() =>
            {
                Ok(OwnedCall {
                    phone_number_id: pn.clone(),
                    tenant: caller.tenant().clone(),
                    call,
                })
            }
            _ => {
                tracing::info!(
                    target: LOG_TARGET,
                    "a call the number does not hold: not found, Meta not asked"
                );
                Err(ServiceError::not_found())
            }
        }
    }
}

/// A call the caller's tenant's number holds, proved by the records before
/// any token was opened. Only [`CallRecords::owned`] makes one. `Debug`
/// shows nothing of the call.
#[derive(Clone)]
pub struct OwnedCall {
    phone_number_id: PhoneNumberId,
    tenant: TenantId,
    call: CallId,
}

impl fmt::Debug for OwnedCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedCall")
            .field("phone_number_id", &self.phone_number_id)
            .finish_non_exhaustive()
    }
}

impl OwnedCall {
    /// The number the call is held by.
    pub fn phone_number_id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }
}

// ─── The requests' checks ────────────────────────────────────────────────

/// A user a request names: a phone number and/or a BSUID.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Callee {
    /// E.164 **with** its `+`.
    pub phone: Option<String>,
    /// A business-scoped user id (`US.13491208655302741918`) or a parent
    /// one (`US.ENT.11815799212886844830`).
    pub user_id: Option<String>,
}

impl fmt::Debug for Callee {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Callee").finish_non_exhaustive()
    }
}

/// Whether `id` is a BSUID or a parent BSUID in the shapes Meta documents:
/// `US.` and 1 to 128 letters or digits, or `US.ENT.` and the same.
fn is_bsuid_or_parent(id: &str) -> bool {
    let id = UserId::new(id);
    if id.is_bsuid() {
        return true;
    }
    let text = id.as_str();
    let bytes = text.as_bytes();
    bytes.len() > 7
        && bytes[..2].iter().all(u8::is_ascii_uppercase)
        && &text[2..7] == ".ENT."
        && (1..=128).contains(&text[7..].len())
        && text[7..].bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The library's recipient for `callee`, checked; errors name `prefix`'s
/// `phone` and `user_id`, and `whole` when neither is given. With both, Meta takes the phone number.
///
/// # Errors
///
/// `422 invalid_request` on `{prefix}phone` (not strict E.164 with its
/// `+`), `{prefix}user_id` (not a BSUID or a parent BSUID), or on
/// `{prefix}` when neither is given. Never quotes a value.
pub fn callee(callee: Callee, prefix: &str, whole: &str) -> Result<Recipient, ServiceError> {
    let field = |name: &str| ServiceError::invalid(format!("{prefix}{name}"));
    let phone = match callee.phone {
        Some(phone) if is_e164(&phone) => Some(phone),
        Some(_) => return Err(field("phone")),
        None => None,
    };
    let user = match callee.user_id {
        Some(user) if is_bsuid_or_parent(&user) => Some(UserId::new(user)),
        Some(_) => return Err(field("user_id")),
        None => None,
    };
    match (phone, user) {
        (Some(phone), Some(user)) => Ok(Recipient::PhoneAndUser { phone, user }),
        (Some(phone), None) => Ok(Recipient::Phone(phone)),
        (None, Some(user)) => Ok(Recipient::User(user)),
        (None, None) => Err(ServiceError::invalid(whole)),
    }
}

/// An SDP offer or answer, checked: not blank, at most [`SDP_MAX_BYTES`].
/// Passed through unparsed. Never quotes it.
fn sdp(field: &'static str, text: &str) -> Result<(), ServiceError> {
    if text.trim().is_empty() || text.len() > SDP_MAX_BYTES || text.contains('\0') {
        return Err(ServiceError::invalid(field));
    }
    Ok(())
}

/// The `recording` or `transcription` object of a request, as the caller
/// carried it.
#[derive(Clone, PartialEq, Eq)]
pub struct Announcement {
    /// `ENABLED` or `DISABLED`.
    pub status: String,
    /// Why the call is recorded or transcribed, spoken to both
    /// participants (required when enabled; at most 250 characters).
    pub purpose: Option<String>,
    /// The announcement's locale (`en_US`; required when enabled).
    pub announcement_language: Option<String>,
}

impl fmt::Debug for Announcement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Announcement")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl Announcement {
    fn status(&self, field: &str) -> Result<FeatureStatus, ServiceError> {
        match self.status.as_str() {
            "ENABLED" => Ok(FeatureStatus::Enabled),
            "DISABLED" => Ok(FeatureStatus::Disabled),
            _ => Err(ServiceError::invalid(format!("{field}.status"))),
        }
    }

    fn recording(&self) -> Result<CallRecording, ServiceError> {
        Ok(CallRecording {
            status: self.status("recording")?,
            purpose: self.purpose.clone(),
            announcement_language: self.announcement_language.clone(),
        })
    }

    fn transcription(&self) -> Result<CallTranscription, ServiceError> {
        Ok(CallTranscription {
            status: self.status("transcription")?,
            purpose: self.purpose.clone(),
            announcement_language: self.announcement_language.clone(),
        })
    }
}

/// A call to place.
#[derive(Clone)]
pub struct ConnectRequest {
    /// The callee.
    pub to: Callee,
    /// The caller's SDP offer (RFC 8866), passed through unparsed.
    pub sdp_offer: String,
    /// Up to 512 characters echoed in the call's webhooks.
    pub biz_opaque_callback_data: Option<String>,
    /// Record the call, if set.
    pub recording: Option<Announcement>,
    /// Transcribe the call, if set.
    pub transcription: Option<Announcement>,
}

impl fmt::Debug for ConnectRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectRequest").finish_non_exhaustive()
    }
}

/// What accepting a call may add.
#[derive(Clone, Default)]
pub struct AcceptRequest {
    /// The caller's SDP answer, passed through unparsed.
    pub sdp_answer: String,
    /// Up to 512 characters echoed in the call's webhooks.
    pub biz_opaque_callback_data: Option<String>,
    /// Record the call, if set.
    pub recording: Option<Announcement>,
    /// Transcribe the call, if set.
    pub transcription: Option<Announcement>,
}

impl fmt::Debug for AcceptRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcceptRequest").finish_non_exhaustive()
    }
}

/// `biz_opaque_callback_data`, checked: at most 512 characters.
fn opaque(data: Option<&str>) -> Result<(), ServiceError> {
    if data.is_some_and(|d| {
        d.chars().count() > meta_whatsapp_rs::client::calling::BIZ_OPAQUE_CALLBACK_DATA_MAX_CHARS
    }) {
        return Err(ServiceError::invalid("biz_opaque_callback_data"));
    }
    Ok(())
}

/// The calling settings a request changes, checked ([`settings_change`]).
#[derive(Clone)]
pub struct SettingsChange(CallingSettings);

impl fmt::Debug for SettingsChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SettingsChange")
            .field("sections", &sections(&self.0))
            .finish()
    }
}

/// The top-level sections `settings` sets, by Meta's names.
fn sections(settings: &CallingSettings) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut add = |set: bool, name| {
        if set {
            out.push(name);
        }
    };
    add(settings.status.is_some(), "status");
    add(
        settings.call_icon_visibility.is_some(),
        "call_icon_visibility",
    );
    add(settings.call_icons.is_some(), "call_icons");
    add(settings.call_hours.is_some(), "call_hours");
    add(
        settings.callback_permission_status.is_some(),
        "callback_permission_status",
    );
    add(settings.sip.is_some(), "sip");
    add(settings.audio.is_some(), "audio");
    add(settings.voicemail.is_some(), "voicemail");
    out
}

/// Whether `status` is a value Meta documents.
fn known_status(status: &FeatureStatus) -> bool {
    !matches!(status, FeatureStatus::Other(_))
}

/// The settings change a request carries: Meta's `calling` object, read
/// from `object` into the library's type and checked for the values the
/// pages list (an enum Meta does not document is refused, not passed on),
/// at least one section set. The library checks the rest (call hours,
/// the SIP server, voicemail) before it sends.
///
/// # Errors
///
/// `422 invalid_request` on `body` (a key or value the object does not
/// have), or on the section at fault (`call_hours.status`, `sip.status`,
/// `voicemail.triggers`, …); on `update` when no section is set. Never
/// quotes a value.
pub fn settings_change(object: serde_json::Value) -> Result<SettingsChange, ServiceError> {
    let settings: CallingSettings =
        serde_json::from_value(object).map_err(|_| ServiceError::invalid("body"))?;
    let bad = |field: &str| ServiceError::invalid(field);
    if settings.status.as_ref().is_some_and(|s| !known_status(s)) {
        return Err(bad("status"));
    }
    if settings
        .callback_permission_status
        .as_ref()
        .is_some_and(|s| !known_status(s))
    {
        return Err(bad("callback_permission_status"));
    }
    if matches!(
        settings.call_icon_visibility,
        Some(CallIconVisibility::Other(_))
    ) {
        return Err(bad("call_icon_visibility"));
    }
    if let Some(hours) = &settings.call_hours {
        if !known_status(&hours.status) {
            return Err(bad("call_hours.status"));
        }
        if hours
            .weekly_operating_hours
            .iter()
            .any(|w| matches!(w.day_of_week, DayOfWeek::Other(_)))
        {
            return Err(bad("call_hours.weekly_operating_hours"));
        }
    }
    if let Some(sip) = &settings.sip {
        if sip.status.as_ref().is_some_and(|s| !known_status(s)) {
            return Err(bad("sip.status"));
        }
        if sip
            .webhook_delivery
            .as_ref()
            .is_some_and(|s| !known_status(s))
        {
            return Err(bad("sip.webhook_delivery"));
        }
    }
    if let Some(audio) = &settings.audio
        && audio
            .additional_codecs
            .iter()
            .flatten()
            .any(|c| matches!(c, AudioCodec::Other(_)))
    {
        return Err(bad("audio.additional_codecs"));
    }
    if let Some(voicemail) = &settings.voicemail {
        if !known_status(&voicemail.status) {
            return Err(bad("voicemail.status"));
        }
        if voicemail
            .triggers
            .iter()
            .any(|t| matches!(t, VoicemailTrigger::Other(_)))
        {
            return Err(bad("voicemail.triggers"));
        }
    }
    if sections(&settings).is_empty() {
        return Err(bad("update"));
    }
    Ok(SettingsChange(settings))
}

impl SettingsChange {
    /// The voicemail announcement's media id, when the change sets one.
    fn announcement_media(&self) -> Option<&MediaId> {
        self.0
            .voicemail
            .as_ref()?
            .audio
            .as_ref()?
            .default
            .announcement_media_id
            .as_ref()
    }
}

// ─── The answers ─────────────────────────────────────────────────────────

/// The call a `connect` placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    /// Its id (`wacid.…`): what the call's actions and webhooks name.
    pub call_id: String,
}

/// A limit on a permission action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionLimit {
    /// ISO 8601 duration (`PT24H`).
    pub time_period: String,
    /// Allowed within the period.
    pub max_allowed: u32,
    /// Used within the period.
    pub current_usage: u32,
    /// When the limit resets; present once it is reached.
    pub limit_expiration_time: Option<OffsetDateTime>,
}

/// What the business may do with a user's call permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionAction {
    /// `start_call` or `send_call_permission_request`, as Meta writes it.
    pub action_name: String,
    /// Whether it is allowed right now, every limit considered.
    pub can_perform_action: bool,
    /// The limits on it.
    pub limits: Vec<PermissionLimit>,
}

/// A user's call permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permissions {
    /// `no_permission`, `temporary`, `permanent`, `granted`, `pending`,
    /// `denied` or `expired`, as Meta writes it.
    pub status: String,
    /// When a temporary permission expires.
    pub expiration_time: Option<OffsetDateTime>,
    /// What the business may do now.
    pub actions: Vec<PermissionAction>,
}

impl From<CallPermissions> for Permissions {
    fn from(p: CallPermissions) -> Self {
        Self {
            status: p.permission.status.as_str().to_owned(),
            expiration_time: p.permission.expiration_time,
            actions: p
                .actions
                .into_iter()
                .map(|a| PermissionAction {
                    action_name: a.action_name.as_str().to_owned(),
                    can_perform_action: a.can_perform_action,
                    limits: a
                        .limits
                        .into_iter()
                        .map(|l| PermissionLimit {
                            time_period: l.time_period,
                            max_allowed: l.max_allowed,
                            current_usage: l.current_usage,
                            limit_expiration_time: l.limit_expiration_time,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

/// The calling settings of a number, without any secret: the SIP server's
/// password is dropped here (the library never asks for it unless told).
#[derive(Debug, Clone)]
pub struct Settings {
    /// The `calling` object, as Meta describes it.
    pub calling: CallingSettings,
}

/// A restriction Meta put on a number's calling (the response-only
/// `restrictions` object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restriction {
    /// `RESTRICTED_BUSINESS_INITIATED_CALLING` or
    /// `RESTRICTED_USER_INITIATED_CALLING`, as Meta writes it.
    pub kind: String,
    /// Why, in Meta's words.
    pub reason: Option<String>,
    /// When it lifts.
    pub expiration: Option<OffsetDateTime>,
}

/// `settings` with every SIP server's password removed: the second lock
/// after the request's (`include_sip_credentials` is not sent), for a
/// password Meta answers anyway. Decisive: the unit test sets one and reads
/// it back through the field, which no serializer can hide.
fn without_sip_passwords(mut settings: CallingSettings) -> CallingSettings {
    for server in settings
        .sip
        .iter_mut()
        .flat_map(|s| s.servers.iter_mut().flatten())
    {
        server.sip_user_password = None;
    }
    settings
}

impl Settings {
    /// The restrictions Meta reports.
    pub fn restrictions(&self) -> Vec<Restriction> {
        self.calling
            .restrictions
            .iter()
            .flat_map(|r| &r.restrictions_list)
            .map(|r| Restriction {
                kind: r.kind.as_str().to_owned(),
                reason: r.reason.clone(),
                expiration: r.expiration,
            })
            .collect()
    }
}

/// One SIP server of a number's calling settings, with the password Meta
/// generated for it: **a secret** (D410). Its `Debug` shows neither the
/// host nor the password; [`Self::password`] is the one way to it, for
/// the answer of the route behind `call_credentials` and nothing else.
#[derive(Clone)]
pub struct SipCredential {
    hostname: String,
    port: Option<u16>,
    password: Option<meta_whatsapp_rs::client::calling::settings::SipPassword>,
}

impl fmt::Debug for SipCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SipCredential").finish_non_exhaustive()
    }
}

impl SipCredential {
    /// The server's host name.
    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    /// Its port, when Meta reports one.
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// The password Meta generated, when it sent one. Never log it, store
    /// it or put it in an error; the answer to the caller is the only
    /// place it goes.
    pub fn password(&self) -> Option<&str> {
        self.password
            .as_ref()
            .map(meta_whatsapp_rs::client::calling::settings::SipPassword::expose_secret)
    }
}

/// The SIP servers of a number's calling settings, with their passwords
/// (D410). No `Debug` output of its own beyond a count.
#[derive(Clone)]
pub struct SipCredentials {
    /// `ENABLED` or `DISABLED`, as Meta writes it (none when SIP was never
    /// configured).
    pub status: Option<String>,
    /// The servers, each with its password.
    pub servers: Vec<SipCredential>,
}

impl fmt::Debug for SipCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SipCredentials")
            .field("servers", &self.servers.len())
            .finish_non_exhaustive()
    }
}

/// The largest voicemail announcement taken: Meta's audio limit (16 MiB),
/// far above what an announcement under 60 seconds needs.
pub const VOICEMAIL_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// A voicemail announcement's file, checked before any request: not empty,
/// at most [`VOICEMAIL_MAX_BYTES`], and Ogg with the Opus codec by its own
/// first bytes (`OggS` and, in the first page, `OpusHead`), the format
/// `calling/call-settings` requires. The part's claimed type is never
/// trusted. Its length (under 60 seconds) is Meta's to check: the service
/// decodes no audio. `Debug` shows no byte.
#[derive(Clone)]
pub struct VoicemailAudio {
    data: bytes::Bytes,
}

impl fmt::Debug for VoicemailAudio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VoicemailAudio")
            .field("len", &self.data.len())
            .finish()
    }
}

impl VoicemailAudio {
    /// The file.
    pub fn data(&self) -> &bytes::Bytes {
        &self.data
    }

    /// Check `data`.
    ///
    /// # Errors
    ///
    /// `413 media_too_large` over [`VOICEMAIL_MAX_BYTES`]; `422
    /// invalid_request` on `file` when it is empty or is not Ogg with
    /// Opus.
    pub fn new(data: bytes::Bytes) -> Result<Self, ServiceError> {
        if u64::try_from(data.len()).unwrap_or(u64::MAX) > VOICEMAIL_MAX_BYTES {
            return Err(ServiceError::new(crate::error::ErrorCode::MEDIA_TOO_LARGE));
        }
        let head = &data[..data.len().min(128)];
        if !data.starts_with(b"OggS") || !head.windows(8).any(|w| w == b"OpusHead") {
            return Err(ServiceError::invalid("file"));
        }
        Ok(Self { data })
    }
}

/// A voicemail announcement that was uploaded: its media id, which the
/// settings' `voicemail.audio.default.announcement_media_id` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnouncementUploaded {
    /// The media id.
    pub media_id: String,
}

// ─── The operations ──────────────────────────────────────────────────────

/// The calling operations on a number of the caller's tenant: an
/// [`OwnedNumber`] its [`Authorizer`] made from that tenant's binding, for
/// a [`Caller`] it made, both admitted ([`Self::new`]), with the number's
/// WABA token. Cheap: a few references.
pub struct Calls<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    records: &'a CallRecords,
    observer: &'a dyn Observer,
    /// [`Self::with_sip_credentials`].
    sip_credentials: bool,
    /// [`Self::naming_the_account`].
    names_account: bool,
}

impl fmt::Debug for Calls<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Calls")
            .field("caller", self.caller)
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> Calls<'a> {
    /// The operations on `number` for `caller`, once `authz` admits both
    /// ([`Authorizer::admit`], [`Authorizer::admit_number`]) and `number`
    /// was made from a binding of `caller`'s tenant. Failed Graph calls are
    /// reported to nobody until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `caller` or `number` another [`Authorizer`] made: `403 forbidden`.
    /// `number` another tenant's: `404 not_found`, logged at `warn`.
    /// Either way, before anything is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        number: &'a OwnedNumber,
        records: &'a CallRecords,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_number(number)?;
        if number.tenant() != caller.tenant() {
            tracing::warn!("refused the calling services on another tenant's number");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
            number,
            records,
            observer: &(),
            sip_credentials: false,
            names_account: false,
        })
    }

    /// Whether [`Self::sip_credentials`] answers (the deployment's
    /// `WA_SERVER_CALL_SIP_CREDENTIALS`, design D410). Off until called:
    /// the read is `403 feature_not_available` and nothing is asked of
    /// Meta. The route's scope is checked by the adapter before this.
    #[must_use]
    pub fn with_sip_credentials(mut self, enabled: bool) -> Self {
        self.sip_credentials = enabled;
        self
    }

    /// Whether every action of a call (`connect`, `pre_accept`, `accept`,
    /// `reject`, `terminate`) names the number's binding's WABA id as its
    /// `messaging_account_id` (roadmap L29, design D310; the adapter passes
    /// `WA_SERVER_SEND_MESSAGING_ACCOUNT_ID`, as for a send). Off until
    /// called: an action names none and Meta resolves the account from the
    /// token.
    ///
    /// The value is the binding's WABA id as this [`OwnedNumber`] holds
    /// it, the same on every action a request makes for one call as long
    /// as the binding does not change; nothing of it is stored with the
    /// call (D311). A request cannot carry the field itself: the call
    /// bodies' unknown keys are refused. The settings and permission reads
    /// never carry it (their pages have no such field).
    #[must_use]
    pub fn naming_the_account(mut self, enabled: bool) -> Self {
        self.names_account = enabled;
        self
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// The error of a failed Graph call: Meta's code and the service's
    /// kind, **without Meta's texts** in the answer or any log (a refusal
    /// of an SDP may quote one of its lines).
    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self
            .number
            .failed_withholding_texts(self.authz, error)
            .await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    /// The library's calling handle, naming the binding's account when
    /// [`Self::naming_the_account`]: the one place the field is set.
    fn client(&self) -> meta_whatsapp_rs::client::calling::Calling {
        let calling = self
            .number
            .client()
            .calling(self.number.phone_number_id().clone());
        if self.names_account {
            calling.messaging_account_id(self.number.waba_id().clone())
        } else {
            calling
        }
    }

    /// `call` is the one of this number (the capability was made for it).
    fn held(&self, call: &OwnedCall) -> Result<CallId, ServiceError> {
        if &call.phone_number_id != self.number.phone_number_id()
            || &call.tenant != self.caller.tenant()
        {
            return Err(ServiceError::not_found());
        }
        Ok(call.call.clone())
    }

    /// The caller's audit event `action`.
    fn audit(&self, action: &'static str, facts: CallFacts<'_>, outcome: Outcome) {
        calls_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.number.waba_id().as_str()),
                phone_number_id: Some(self.number.phone_number_id().as_str()),
                ..Subject::default()
            },
            facts,
            outcome,
        );
    }

    /// The error of a change that failed, audited as `action` with an
    /// unknown outcome when it may have gone through.
    async fn change_failed(
        &self,
        action: &'static str,
        facts: CallFacts<'_>,
        error: &Error,
    ) -> ServiceError {
        let failed = self.failed(error).await;
        if failed.may_have_been_sent() {
            self.audit(action, facts, Outcome::Unanswered);
        }
        failed
    }

    /// The number's calling settings, the SIP server's password never
    /// asked for and never answered.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its code (no text).
    pub async fn settings(&self) -> Result<Settings, ServiceError> {
        match self.client().settings(false).await {
            Ok(settings) => Ok(Settings {
                calling: without_sip_passwords(settings.unwrap_or_default()),
            }),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Change the settings with `change` ([`settings_change`]). A voicemail
    /// announcement's media id must be the number's own media: it is looked
    /// up with `phone_number_id={pn}` first, and Meta refusing it (another
    /// number's, none) is `404 not_found` with nothing changed. The
    /// library marks the call idempotent (it sets values), so a timeout is
    /// replayed. The caller's audit event `call_settings_updated` names the
    /// sections changed, never their values.
    ///
    /// # Errors
    ///
    /// `404 not_found` for a media id Meta does not show as the number's;
    /// `422 invalid_request` on what the library checks before it sends
    /// (the call hours' windows, the SIP server, the voicemail's
    /// announcement and trigger); Meta's refusal or failure.
    pub async fn update_settings(&self, change: &SettingsChange) -> Result<(), ServiceError> {
        if let Some(media) = change.announcement_media() {
            let id: MediaId = typed_graph_id(
                "voicemail.audio.default.announcement_media_id",
                media.as_str(),
            )?;
            let found = self
                .number
                .client()
                .media(self.number.phone_number_id().clone())
                .restrict_to_phone_number()
                .url(&id)
                .await;
            match found {
                Ok(info) if info.id == id => {}
                Ok(_) => return Err(ServiceError::not_found()),
                Err(error) => {
                    let api = self.failed(&error).await;
                    return Err(if object_refused(&error) {
                        ServiceError::not_found()
                    } else {
                        api
                    });
                }
            }
        }
        let names = sections(&change.0).join(",");
        let facts = CallFacts {
            sections: Some(&names),
            ..CallFacts::default()
        };
        match self.client().update_settings(&change.0).await {
            Ok(()) => {
                self.audit(SETTINGS_UPDATED, facts, Outcome::Answered);
                Ok(())
            }
            Err(error) => Err(self.change_failed(SETTINGS_UPDATED, facts, &error).await),
        }
    }

    /// Whether the business may call `user`, and what it may do next.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its code (no text).
    pub async fn permissions(&self, user: &Recipient) -> Result<Permissions, ServiceError> {
        match self.client().permissions(user).await {
            Ok(permissions) => Ok(Permissions::from(permissions)),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The number's SIP servers **with their passwords** (design D410,
    /// roadmap M5e2): `GET /{pn}/settings?include_sip_credentials=true`,
    /// the one place the service sends that parameter. The password is
    /// returned to the caller and nowhere else: not cached, not logged, in
    /// no audit event, no error and no idempotency record (a `GET` has
    /// none). The caller's audit event `call_sip_credentials_read` says
    /// who read which number's, never a host or a password. A failure
    /// answers Meta's code and the service's kind, never Meta's text.
    ///
    /// The adapter calls this only for a key holding the scope
    /// `call_credentials` ([`crate::model::Scope::CallCredentials`]); the
    /// deployment's setting is checked here, **before the token is opened**.
    ///
    /// # Errors
    ///
    /// `403 feature_not_available` unless [`Self::with_sip_credentials`]
    /// (nothing is asked of Meta); Meta's refusal or failure.
    pub async fn sip_credentials(&self) -> Result<SipCredentials, ServiceError> {
        if !self.sip_credentials {
            return Err(ServiceError::new(
                crate::error::ErrorCode::FEATURE_NOT_AVAILABLE,
            ));
        }
        match self.client().settings(true).await {
            Ok(settings) => {
                let sip = settings.unwrap_or_default().sip;
                let credentials = SipCredentials {
                    status: sip
                        .as_ref()
                        .and_then(|s| s.status.as_ref())
                        .map(|s| s.as_str().to_owned()),
                    servers: sip
                        .into_iter()
                        .flat_map(|s| s.servers.unwrap_or_default())
                        .map(|server| SipCredential {
                            hostname: server.hostname,
                            port: server.port,
                            password: server.sip_user_password,
                        })
                        .collect(),
                };
                self.audit(
                    SIP_CREDENTIALS_READ,
                    CallFacts {
                        sections: Some("sip"),
                        ..CallFacts::default()
                    },
                    Outcome::Answered,
                );
                Ok(credentials)
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// A transfer slot of the number's tenant's share of `transfers` (the
    /// adapter's media pool), to be taken **before the announcement's body
    /// is read**: the body is held in memory, at most
    /// [`VOICEMAIL_MAX_BYTES`].
    ///
    /// # Errors
    ///
    /// `429 too_many_requests` (`Retry-After: 1`): the tenant's share, or
    /// the pool, is taken.
    pub fn transfer_slot(&self, transfers: &Slots) -> Result<Slot, ServiceError> {
        transfers
            .try_acquire(self.number.tenant())
            .ok_or_else(|| ServiceError::too_many_requests(1))
    }

    /// Upload `audio` as a voicemail announcement
    /// (`POST /{pn}/media` with `use_case=call_voicemail_announcement`,
    /// Ogg/Opus). The media is the number's own; its id is what the
    /// settings' `announcement_media_id` takes. Not replayed on a timeout
    /// (a replay would upload twice): the adapter takes an
    /// `Idempotency-Key`. The caller's audit event
    /// `call_voicemail_announcement_uploaded` holds no byte of the file.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure (its code, never its text); a timeout is
    /// audited as unknown.
    pub async fn upload_voicemail_announcement(
        &self,
        audio: &VoicemailAudio,
    ) -> Result<AnnouncementUploaded, ServiceError> {
        let media = self
            .number
            .client()
            .media(self.number.phone_number_id().clone());
        let uploaded = media
            .upload_with(
                audio.data.clone(),
                "audio/ogg",
                "announcement.ogg",
                &UploadOptions::voicemail_announcement(),
            )
            .await;
        match uploaded {
            Ok(id) => {
                self.audit(VOICEMAIL_UPLOADED, CallFacts::default(), Outcome::Answered);
                Ok(AnnouncementUploaded {
                    media_id: id.into_inner(),
                })
            }
            Err(error) => Err(self
                .change_failed(VOICEMAIL_UPLOADED, CallFacts::default(), &error)
                .await),
        }
    }

    /// Place a call to `request.to` with the caller's SDP offer. Not
    /// replayed on a timeout (a replay would ring the user twice): the
    /// adapter takes an `Idempotency-Key`. The call becomes the number's
    /// here, so its actions find it; its webhooks make the record again.
    /// The caller's audit event `call_connect_requested` says whether
    /// recording or transcription was enabled, never who is called.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `to`, `sdp_offer`, `biz_opaque_callback_data`,
    /// `recording` or `transcription`, before any request; Meta's refusal or
    /// failure, answered as its kind (`call_permission_missing` for `138006`,
    /// `call_limit_reached` for the limits, ...), never with Meta's text.
    pub async fn connect(&self, request: &ConnectRequest) -> Result<Started, ServiceError> {
        let to = callee(request.to.clone(), "to.", "to")?;
        sdp("sdp_offer", &request.sdp_offer)?;
        opaque(request.biz_opaque_callback_data.as_deref())?;
        let recording = request
            .recording
            .as_ref()
            .map(Announcement::recording)
            .transpose()?;
        let transcription = request
            .transcription
            .as_ref()
            .map(Announcement::transcription)
            .transpose()?;
        let facts = CallFacts {
            recording: recording
                .as_ref()
                .is_some_and(|r| r.status == FeatureStatus::Enabled),
            transcription: transcription
                .as_ref()
                .is_some_and(|t| t.status == FeatureStatus::Enabled),
            ..CallFacts::default()
        };
        let mut call = ConnectCall::new(to, request.sdp_offer.clone());
        call.biz_opaque_callback_data
            .clone_from(&request.biz_opaque_callback_data);
        call.recording = recording;
        call.transcription = transcription;
        match self.client().connect(&call).await {
            Ok(started) => {
                self.audit(CALL_REQUESTED, facts, Outcome::Answered);
                let Some(id) = started.call_id() else {
                    // Meta answered without the call: it may be ringing.
                    return Err(ServiceError::new(crate::error::ErrorCode::UPSTREAM)
                        .with_may_have_been_sent(true));
                };
                // The record is best effort: the call is placed, and its
                // webhooks make the record again.
                if let Err(error) = self
                    .records
                    .record(self.caller.tenant(), self.number.phone_number_id(), id)
                    .await
                {
                    tracing::warn!(
                        target: LOG_TARGET,
                        error = %error,
                        "a placed call could not be recorded; its webhooks will record it"
                    );
                }
                Ok(Started {
                    call_id: id.as_str().to_owned(),
                })
            }
            Err(error) => Err(self.change_failed(CALL_REQUESTED, facts, &error).await),
        }
    }

    /// Pre-accept the user's call with the caller's SDP answer, so that
    /// media can connect before [`Self::accept`].
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `sdp_answer`, before any request; Meta's
    /// refusal or failure.
    pub async fn pre_accept(&self, call: &OwnedCall, sdp_answer: &str) -> Result<(), ServiceError> {
        let id = self.held(call)?;
        sdp("sdp_answer", sdp_answer)?;
        match self.client().pre_accept(&id, sdp_answer).await {
            Ok(()) => {
                self.audit(CALL_PRE_ACCEPTED, CallFacts::default(), Outcome::Answered);
                Ok(())
            }
            Err(error) => Err(self
                .change_failed(CALL_PRE_ACCEPTED, CallFacts::default(), &error)
                .await),
        }
    }

    /// Accept the user's call with the caller's SDP answer (the one given
    /// to [`Self::pre_accept`], if any).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `sdp_answer`, `biz_opaque_callback_data`,
    /// `recording` or `transcription`, before any request; Meta's refusal
    /// or failure.
    pub async fn accept(
        &self,
        call: &OwnedCall,
        request: &AcceptRequest,
    ) -> Result<(), ServiceError> {
        let id = self.held(call)?;
        sdp("sdp_answer", &request.sdp_answer)?;
        opaque(request.biz_opaque_callback_data.as_deref())?;
        let recording = request
            .recording
            .as_ref()
            .map(Announcement::recording)
            .transpose()?;
        let transcription = request
            .transcription
            .as_ref()
            .map(Announcement::transcription)
            .transpose()?;
        let facts = CallFacts {
            recording: recording
                .as_ref()
                .is_some_and(|r| r.status == FeatureStatus::Enabled),
            transcription: transcription
                .as_ref()
                .is_some_and(|t| t.status == FeatureStatus::Enabled),
            ..CallFacts::default()
        };
        let options = AcceptOptions {
            biz_opaque_callback_data: request.biz_opaque_callback_data.clone(),
            recording,
            transcription,
        };
        match self
            .client()
            .accept_with(&id, &request.sdp_answer, &options)
            .await
        {
            Ok(()) => {
                self.audit(CALL_ACCEPTED, facts, Outcome::Answered);
                Ok(())
            }
            Err(error) => Err(self.change_failed(CALL_ACCEPTED, facts, &error).await),
        }
    }

    /// Reject the user's call.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure.
    pub async fn reject(&self, call: &OwnedCall) -> Result<(), ServiceError> {
        let id = self.held(call)?;
        match self.client().reject(&id).await {
            Ok(()) => {
                self.audit(CALL_REJECTED, CallFacts::default(), Outcome::Answered);
                Ok(())
            }
            Err(error) => Err(self
                .change_failed(CALL_REJECTED, CallFacts::default(), &error)
                .await),
        }
    }

    /// End the call, in either direction.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure.
    pub async fn terminate(&self, call: &OwnedCall) -> Result<(), ServiceError> {
        let id = self.held(call)?;
        match self.client().terminate(&id).await {
            Ok(()) => {
                self.audit(CALL_TERMINATED, CallFacts::default(), Outcome::Answered);
                Ok(())
            }
            Err(error) => Err(self
                .change_failed(CALL_TERMINATED, CallFacts::default(), &error)
                .await),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a panic is the report
mod tests {
    use serde_json::json;

    use super::*;

    fn field(error: &ServiceError) -> String {
        error.field().unwrap_or("?").to_owned()
    }

    fn who(phone: Option<&str>, user: Option<&str>) -> Callee {
        Callee {
            phone: phone.map(str::to_owned),
            user_id: user.map(str::to_owned),
        }
    }

    /// A callee is a phone number and/or a BSUID in the shapes Meta
    /// documents, the phone number winning when both are given (the
    /// library sends what Meta takes). Decisive: each clause, and the
    /// field named is the one at fault, never the value.
    #[test]
    fn a_callee_is_a_phone_number_and_or_a_bsuid() {
        let ok = |c| callee(c, "to.", "to");
        assert!(matches!(
            ok(who(Some("+16505551234"), None)),
            Ok(Recipient::Phone(_))
        ));
        assert!(matches!(
            ok(who(None, Some("US.13491208655302741918"))),
            Ok(Recipient::User(_))
        ));
        // A parent BSUID is taken (the page says so).
        assert!(matches!(
            ok(who(None, Some("US.ENT.11815799212886844830"))),
            Ok(Recipient::User(_))
        ));
        assert!(matches!(
            ok(who(Some("+16505551234"), Some("US.1"))),
            Ok(Recipient::PhoneAndUser { .. })
        ));
        assert_eq!(field(&ok(who(None, None)).unwrap_err()), "to");
        assert_eq!(
            field(&ok(who(Some("16505551234"), None)).unwrap_err()),
            "to.phone",
            "the + is required"
        );
        for bad in ["us.1", "US.", "US.ENT.", "USA.1", "US.a b", "1234567890"] {
            assert_eq!(
                field(&ok(who(None, Some(bad))).unwrap_err()),
                "to.user_id",
                "{bad}"
            );
        }
        assert_eq!(
            field(&callee(who(Some("x"), None), "", "user").unwrap_err()),
            "phone"
        );
    }

    /// An SDP is not blank and at most 32 KiB, and no error quotes it.
    #[test]
    fn an_sdp_is_bounded_and_never_quoted() {
        assert!(sdp("sdp_offer", "v=0\r\n").is_ok());
        for bad in ["", "  \r\n", &"a".repeat(SDP_MAX_BYTES + 1), "v=0\0"] {
            let error = sdp("sdp_offer", bad).unwrap_err();
            assert_eq!(field(&error), "sdp_offer");
        }
        assert!(sdp("sdp_offer", &"a".repeat(SDP_MAX_BYTES)).is_ok());
    }

    /// A settings change reads Meta's `calling` object, refuses a value the
    /// pages do not list and an empty change. Decisive: each enum, and
    /// `body` for a key the object does not have is the library's lenience
    /// closed by the adapter's strict body (here: a value of the wrong type).
    #[test]
    fn a_settings_change_names_something_in_the_pages_values() {
        let ok = |v| settings_change(v);
        let change = ok(
            json!({"status": "ENABLED", "call_icon_visibility": "DEFAULT",
            "callback_permission_status": "DISABLED",
            "audio": {"additional_codecs": ["PCMA"]}}),
        )
        .unwrap();
        assert_eq!(
            sections(&change.0),
            [
                "status",
                "call_icon_visibility",
                "callback_permission_status",
                "audio"
            ]
        );
        assert_eq!(field(&ok(json!({})).unwrap_err()), "update");
        assert_eq!(field(&ok(json!({"status": "ON"})).unwrap_err()), "status");
        assert_eq!(
            field(&ok(json!({"call_icon_visibility": "HIDE"})).unwrap_err()),
            "call_icon_visibility"
        );
        assert_eq!(
            field(&ok(json!({"audio": {"additional_codecs": ["OPUS"]}})).unwrap_err()),
            "audio.additional_codecs"
        );
        assert_eq!(
            field(
                &ok(json!({"voicemail": {"status": "ENABLED", "triggers": ["BUSY"]}})).unwrap_err()
            ),
            "voicemail.triggers"
        );
        assert_eq!(field(&ok(json!({"call_hours": 3})).unwrap_err()), "body");
        assert_eq!(field(&ok(json!({"status": 3})).unwrap_err()), "status");
        // The media id of a voicemail announcement is found for the lookup.
        let change = ok(
            json!({"voicemail": {"status": "ENABLED", "triggers": ["REJECT"],
            "audio": {"default": {"announcement_media_id": "1037543291543636"}}}}),
        )
        .unwrap();
        assert_eq!(
            change.announcement_media().map(MediaId::as_str),
            Some("1037543291543636")
        );
        // Debug names sections, never values.
        let shown = format!("{change:?}");
        assert!(shown.contains("voicemail") && !shown.contains("1037543291543636"));
    }

    /// A SIP server's password never leaves the core: the library keeps it
    /// off the wire when serializing, so only the field itself shows the
    /// drop. Decisive: delete the loop of `without_sip_passwords` and the
    /// assertion fails, whatever the adapter's view holds.
    #[test]
    fn a_sip_servers_password_is_dropped_from_the_settings() {
        let settings: CallingSettings = serde_json::from_value(json!({
            "status": "ENABLED",
            "sip": {"status": "ENABLED", "servers": [
                {"hostname": "sip.example.com", "port": 5061, "sip_user_password": "hunter2-secret"},
                {"hostname": "sip2.example.com", "sip_user_password": "other-secret"}]}
        }))
        .unwrap();
        let servers = |s: &CallingSettings| s.sip.as_ref().unwrap().servers.clone().unwrap();
        assert!(
            servers(&settings)
                .iter()
                .all(|s| s.sip_user_password.is_some()),
            "the fixture carries passwords"
        );
        let kept = without_sip_passwords(settings);
        let after = servers(&kept);
        assert_eq!(after.len(), 2, "the servers stay");
        assert!(after.iter().all(|s| s.sip_user_password.is_none()));
        assert_eq!(after[0].hostname, "sip.example.com");
        // A change with no SIP section is untouched.
        let plain = without_sip_passwords(CallingSettings::default());
        assert!(plain.sip.is_none());
    }

    /// A SIP server read with its password prints neither the host nor the
    /// password in `Debug`, whether alone or in the set (roadmap M5e2,
    /// D410): a stray `{:?}` of the credentials in a log line leaks
    /// nothing. Decisive: the manual `Debug` impls (derive them and the
    /// password, redacted by the library, still hides, but the host shows).
    #[test]
    fn sip_credentials_print_nothing_of_the_server_or_its_password() {
        let settings: CallingSettings = serde_json::from_value(json!({
            "sip": {"status": "ENABLED", "servers": [
                {"hostname": "sip.secret-host.example", "sip_user_password": "hunter2-secret"}]}
        }))
        .unwrap();
        let server = settings.sip.unwrap().servers.unwrap().remove(0);
        let credential = SipCredential {
            hostname: server.hostname,
            port: server.port,
            password: server.sip_user_password,
        };
        assert_eq!(credential.password(), Some("hunter2-secret"));
        let set = SipCredentials {
            status: Some("ENABLED".into()),
            servers: vec![credential.clone()],
        };
        for shown in [format!("{credential:?}"), format!("{set:?}")] {
            assert!(!shown.contains("hunter2"), "{shown}");
            assert!(!shown.contains("secret-host"), "{shown}");
        }
    }

    /// A voicemail announcement is Ogg with Opus by its own first bytes, not
    /// empty and at most 16 MiB: each clause refused with the field named
    /// (`file`) and `Debug` shows no byte (roadmap M5e2, D411).
    #[test]
    fn a_voicemail_announcement_is_ogg_opus_by_its_bytes() {
        let ogg = |tail: &[u8]| {
            let mut data = b"OggS\x00\x02".to_vec();
            data.extend([0u8; 20]);
            data.extend(b"\x01\x13OpusHead");
            data.extend(tail);
            bytes::Bytes::from(data)
        };
        let good = VoicemailAudio::new(ogg(b"audio-bytes-9Q")).unwrap();
        assert!(!format!("{good:?}").contains("audio-bytes"));
        assert_eq!(good.data().len(), 6 + 20 + 10 + 14);
        for (what, data) in [
            ("empty", bytes::Bytes::new()),
            ("mp3", bytes::Bytes::from_static(b"ID3\x04")),
            ("vorbis", bytes::Bytes::from_static(b"OggS\x00\x02vorbis")),
            ("opus without ogg", bytes::Bytes::from_static(b"OpusHead")),
        ] {
            let error = VoicemailAudio::new(data).unwrap_err();
            assert_eq!(field(&error), "file", "{what}");
        }
        let mut far = b"OggS".to_vec();
        far.extend([0u8; 200]);
        far.extend(b"OpusHead");
        assert!(VoicemailAudio::new(bytes::Bytes::from(far)).is_err());
        let mut big = ogg(b"").to_vec();
        big.resize(usize::try_from(VOICEMAIL_MAX_BYTES).unwrap() + 1, 0);
        let error = VoicemailAudio::new(bytes::Bytes::from(big)).unwrap_err();
        assert_eq!(
            error.code(),
            crate::error::ErrorCode::MEDIA_TOO_LARGE.as_str()
        );
    }

    /// The record key is pinned (a stored name: a change orphans the
    /// records), differs by number and by call, and hides the call's id.
    #[test]
    fn the_namespace_and_the_record_key_are_pinned() {
        assert_eq!(NAMESPACE, "wa.server.call");
        let number = PhoneNumberId::new("106540352242922").unwrap();
        let call = CallId::new("wacid.ABGGFjFVU2AfAgo6V-Hc5eCgK5Gh");
        let key = CallRecords::key(&number, &call);
        assert!(key.starts_with("106540352242922."));
        assert!(!key.contains("ABGG"));
        assert_eq!(key.len(), 15 + 1 + 64);
        assert_ne!(
            key,
            CallRecords::key(&PhoneNumberId::new("106540352242923").unwrap(), &call)
        );
        assert_ne!(key, CallRecords::key(&number, &CallId::new("wacid.other")));
        assert!(call_id("").is_none());
        assert!(call_id("a b").is_none());
        assert!(call_id("é").is_none());
        assert!(call_id(&"a".repeat(MAX_ID_CHARS + 1)).is_none());
        assert!(call_id("wacid.ABGGFjFVU2AfAgo6V-Hc5eCgK5Gh").is_some());
    }

    /// The recording and the transcription are the caller's: a status
    /// other than the two Meta lists is refused, and nothing is defaulted.
    #[test]
    fn an_announcement_status_is_enabled_or_disabled() {
        let announcement = |status: &str| Announcement {
            status: status.to_owned(),
            purpose: Some("quality".to_owned()),
            announcement_language: Some("en_US".to_owned()),
        };
        assert!(announcement("ENABLED").recording().is_ok());
        assert!(announcement("DISABLED").transcription().is_ok());
        assert_eq!(
            field(&announcement("enabled").recording().unwrap_err()),
            "recording.status"
        );
        assert_eq!(
            field(&announcement("ON").transcription().unwrap_err()),
            "transcription.status"
        );
    }

    /// What a webhook event does to the records: a call event and a call
    /// status each make the call the tenant's number's for a day, every
    /// event renews the day, a day after the last event the call is gone,
    /// and no other event writes anything. Decisive: both arms of
    /// `CallRecords::on_event` and `RECORD_TTL` (make it `Expiry::Never`
    /// and the last assertion fails).
    #[tokio::test]
    async fn an_event_records_a_call_for_a_day_and_each_event_renews_it() {
        use meta_whatsapp_rs::adapters::store::MemoryKvStore;
        use meta_whatsapp_rs::core::clock::ManualClock;
        use meta_whatsapp_rs::core::ids::WabaId;
        use meta_whatsapp_rs::webhooks::fields::{Call, CallStatus};
        let clock = ManualClock::new(time::macros::datetime!(2026-11-05 12:00 UTC));
        let records =
            CallRecords::new(Arc::new(MemoryKvStore::with_clock(Arc::new(clock.clone()))));
        let tenant = TenantId::parse("merchant-42").unwrap();
        let pn = PhoneNumberId::new("106540352242922").unwrap();
        let (call, other) = (
            CallId::new("wacid.ABGGFjFVU2AfAgo6V"),
            CallId::new("wacid.HBgLMTIxODU1NTI4MjgV"),
        );
        let waba = WabaId::new("102290129340398").unwrap();
        let updated = |id: &CallId| {
            let call: Call = serde_json::from_value(serde_json::json!({
                "id": id.as_str(), "event": "connect", "timestamp": "1671644824",
                "direction": "USER_INITIATED"}))
            .unwrap();
            WebhookEvent::CallUpdated {
                waba_id: waba.clone(),
                phone_number_id: pn.clone(),
                display_phone_number: "15550783881".to_owned(),
                contact: None,
                call: Box::new(call),
            }
        };
        let status = |id: &CallId| {
            let status: CallStatus = serde_json::from_value(serde_json::json!({
                "id": id.as_str(), "type": "call", "status": "RINGING", "timestamp": "1671644824"}))
            .unwrap();
            WebhookEvent::CallStatusUpdated {
                waba_id: waba.clone(),
                phone_number_id: pn.clone(),
                display_phone_number: "15550783881".to_owned(),
                contact: None,
                status: Box::new(status),
            }
        };
        let held = |id: &CallId| {
            let (records, key) = (records.clone(), CallRecords::key(&pn, id));
            async move { records.store.get(&key).await.unwrap().is_some() }
        };
        // Neither a message nor an unknown field writes anything.
        let other_event = WebhookEvent::Unknown {
            waba_id: waba.clone(),
            field: "a_field_meta_adds_later".to_owned(),
            time: None,
            raw: serde_json::json!({}),
            parse_error: None,
        };
        records.on_event(&tenant, &other_event).await.unwrap();
        assert!(!held(&call).await && !held(&other).await);
        // Each arm records its call, and only that call.
        records.on_event(&tenant, &updated(&call)).await.unwrap();
        records.on_event(&tenant, &status(&other)).await.unwrap();
        assert!(held(&call).await && held(&other).await);
        let (record, _, _) = records
            .store
            .get(&CallRecords::key(&pn, &call))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (record.tenant.as_str(), record.phone_number_id.as_str()),
            ("merchant-42", "106540352242922")
        );
        // A day on, an event renews its call; the other one is gone.
        clock.advance(Duration::from_hours(23));
        records.on_event(&tenant, &status(&call)).await.unwrap();
        clock.advance(Duration::from_hours(2));
        assert!(held(&call).await, "renewed by the event");
        assert!(!held(&other).await, "a day since its last event");
        clock.advance(Duration::from_hours(23));
        assert!(!held(&call).await, "a day since its last event");
        // A number that is not a bound number's shape is never keyed.
        let odd = PhoneNumberId::new_unchecked("106540352242922.abc");
        records.record(&tenant, &odd, &call).await.unwrap();
        assert!(
            records
                .store
                .get(&CallRecords::key(&odd, &call))
                .await
                .unwrap()
                .is_none()
        );
    }
}
