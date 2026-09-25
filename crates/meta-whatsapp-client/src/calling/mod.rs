//! Calling API signalling: calling settings, call permissions, and the call
//! actions `connect`, `pre_accept`, `accept`, `reject`, `terminate`.
//!
//! Docs:
//! - `calling` — overview and limits.
//! - `calling/call-settings`, `calling/reference` — `GET`/`POST
//!   /{phone-number-id}/settings` with the `calling` object (see
//!   [`settings`]); `calling/sip` for its `sip` sub-object.
//! - `calling/business-initiated-calls` — `connect` and `terminate`.
//! - `calling/user-initiated-calls` — `pre_accept`, `accept`, `reject`,
//!   `terminate`.
//! - `calling/user-call-permissions` — `GET
//!   /{phone-number-id}/call_permissions`. Sending the permission-request
//!   *message* is a send (`crate::messages`), not signalling.
//! - `reference/whatsapp-business-phone-number/calling-api` — `POST
//!   /{phone-number-id}/calls` and `call_permissions`.
//! - `business-scoped-user-ids#calling-api` — calling and permission lookup
//!   by BSUID (`recipient`).
//!
//! Every call action is a `POST /{phone-number-id}/calls` with an `action`.
//! SDP is opaque here: it is passed through unparsed (RFC 8866; WebRTC
//! media is out of scope). Call events arrive as `calls` webhooks
//! (`meta-whatsapp-webhooks`). The reference's enum also lists `media_update`, which
//! no page documents; it is not wrapped.
//!
//! Limits enforced locally: `biz_opaque_callback_data` ≤ 512 characters,
//! non-empty SDP and call ids, no groups (calling is not supported in
//! groups), and the call-settings limits listed in [`settings`].
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{CallId, PhoneNumberId, UserId};
use meta_whatsapp_core::recipient::Recipient;

use crate::Client;

pub mod settings;
#[cfg(test)]
mod tests;

pub use settings::CallingSettings;

/// Longest `biz_opaque_callback_data`, in characters.
pub const BIZ_OPAQUE_CALLBACK_DATA_MAX_CHARS: usize = 512;

/// Entry point, see [`Client::calling`].
#[derive(Debug, Clone)]
pub struct Calling {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Calling`] API for `phone_number_id`.
    pub fn calling(&self, phone_number_id: impl Into<PhoneNumberId>) -> Calling {
        Calling {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl Calling {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Calling settings: `GET /{phone-number-id}/settings`, the `calling`
    /// object (`calling/call-settings#get-phone-number-calling-settings`).
    /// The endpoint also returns non-calling settings, which are ignored
    /// here. `None` when the answer has no `calling` object.
    ///
    /// With `include_sip_credentials`, SIP servers carry the Meta-generated
    /// digest password ([`settings::SipServer::sip_user_password`]).
    pub async fn settings(&self, include_sip_credentials: bool) -> Result<Option<CallingSettings>> {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            calling: Option<CallingSettings>,
        }
        let env: Envelope = self
            .client
            .get_at(&self.segments("settings"))
            .query_opt(
                "include_sip_credentials",
                include_sip_credentials.then_some(true),
            )
            .context("calling settings response")
            .send()
            .await?;
        Ok(env.calling)
    }

    /// Update calling settings: `POST /{phone-number-id}/settings` with
    /// `{"calling": …}` (`calling/call-settings#configure-or-update-business-phone-number-calling-settings`).
    /// Only the set fields are sent; `call_hours` is replaced as a whole.
    /// Users' apps may take up to 7 days to reflect a change.
    ///
    /// Marked idempotent: it sets fields to values.
    pub async fn update_settings(&self, calling: &CallingSettings) -> Result<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            calling: &'a CallingSettings,
        }
        calling.validate()?;
        self.client
            .post_at(&self.segments("settings"))
            .json(&Body { calling })
            .idempotent(true)
            .context("update calling settings response")
            .send_success()
            .await
    }

    /// Whether you may call `user`, and what you may do next:
    /// `GET /{phone-number-id}/call_permissions`
    /// (`calling/user-call-permissions#get-current-call-permission-state`).
    ///
    /// A phone number is sent as `user_wa_id`, a BSUID (or parent BSUID) as
    /// `recipient`. For [`Recipient::PhoneAndUser`] only `user_wa_id` is
    /// sent: the page documents one or the other, and elsewhere Meta lets
    /// the phone number win.
    pub async fn permissions(&self, user: &Recipient) -> Result<CallPermissions> {
        let (user_wa_id, recipient) = match user {
            Recipient::Phone(phone) | Recipient::PhoneAndUser { phone, .. } => {
                (Some(phone.as_str()), None)
            }
            Recipient::User(id) => (None, Some(id.as_str())),
            _ => return Err(not_an_individual("user")),
        };
        self.client
            .get_at(&self.segments("call_permissions"))
            .query_opt("user_wa_id", user_wa_id)
            .query_opt("recipient", recipient)
            .context("call permissions response")
            .send()
            .await
    }

    /// Start a business-initiated call with an SDP offer: `action:
    /// "connect"` (`calling/business-initiated-calls#initiate-call`). You
    /// need the user's call permission first.
    ///
    /// Not idempotent: a replay would ring the user twice.
    pub async fn connect(&self, call: &ConnectCall) -> Result<CallStarted> {
        let (to, recipient) = match &call.to {
            Recipient::Phone(phone) => (Some(phone.as_str()), None),
            Recipient::User(id) => (None, Some(id)),
            Recipient::PhoneAndUser { phone, user } => (Some(phone.as_str()), Some(user)),
            _ => return Err(not_an_individual("to")),
        };
        let body = CallsBody {
            to,
            recipient,
            session: Some(Session::new(SdpType::Offer, &call.sdp_offer)?),
            biz_opaque_callback_data: opaque(call.biz_opaque_callback_data.as_deref())?,
            ..CallsBody::new(Action::Connect)
        };
        self.client
            .post_at(&self.segments("calls"))
            .json(&body)
            .context("connect call response")
            .send()
            .await
    }

    /// Pre-accept an incoming call with your SDP answer so media can
    /// connect before you [`Self::accept`] (recommended; avoids clipped
    /// audio): `action: "pre_accept"`
    /// (`calling/user-initiated-calls#pre-accept-call`).
    pub async fn pre_accept(&self, call_id: &CallId, sdp_answer: &str) -> Result<()> {
        let body = CallsBody {
            call_id: Some(check_call_id(call_id)?),
            session: Some(Session::new(SdpType::Answer, sdp_answer)?),
            ..CallsBody::new(Action::PreAccept)
        };
        self.send_action(&body, "pre-accept call response").await
    }

    /// Accept an incoming call within 30–60 s of the connect webhook:
    /// `action: "accept"` (`calling/user-initiated-calls#accept-call`). The
    /// SDP answer must match the one given to [`Self::pre_accept`], if any.
    /// Start sending media only after this returns.
    pub async fn accept(
        &self,
        call_id: &CallId,
        sdp_answer: &str,
        biz_opaque_callback_data: Option<&str>,
    ) -> Result<()> {
        let body = CallsBody {
            call_id: Some(check_call_id(call_id)?),
            session: Some(Session::new(SdpType::Answer, sdp_answer)?),
            biz_opaque_callback_data: opaque(biz_opaque_callback_data)?,
            ..CallsBody::new(Action::Accept)
        };
        self.send_action(&body, "accept call response").await
    }

    /// Reject an incoming call: `action: "reject"`
    /// (`calling/user-initiated-calls#reject-call`).
    pub async fn reject(&self, call_id: &CallId) -> Result<()> {
        let body = CallsBody {
            call_id: Some(check_call_id(call_id)?),
            ..CallsBody::new(Action::Reject)
        };
        self.send_action(&body, "reject call response").await
    }

    /// End an active call, in either direction: `action: "terminate"`
    /// (`calling/business-initiated-calls#terminate-call`). Do it even after
    /// an RTCP BYE, for accurate pricing.
    pub async fn terminate(&self, call_id: &CallId) -> Result<()> {
        let body = CallsBody {
            call_id: Some(check_call_id(call_id)?),
            ..CallsBody::new(Action::Terminate)
        };
        self.send_action(&body, "terminate call response").await
    }

    /// `[phone-number-id, edge]`; the id stays one segment whatever it
    /// contains.
    fn segments<'a>(&'a self, edge: &'a str) -> [&'a str; 2] {
        [self.phone_number_id.as_str(), edge]
    }

    async fn send_action(&self, body: &CallsBody<'_>, context: &'static str) -> Result<()> {
        self.client
            .post_at(&self.segments("calls"))
            .json(body)
            .context(context)
            .send_success()
            .await
    }
}

fn not_an_individual(field: &str) -> meta_whatsapp_core::Error {
    ValidationError::new(
        field,
        "calls need a user (phone number and/or BSUID); the Calling API is not supported in groups",
    )
    .into()
}

fn check_call_id(call_id: &CallId) -> Result<&CallId> {
    if call_id.as_str().trim().is_empty() {
        return Err(ValidationError::new("call_id", "must not be empty").into());
    }
    Ok(call_id)
}

fn opaque(data: Option<&str>) -> Result<Option<&str>> {
    if let Some(d) = data {
        let len = d.chars().count();
        if len > BIZ_OPAQUE_CALLBACK_DATA_MAX_CHARS {
            return Err(ValidationError::new(
                "biz_opaque_callback_data",
                format!(
                    "must be at most {BIZ_OPAQUE_CALLBACK_DATA_MAX_CHARS} characters, got {len}"
                ),
            )
            .into());
        }
    }
    Ok(data)
}

#[derive(Serialize)]
struct CallsBody<'a> {
    messaging_product: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recipient: Option<&'a UserId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_id: Option<&'a CallId>,
    action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<Session<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    biz_opaque_callback_data: Option<&'a str>,
}

impl CallsBody<'_> {
    fn new(action: Action) -> Self {
        Self {
            messaging_product: "whatsapp",
            to: None,
            recipient: None,
            call_id: None,
            action,
            session: None,
            biz_opaque_callback_data: None,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Connect,
    PreAccept,
    Accept,
    Reject,
    Terminate,
}

#[derive(Serialize)]
struct Session<'a> {
    sdp_type: SdpType,
    sdp: &'a str,
}

impl<'a> Session<'a> {
    fn new(sdp_type: SdpType, sdp: &'a str) -> Result<Self> {
        if sdp.trim().is_empty() {
            return Err(ValidationError::new("session.sdp", "must not be empty").into());
        }
        Ok(Self { sdp_type, sdp })
    }
}

/// `offer` for `connect`, `answer` for `pre_accept`/`accept` (the calling
/// reference schema and every example; one table row says "offer" for all).
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum SdpType {
    Offer,
    Answer,
}

/// Arguments of [`Calling::connect`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectCall {
    /// Callee: a phone number (`to`), a BSUID or parent BSUID
    /// (`recipient`), or both (Meta then uses the phone number).
    pub to: Recipient,
    /// Your SDP offer (RFC 8866), passed through unparsed.
    pub sdp_offer: String,
    /// Up to 512 characters echoed in the call terminate webhook.
    pub biz_opaque_callback_data: Option<String>,
}

impl ConnectCall {
    /// A call to `to` with `sdp_offer`.
    pub fn new(to: impl Into<Recipient>, sdp_offer: impl Into<String>) -> Self {
        Self {
            to: to.into(),
            sdp_offer: sdp_offer.into(),
            biz_opaque_callback_data: None,
        }
    }
}

/// Answer of [`Calling::connect`] (`CallResponsePayload`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CallStarted {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// The new call (one element).
    #[serde(default)]
    pub calls: Vec<StartedCall>,
}

impl CallStarted {
    /// Id of the call, used by [`Calling::terminate`] and the webhooks.
    pub fn call_id(&self) -> Option<&CallId> {
        self.calls.first().map(|c| &c.id)
    }
}

/// An element of [`CallStarted::calls`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StartedCall {
    /// Call id (`wacid.…`).
    pub id: CallId,
}

/// Answer of [`Calling::permissions`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CallPermissions {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// Current permission.
    pub permission: CallPermission,
    /// What you may do now, with the limits that apply.
    #[serde(default)]
    pub actions: Vec<CallPermissionAction>,
}

/// `permission` of [`CallPermissions`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CallPermission {
    /// Status.
    pub status: CallPermissionStatus,
    /// When a temporary permission expires (absent for permanent ones).
    /// The guide's table calls it `expiration`; both names are read.
    #[serde(
        default,
        alias = "expiration",
        with = "meta_whatsapp_core::timestamp::unix_option"
    )]
    pub expiration_time: Option<OffsetDateTime>,
}

/// Call permission status. The guides document `no_permission`,
/// `temporary`, `permanent`; the endpoint reference lists `granted`,
/// `pending`, `denied`, `expired`. All are known.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPermissionStatus {
    /// `no_permission` (also returned for uncallable numbers).
    NoPermission,
    /// `temporary`: granted for 7 days.
    Temporary,
    /// `permanent`.
    Permanent,
    /// `granted`.
    Granted,
    /// `pending`.
    Pending,
    /// `denied`.
    Denied,
    /// `expired`.
    Expired,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// An element of [`CallPermissions::actions`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CallPermissionAction {
    /// The action.
    pub action_name: CallPermissionActionName,
    /// Whether it is allowed right now, all limits considered.
    pub can_perform_action: bool,
    /// Time-bound limits on it.
    #[serde(default)]
    pub limits: Vec<ActionLimit>,
}

/// Name of a call-permission action.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPermissionActionName {
    /// `start_call`: a new call that the user picks up.
    StartCall,
    /// `send_call_permission_request`.
    SendCallPermissionRequest,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// A limit on an action.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActionLimit {
    /// ISO 8601 duration, e.g. `PT24H`, `P7D`.
    pub time_period: String,
    /// Allowed within the period.
    pub max_allowed: u32,
    /// Used within the period.
    pub current_usage: u32,
    /// When the limit resets; present only once it is reached.
    #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
    pub limit_expiration_time: Option<OffsetDateTime>,
}
