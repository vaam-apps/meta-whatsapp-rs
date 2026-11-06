//! Conversation routing (docs/design/server.md, section 4.2; roadmap M5l),
//! as services an API adapter calls, with the tenant's WABA token, over
//! the library's `client::thread_control`:
//!
//! | Operation | Graph call |
//! | --- | --- |
//! | [`Routing::roles`] | `GET /{pn}/routing_roles` |
//! | [`Routing::pass`] | `POST /{pn}/thread_control` with `action: pass` |
//! | [`Routing::take`] | `POST /{pn}/thread_control` with `action: take` |
//! | [`Routing::release`] | `POST /{pn}/thread_control` with `action: release` |
//!
//! Pages: `conversation-routing/thread-control`,
//! `conversation-routing/routing-roles`, `conversation-routing/overview`.
//!
//! **The thread is the user's with the number**, named as the page names
//! it: by `to` (a phone number or `wa_id`) or by `recipient` (a BSUID,
//! Meta's preferred identifier), exactly one ([`user`]). A group, a parent
//! BSUID and a user named twice are refused before any request. A user is
//! no object of Meta's to check against the number, so M5.2 has nothing
//! further to look for: **no request field names an app**. The target of a
//! pass is a *role* ([`TARGET_ROLES`]: the six values of the page's table),
//! and a role is configured by the business that owns the number, so it
//! cannot name another tenant's app. The services open only on a number of
//! the [`Caller`]'s own tenant (step 4 of the authorization order,
//! [`crate::authz`]).
//!
//! **Nothing here accepts terms.** Thread control works only once the
//! business accepted Meta's Business Agents and Platform Terms of Service,
//! in Meta Business Suite; no call here does, and Meta's refusal for a
//! business that has not is passed on as its code.
//!
//! **A thread action acts on a person**, so each is the caller's audit
//! event ([`crate::audit::routing_audit`]: `thread_passed`,
//! `thread_taken`, `thread_released`) with the number, the WABA, the
//! target role of a pass and an outcome, `unknown` when the call may have
//! gone through unanswered. **Never who**: no user id, no phone number and
//! no `metadata` (free text forwarded to the new owner) reaches the event
//! or any log. Meta's texts are withheld from the answer and the logs of
//! the three actions as the block list's are
//! ([`OwnedNumber::failed_withholding_texts`]): a text may quote the user.
//! The roles read keeps them (no user is named).
//!
//! **A thread action is not idempotent at Meta and is never replayed**:
//! the library does not retry it after a timeout, and the adapter keys the
//! three routes by `Idempotency-Key`.

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::thread_control::{
    METADATA_MAX_CHARS, Pass, RoutingRole, RoutingRoles, TargetRole, ThreadControl,
    ThreadControlResponse,
};
use meta_whatsapp_rs::core::ids::UserId;
use meta_whatsapp_rs::core::recipient::Recipient;

use crate::audit::{Outcome, RoutingFacts, Subject, routing_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::error::ServiceError;
use crate::messages::is_e164;
use crate::observe::Observer;

/// The audit actions (docs/design/server.md, D58 and D221).
const PASSED: &str = "thread_passed";
const TAKEN: &str = "thread_taken";
const RELEASED: &str = "thread_released";

/// The values of `control_pass.target_role` the page documents
/// (`conversation-routing/thread-control#pass`, its table). A closed list
/// on purpose (D222): a role Meta adds later is a service release, never a
/// string passed through.
pub const TARGET_ROLES: [&str; 6] = [
    "ai_agent",
    "ctwa",
    "customer_service",
    "escalation",
    "marketing",
    "utility",
];

// ─── The request's checks ────────────────────────────────────────────────

/// The user whose thread a request acts on: a phone number, a `wa_id` or a
/// BSUID, exactly one.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Person {
    /// E.164 **with** its `+`.
    pub phone: Option<String>,
    /// The user's `wa_id`: digits, no `+`.
    pub wa_id: Option<String>,
    /// A business-scoped user id (`US.13491208655302741918`).
    pub user_id: Option<String>,
}

impl std::fmt::Debug for Person {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Person").finish_non_exhaustive()
    }
}

/// The library's recipient for `person`, checked.
///
/// # Errors
///
/// `422 invalid_request` on `user` (none or more than one of `phone`,
/// `wa_id`, `user_id`), on `user.phone` (not strict E.164 with its `+`),
/// `user.wa_id` (not 1 to 15 digits) or `user.user_id` (not a standard
/// BSUID: a parent BSUID is not one). Never quotes a value.
pub fn user(person: Person) -> Result<Recipient, ServiceError> {
    match (person.phone, person.wa_id, person.user_id) {
        (Some(phone), None, None) if is_e164(&phone) => Ok(Recipient::Phone(phone)),
        (Some(_), None, None) => Err(ServiceError::invalid("user.phone")),
        (None, Some(wa_id), None)
            if (1..=15).contains(&wa_id.len())
                && wa_id.bytes().all(|b| b.is_ascii_digit())
                && !wa_id.starts_with('0') =>
        {
            Ok(Recipient::Phone(wa_id))
        }
        (None, Some(_), None) => Err(ServiceError::invalid("user.wa_id")),
        (None, None, Some(user)) => {
            let id = UserId::new(user);
            if id.is_bsuid() {
                Ok(Recipient::User(id))
            } else {
                Err(ServiceError::invalid("user.user_id"))
            }
        }
        _ => Err(ServiceError::invalid("user")),
    }
}

/// The `metadata` of a request: at most [`METADATA_MAX_CHARS`] characters
/// (Meta's bound; Unicode scalar values).
///
/// # Errors
///
/// `422 invalid_request` on `metadata`. Never quotes it.
pub fn metadata(metadata: Option<String>) -> Result<Option<String>, ServiceError> {
    match metadata {
        Some(text) if text.chars().count() > METADATA_MAX_CHARS => {
            Err(ServiceError::invalid("metadata"))
        }
        other => Ok(other),
    }
}

/// The library's [`Pass`] for a request: an untargeted pass (to the
/// escalation partner) without `target_role`, else a targeted one.
///
/// # Errors
///
/// `422 invalid_request` on `target_role` (not one of [`TARGET_ROLES`]) or
/// `metadata` ([`metadata`]). Never quotes them.
pub fn pass_of(target_role: Option<String>, note: Option<String>) -> Result<Pass, ServiceError> {
    let note = metadata(note)?;
    let mut pass = match target_role {
        None => Pass::to_escalation_partner(),
        Some(role) if TARGET_ROLES.contains(&role.as_str()) => {
            Pass::to_role(TargetRole::from(role))
        }
        Some(_) => return Err(ServiceError::invalid("target_role")),
    };
    pass.metadata = note;
    Ok(pass)
}

// ─── The answers ─────────────────────────────────────────────────────────

/// The answer of a thread action Meta accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Acted {
    /// Always `whatsapp`, when Meta says.
    pub messaging_product: Option<String>,
    /// Meta's opaque id of the request, for its support.
    pub request_id: Option<String>,
}

impl From<ThreadControlResponse> for Acted {
    fn from(response: ThreadControlResponse) -> Self {
        Self {
            messaging_product: response.messaging_product,
            request_id: response.request_id,
        }
    }
}

/// The routing roles the number's business holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roles {
    /// The roles, by Meta's names (`calls`, `ctwa`, `customer_service`,
    /// `escalation`, `marketing`, `utility`; a later one as it comes).
    pub roles: Vec<String>,
    /// Whether the business has standby visibility.
    pub is_standby: bool,
}

impl From<RoutingRoles> for Roles {
    fn from(roles: RoutingRoles) -> Self {
        Self {
            roles: roles
                .roles
                .iter()
                .map(RoutingRole::as_str)
                .map(str::to_owned)
                .collect(),
            is_standby: roles.is_standby,
        }
    }
}

// ─── The operations ──────────────────────────────────────────────────────

/// The routing operations on a number of the caller's tenant: an
/// [`OwnedNumber`] its [`Authorizer`] made from that tenant's binding, for
/// a [`Caller`] it made, both admitted ([`Self::new`]), with the number's
/// WABA token. Cheap: a few references.
pub struct Routing<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Routing<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Routing")
            .field("caller", self.caller)
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> Routing<'a> {
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
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_number(number)?;
        if number.tenant() != caller.tenant() {
            tracing::warn!("refused the routing services on another tenant's number");
            return Err(ServiceError::not_found());
        }
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

    fn thread_control(&self) -> ThreadControl {
        self.number
            .client()
            .thread_control(self.number.phone_number_id().clone())
    }

    /// The error of a failed Graph call, with Meta's texts or without.
    async fn failed(&self, error: &Error, withhold_texts: bool) -> ServiceError {
        let api = if withhold_texts {
            self.number
                .failed_withholding_texts(self.authz, error)
                .await
        } else {
            self.number
                .failed(self.authz, error)
                .await
                .with_details(error)
        };
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    fn audit(&self, action: &'static str, target_role: Option<&str>, outcome: Outcome) {
        routing_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.number.waba_id().as_str()),
                phone_number_id: Some(self.number.phone_number_id().as_str()),
                ..Subject::default()
            },
            &RoutingFacts { target_role },
            outcome,
        );
    }

    /// The error of an action that failed, audited as `action` with an
    /// unknown outcome when it may have gone through.
    async fn action_failed(
        &self,
        action: &'static str,
        target_role: Option<&str>,
        error: &Error,
    ) -> ServiceError {
        let failed = self.failed(error, true).await;
        if failed.may_have_been_sent() {
            self.audit(action, target_role, Outcome::Unanswered);
        }
        failed
    }

    /// The routing roles the number's business holds: `GET
    /// /{pn}/routing_roles`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details` (code `100` for a
    /// number without Conversation Routing).
    pub async fn roles(&self) -> Result<Roles, ServiceError> {
        match self.thread_control().routing_roles().await {
            Ok(roles) => Ok(Roles::from(roles)),
            Err(error) => Err(self.failed(&error, false).await),
        }
    }

    /// Pass the thread with `user` ([`user`], checked) as `pass`
    /// ([`pass_of`], checked): not replayed on a timeout. The caller's
    /// audit event `thread_passed`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its code (a timeout
    /// `may_have_been_sent`).
    pub async fn pass(&self, user: &Recipient, pass: &Pass) -> Result<Acted, ServiceError> {
        let role = pass.target_role.as_ref().map(TargetRole::as_str);
        match self.thread_control().pass(user, pass).await {
            Ok(response) => {
                self.audit(PASSED, role, Outcome::Answered);
                Ok(Acted::from(response))
            }
            Err(error) => Err(self.action_failed(PASSED, role, &error).await),
        }
    }

    /// Take the thread with `user` (checked), the escalation partner's
    /// only: not replayed on a timeout. The caller's audit event
    /// `thread_taken`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its code (`2494191`: not the
    /// escalation partner).
    pub async fn take(&self, user: &Recipient, note: Option<&str>) -> Result<Acted, ServiceError> {
        match self.thread_control().take(user, note).await {
            Ok(response) => {
                self.audit(TAKEN, None, Outcome::Answered);
                Ok(Acted::from(response))
            }
            Err(error) => Err(self.action_failed(TAKEN, None, &error).await),
        }
    }

    /// Release the thread with `user` (checked) to idle: not replayed on a
    /// timeout. The caller's audit event `thread_released`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its code.
    pub async fn release(&self, user: &Recipient) -> Result<Acted, ServiceError> {
        match self.thread_control().release(user).await {
            Ok(response) => {
                self.audit(RELEASED, None, Outcome::Answered);
                Ok(Acted::from(response))
            }
            Err(error) => Err(self.action_failed(RELEASED, None, &error).await),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(error: &ServiceError) -> String {
        error.field().unwrap_or("?").to_owned()
    }

    fn phone(phone: &str) -> Person {
        Person {
            phone: Some(phone.to_owned()),
            ..Person::default()
        }
    }

    /// The user of a request: exactly one of a strict E.164 phone, a
    /// `wa_id` of digits, a standard BSUID; the field named, never the
    /// value. Decisive: each clause.
    #[test]
    fn a_request_names_one_user_one_way() {
        assert!(matches!(
            user(phone("+16505551234")).unwrap(),
            Recipient::Phone(_)
        ));
        let wa = |id: &str| Person {
            wa_id: Some(id.to_owned()),
            ..Person::default()
        };
        assert!(matches!(
            user(wa("16505551234")).unwrap(),
            Recipient::Phone(_)
        ));
        let bsuid = |id: &str| Person {
            user_id: Some(id.to_owned()),
            ..Person::default()
        };
        assert!(matches!(
            user(bsuid("US.13491208655302741918")).unwrap(),
            Recipient::User(_)
        ));
        assert_eq!(
            field(&user(phone("16505551234")).unwrap_err()),
            "user.phone"
        );
        for bad in ["+16505551234", "0123", "1234567890123456", ""] {
            assert_eq!(field(&user(wa(bad)).unwrap_err()), "user.wa_id", "{bad}");
        }
        for bad in [
            "US.ENT.11815799212886844830",
            "13491208655302741918",
            "us.1",
        ] {
            assert_eq!(
                field(&user(bsuid(bad)).unwrap_err()),
                "user.user_id",
                "{bad}"
            );
        }
        let both = Person {
            phone: Some("+16505551234".to_owned()),
            user_id: Some("US.1".to_owned()),
            ..Person::default()
        };
        assert_eq!(field(&user(both).unwrap_err()), "user");
        assert_eq!(field(&user(Person::default()).unwrap_err()), "user");
    }

    /// A pass without a role is untargeted; with one of the page's six it
    /// is targeted; any other is refused. The note is bounded in
    /// characters, not bytes. Decisive: the list, the bound's edge.
    #[test]
    fn a_pass_targets_one_of_the_six_roles_or_none() {
        assert_eq!(pass_of(None, None).unwrap(), Pass::to_escalation_partner());
        // The list is pinned by value, not by itself: the page's six.
        assert_eq!(
            TARGET_ROLES,
            [
                "ai_agent",
                "ctwa",
                "customer_service",
                "escalation",
                "marketing",
                "utility"
            ]
        );
        for role in [
            "ai_agent",
            "ctwa",
            "customer_service",
            "escalation",
            "marketing",
            "utility",
        ] {
            let pass = pass_of(Some(role.to_owned()), None).unwrap();
            assert_eq!(
                pass.target_role.as_ref().map(TargetRole::as_str),
                Some(role)
            );
        }
        for bad in ["", "Escalation", "calls", "billing", " ctwa"] {
            assert_eq!(
                field(&pass_of(Some(bad.to_owned()), None).unwrap_err()),
                "target_role",
                "{bad:?}"
            );
        }
        let edge = "é".repeat(METADATA_MAX_CHARS);
        assert!(pass_of(None, Some(edge.clone())).is_ok());
        assert_eq!(
            field(&pass_of(None, Some(format!("{edge}x"))).unwrap_err()),
            "metadata"
        );
    }
}
