//! Conversation Routing's thread control: `pass`, `take` and `release` a
//! thread.
//!
//! Docs:
//! - `conversation-routing/thread-control` — the three actions, their
//!   parameters, the shared response, the errors and the
//!   `messaging_handovers` events they trigger.
//! - `conversation-routing/overview` — threads, owners and the role
//!   identifiers ([`TargetRole`]).
//! - `conversation-routing/thread-lifecycle` — idle and owned threads,
//!   and why a Service message needs ownership.
//! - `conversation-routing/get-started` § Terms of Service requirement —
//!   thread control works only once the business has accepted the Meta
//!   Business Agents and Platform Terms of Service. Only the business can
//!   accept them, in Meta Business Suite; no call here does.
//!
//! All three actions are `POST /{phone-number-id}/thread_control` with an
//! `action`. One responder owns a thread (a business phone number's
//! conversation with one WhatsApp user) at a time:
//!
//! | Method | `action` | Who may call it | Webhook it triggers |
//! | --- | --- | --- | --- |
//! | [`ThreadControl::pass`] | `pass` | the thread's owner | `control_passed`, to the new owner |
//! | [`ThreadControl::take`] | `take` | the escalation partner only | `control_taken`, to the previous owner |
//! | [`ThreadControl::release`] | `release` | the thread's owner (the escalation partner too must `take` first) | none |
//!
//! The events arrive on the `messaging_handovers` webhook field
//! (`meta_whatsapp_webhooks::fields::MessagingHandoversValue`, normalized
//! to `WebhookEvent::ThreadControlChanged`). The caller of `pass` and
//! `take` hears nothing back, and no endpoint reports a thread's owner: a
//! caller that tracks ownership records its own successful calls
//! (`meta_whatsapp_rs::inbox::Inbox::record_thread_owner` and
//! `record_release`). Roles are the identity here, never app ids: no
//! request field names an app.
//!
//! The WhatsApp user is addressed with [`Recipient`]: a phone number or
//! `wa_id` becomes `to`, a BSUID becomes `recipient` (Meta's preferred
//! identifier, stable across number changes). Meta rejects a request with
//! both or neither, so [`Recipient::PhoneAndUser`] is refused locally, as
//! is a group (a thread is with a WhatsApp user).
//!
//! Limits enforced locally: `metadata` at most
//! [`METADATA_MAX_CHARS`] characters (Unicode scalar values); exactly one
//! non-empty identifier; a non-empty `target_role`; `control_pass` only on
//! `pass` (by construction: only [`Pass`] carries a target). Not checked
//! locally: whether this app owns the thread, holds the escalation role,
//! or the target role is enabled on the account (Meta's to judge; its
//! errors say which, see `conversation-routing/thread-control` § Errors).
//!
//! None of the three is replayed after a timeout or any other error that
//! leaves Meta's state unknown: a replayed `pass` or `take` is a second
//! transfer, and a replayed `release` can release a thread a new inbound
//! message routed back to this app in between.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{PhoneNumberId, UserId};
use meta_whatsapp_core::recipient::Recipient;
use serde::{Deserialize, Serialize};

use crate::Client;

/// Longest `metadata`, in characters (`conversation-routing/thread-control`
/// § Parameters: "maximum 2,000 characters").
pub const METADATA_MAX_CHARS: usize = 2000;

/// Entry point, see [`Client::thread_control`].
#[derive(Debug, Clone)]
pub struct ThreadControl {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`ThreadControl`] API for the business phone number
    /// `phone_number_id`.
    pub fn thread_control(&self, phone_number_id: impl Into<PhoneNumberId>) -> ThreadControl {
        ThreadControl {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl ThreadControl {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Pass the thread with `user` to another responder: `POST
    /// /{phone-number-id}/thread_control` with `action: "pass"`
    /// (`conversation-routing/thread-control#pass`).
    ///
    /// [`Pass::to_escalation_partner`] (untargeted, no `control_pass`)
    /// passes to the account's escalation partner, which therefore cannot
    /// call it; [`Pass::to_role`] names a [`TargetRole`]. Only the
    /// current owner of an active thread may pass it: Meta rejects a pass
    /// on an idle thread, a thread you do not own, a target entry point
    /// with no primary or several, a target that already owns the thread,
    /// and a target not enabled on the account (`ai_agent` needs Meta
    /// Business Agent). The new owner receives a `control_passed` event
    /// carrying [`Pass::metadata`]; you receive nothing.
    ///
    /// Like the other two, it sends the client's token (a merchant's,
    /// through [`Client::with_token`]); the page names no permission.
    pub async fn pass(&self, user: &Recipient, pass: &Pass) -> Result<ThreadControlResponse> {
        let control_pass = match &pass.target_role {
            Some(role) => {
                if role.as_str().trim().is_empty() {
                    return Err(ValidationError::new(
                        "control_pass.target_role",
                        "must not be empty",
                    )
                    .into());
                }
                Some(ControlPass { target_role: role })
            }
            None => None,
        };
        let body = Body::new(user, Action::Pass, control_pass, pass.metadata.as_deref())?;
        self.send(&body, "thread control pass response").await
    }

    /// Take the thread with `user`, whoever owns it: `POST
    /// /{phone-number-id}/thread_control` with `action: "take"`
    /// (`conversation-routing/thread-control#take`).
    ///
    /// Only the account's designated escalation partner may take a
    /// thread; any other caller gets Meta's `2494191` ("You are not
    /// permitted to take thread control for this phone number"). The
    /// escalation partner also takes a thread implicitly by sending a
    /// Service message, with no call. The previous owner receives a
    /// `control_taken` event carrying `metadata` (at most
    /// [`METADATA_MAX_CHARS`] characters).
    pub async fn take(
        &self,
        user: &Recipient,
        metadata: Option<&str>,
    ) -> Result<ThreadControlResponse> {
        let body = Body::new(user, Action::Take, None, metadata)?;
        self.send(&body, "thread control take response").await
    }

    /// Release the thread with `user` back to idle: `POST
    /// /{phone-number-id}/thread_control` with `action: "release"`
    /// (`conversation-routing/thread-control#release`). The user's next
    /// message is routed by entry point.
    ///
    /// Only the current owner may release (the escalation partner too:
    /// it must [`Self::take`] first). Releasing an idle thread succeeds.
    /// A release names no new owner, so it triggers no
    /// `messaging_handovers` event; for the same reason this method sends
    /// no `metadata`, which Meta forwards only on that event.
    pub async fn release(&self, user: &Recipient) -> Result<ThreadControlResponse> {
        let body = Body::new(user, Action::Release, None, None)?;
        self.send(&body, "thread control release response").await
    }

    /// Not idempotent, see the [module docs](self).
    async fn send(&self, body: &Body<'_>, context: &'static str) -> Result<ThreadControlResponse> {
        self.client
            .post_at(&[self.phone_number_id.as_str(), "thread_control"])
            .json(body)
            .context(context)
            .send()
            .await
    }
}

/// What [`ThreadControl::pass`] sends besides the user: the target and the
/// `metadata`.
///
/// Build it with [`Pass::to_escalation_partner`] (also the
/// [`Default`]) or [`Pass::to_role`], then [`Pass::metadata`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pass {
    /// `control_pass.target_role`; `None` sends no `control_pass`, which
    /// passes to the escalation partner.
    pub target_role: Option<TargetRole>,
    /// Free-form text forwarded verbatim to the new owner on its
    /// `control_passed` event; at most [`METADATA_MAX_CHARS`] characters.
    pub metadata: Option<String>,
}

impl Pass {
    /// An untargeted pass: to the account's escalation partner, with no
    /// `control_pass` object.
    pub fn to_escalation_partner() -> Self {
        Self::default()
    }

    /// A targeted pass to `role` (`control_pass.target_role`). Meta treats
    /// [`TargetRole::Escalation`] as an untargeted pass.
    pub fn to_role(role: TargetRole) -> Self {
        Self {
            target_role: Some(role),
            metadata: None,
        }
    }

    /// Set the `metadata` field.
    #[must_use]
    pub fn metadata(mut self, metadata: impl Into<String>) -> Self {
        self.metadata = Some(metadata.into());
        self
    }
}

/// `control_pass.target_role`: the role a targeted [`ThreadControl::pass`]
/// hands the thread to (`conversation-routing/thread-control#pass`, the six
/// values of its table).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TargetRole {
    /// `ai_agent`: the account's designated AI agent, currently Meta
    /// Business Agent; only where it is enabled. The one role the
    /// business does not configure.
    AiAgent,
    /// `ctwa`: the responder for the click-to-WhatsApp ads entry point.
    Ctwa,
    /// `customer_service`: the responder for the customer service entry
    /// point.
    CustomerService,
    /// `escalation`: the account's escalation partner (an untargeted pass
    /// reaches it too).
    Escalation,
    /// `marketing`: the responder for the marketing entry point.
    Marketing,
    /// `utility`: the responder for the utility entry point.
    Utility,
    /// Any other value, sent verbatim (a role Meta adds later).
    #[serde(untagged)]
    Other(String),
}

impl TargetRole {
    /// The value as sent to Meta.
    pub fn as_str(&self) -> &str {
        match self {
            Self::AiAgent => "ai_agent",
            Self::Ctwa => "ctwa",
            Self::CustomerService => "customer_service",
            Self::Escalation => "escalation",
            Self::Marketing => "marketing",
            Self::Utility => "utility",
            Self::Other(other) => other,
        }
    }
}

impl std::fmt::Display for TargetRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The answer to a successful `pass`, `take` or `release`
/// (`conversation-routing/thread-control#response`). A failed one is an
/// error instead; quote its `fbtrace_id`
/// ([`GraphApiError`](meta_whatsapp_core::GraphApiError)) to support.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct ThreadControlResponse {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// Opaque identifier of this request; quote it to support.
    #[serde(default)]
    pub request_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Pass,
    Release,
    Take,
}

#[derive(Serialize)]
struct ControlPass<'a> {
    target_role: &'a TargetRole,
}

#[derive(Serialize)]
struct Body<'a> {
    messaging_product: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recipient: Option<&'a UserId>,
    action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    control_pass: Option<ControlPass<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<&'a str>,
}

impl<'a> Body<'a> {
    fn new(
        user: &'a Recipient,
        action: Action,
        control_pass: Option<ControlPass<'a>>,
        metadata: Option<&'a str>,
    ) -> Result<Self> {
        let (to, recipient) = match user {
            Recipient::Phone(phone) => {
                if phone.trim().is_empty() {
                    return Err(ValidationError::new("to", "must not be empty").into());
                }
                (Some(phone.as_str()), None)
            }
            Recipient::User(id) => {
                if id.as_str().trim().is_empty() {
                    return Err(ValidationError::new("recipient", "must not be empty").into());
                }
                (None, Some(id))
            }
            Recipient::PhoneAndUser { .. } => {
                return Err(ValidationError::new(
                    "recipient",
                    "give exactly one of `to` (phone number or wa_id) and `recipient` (BSUID), not both",
                )
                .into());
            }
            _ => {
                return Err(ValidationError::new(
                    "to",
                    "thread control acts on a WhatsApp user's thread: a phone number, wa_id or BSUID",
                )
                .into());
            }
        };
        if let Some(text) = metadata {
            let len = text.chars().count();
            if len > METADATA_MAX_CHARS {
                return Err(ValidationError::new(
                    "metadata",
                    format!("must be at most {METADATA_MAX_CHARS} characters, got {len}"),
                )
                .into());
            }
        }
        Ok(Self {
            messaging_product: "whatsapp",
            to,
            recipient,
            action,
            control_pass,
            metadata,
        })
    }
}

#[cfg(test)]
mod tests;
