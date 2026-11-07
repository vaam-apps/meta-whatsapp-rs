//! Conversation Routing's thread control: `pass`, `take` and `release` a
//! thread, and read the routing roles that say which of them the caller's
//! business may take ([`ThreadControl::routing_roles`]).
//!
//! Docs:
//! - `conversation-routing/thread-control` — the three actions, their
//!   parameters, the shared response, the errors and the
//!   `messaging_handovers` events they trigger.
//! - `conversation-routing/routing-roles` — `GET
//!   /{phone-number-id}/routing_roles`: the roles the caller's business
//!   holds on a number ([`RoutingRoles`], [`RoutingRole`]).
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
//! `take` hears nothing back, and no endpoint reports a thread's owner
//! ([`ThreadControl::routing_roles`] reports the business's configured
//! roles, not who owns a thread): a caller that tracks ownership records
//! its own successful calls
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
use meta_whatsapp_core::open_enum;
use meta_whatsapp_core::recipient::Recipient;
use serde::{Deserialize, Serialize};

use crate::Client;
use crate::request::send_checked;

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
    /// permitted to take thread control for this phone number"), whose
    /// [`ErrorKind`](meta_whatsapp_core::ErrorKind) is `Unknown`: branch on
    /// [`GraphApiError::code`](meta_whatsapp_core::GraphApiError::code). The
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

    /// The routing roles the caller's business holds on this phone number:
    /// `GET /{phone-number-id}/routing_roles`
    /// (`conversation-routing/routing-roles`). It takes no parameters.
    ///
    /// The answer is the business's routing configuration for this number
    /// (which entry points route to you, whether you are the escalation
    /// partner, whether you get standby copies), so it says which traffic
    /// to expect and which actions are open to you: [`Self::take`] needs
    /// [`RoutingRole::Escalation`], and a [`Pass`] can reach you only
    /// through a role you hold. It never says who owns a thread, and never
    /// names another partner or its roles. Roles belong to the business
    /// that owns the app, so every app of that business reads the same;
    /// they can differ between numbers of one WhatsApp Business Account
    /// (a per-number override), so read each number you serve.
    ///
    /// An empty [`RoutingRoles::roles`] is a success (routing is on, you
    /// hold no role); a number without Conversation Routing is an error
    /// instead, Meta's code `100` ("Conversation routing is not enabled
    /// for this phone number"), as is an id that does not exist or that
    /// the token cannot read (code `100` too): both are
    /// [`ErrorKind::InvalidParameter`](meta_whatsapp_core::ErrorKind::InvalidParameter)
    /// with the same code, so no kind tells them apart; Meta's message,
    /// for an operator to read, says which.
    ///
    /// The token needs `whatsapp_business_messaging` or
    /// `whatsapp_business_management` on the WhatsApp Business Account
    /// that owns the number (either gives the same answer). The endpoint
    /// is rate limited per number and per app: Meta asks for a read after
    /// onboarding, at start-up and periodically (spread across numbers),
    /// and when traffic stops matching what you expect, never before each
    /// message or action. A GET, it is retried under the client's
    /// [`RetryPolicy`](crate::RetryPolicy).
    pub async fn routing_roles(&self) -> Result<RoutingRoles> {
        self.client
            .get_at(&[self.phone_number_id.as_str(), "routing_roles"])
            .context("routing roles response")
            .send()
            .await
    }

    /// Not idempotent, see the [module docs](self).
    async fn send(&self, body: &Body<'_>, context: &'static str) -> Result<ThreadControlResponse> {
        // A 200 with `{"success": false}` is no answer of success: the page
        // documents none, but a caller that records the action (the
        // service's inbox) must never record one Meta did not take.
        let request = self
            .client
            .post_at(&[self.phone_number_id.as_str(), "thread_control"])
            .json(body);
        send_checked(request, context).await
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

open_enum! {
    /// `control_pass.target_role`: the role a targeted [`ThreadControl::pass`]
    /// hands the thread to (`conversation-routing/thread-control#pass`, the six
    /// values of its table).
    pub enum TargetRole {
        /// `ai_agent`: the account's designated AI agent, currently Meta
        /// Business Agent; only where it is enabled. The one role the
        /// business does not configure.
        AiAgent => "ai_agent",
        /// `ctwa`: the responder for the click-to-WhatsApp ads entry point.
        Ctwa => "ctwa",
        /// `customer_service`: the responder for the customer service entry
        /// point.
        CustomerService => "customer_service",
        /// `escalation`: the account's escalation partner (an untargeted pass
        /// reaches it too).
        Escalation => "escalation",
        /// `marketing`: the responder for the marketing entry point.
        Marketing => "marketing",
        /// `utility`: the responder for the utility entry point.
        Utility => "utility",
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

/// The answer of [`ThreadControl::routing_roles`]
/// (`conversation-routing/routing-roles` § Response): the routing roles
/// the caller's business holds on the number, and whether it has standby
/// visibility.
///
/// Meta documents both fields as always present, so a response without
/// either fails to decode rather than reading as "no role" or "not
/// standby".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct RoutingRoles {
    /// `roles`: the routing roles the business holds on this number, in
    /// Meta's order, which carries no meaning (Meta: an unordered set).
    /// Empty when it holds none: routing is enabled, but no entry point
    /// routes to it and it is not the escalation partner, so no inbound
    /// message is routed to it and no [`Pass`] can reach it.
    pub roles: Vec<RoutingRole>,
    /// `is_standby`: `true` when the business granted standby visibility
    /// (copies of activity on threads it does not own). Independent of
    /// [`Self::roles`]. It reports the routing configuration only, not
    /// whether the app subscribed to the `standby` webhook field, which
    /// the copies also need.
    pub is_standby: bool,
}

impl RoutingRoles {
    /// Whether [`Self::roles`] contains `role`, compared by wire name (so
    /// `RoutingRole::Other("calls".into())` finds [`RoutingRole::Calls`]).
    pub fn holds(&self, role: &RoutingRole) -> bool {
        self.roles.iter().any(|held| held.as_str() == role.as_str())
    }
}

open_enum! {
    /// One of `roles` in [`RoutingRoles`]
    /// (`conversation-routing/routing-roles` § Role values, the six values of
    /// its table).
    ///
    /// All but [`RoutingRole::Calls`] are also the role identifiers of
    /// `messaging_handovers` events (`previous_owner_role`, `new_owner_role`).
    /// `ai_agent` is never returned (a partner business cannot hold it); were
    /// Meta to send it, or a role it adds later, it reads as
    /// [`RoutingRole::Other`].
    pub enum RoutingRole {
        /// `calls`: the Call primary, which receives call webhooks and call
        /// permission replies and alone may take call actions. Never on a
        /// handover event: handling a call never makes it the thread's owner.
        Calls => "calls",
        /// `ctwa`: the primary for the click-to-WhatsApp ads entry point.
        Ctwa => "ctwa",
        /// `customer_service`: the primary for the Service entry point.
        CustomerService => "customer_service",
        /// `escalation`: the designated escalation partner, which may
        /// [`ThreadControl::take`] any thread.
        Escalation => "escalation",
        /// `marketing`: the primary for the Marketing Message Response entry
        /// point.
        Marketing => "marketing",
        /// `utility`: the primary for the Utility Response entry point.
        Utility => "utility",
    }
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
