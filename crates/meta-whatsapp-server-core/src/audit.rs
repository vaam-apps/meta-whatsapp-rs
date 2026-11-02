//! Audit events (docs/design/server.md, section 6): what an operator, or a
//! tenant destroying something at Meta, changed. Each is a `tracing` event
//! with the target `audit`, whichever crate emits it, so a subscriber
//! routes them by target alone; the services of [`crate::admin`] emit
//! theirs, and an API adapter emits the tenant's.
//!
//! An event names ids only (a tenant, a key's public id, a WABA): never a
//! secret or a phone number. A payment change's ([`payment_audit`]) adds
//! what reconciles it: the number's id, the configuration's name, an
//! order's `reference_id`, an amount and Meta's refund id, and never an
//! OAuth link (docs/design/server.md, D58).

/// An operator's change, logged at `info` with the target `audit`:
/// `action`, the admin key's public id, and the ids it touched (tenant,
/// key, WABA; never a secret or a phone number).
pub fn audit(action: &'static str, admin_key_id: &str, subject: &Subject<'_>) {
    tracing::info!(
        target: "audit",
        action,
        admin_key_id,
        tenant = subject.tenant,
        key_id = subject.key_id,
        waba_id = subject.waba_id,
        "admin change"
    );
}

/// A tenant's change that destroys something at Meta (a template
/// deletion), logged like [`audit`] (target `audit`) with the public id of
/// the tenant or platform key that made it.
pub fn tenant_audit(action: &'static str, key_id: &str, subject: &Subject<'_>) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        "tenant change"
    );
}

/// A tenant's payment change (roadmap P2; docs/design/server.md, D58),
/// logged like [`tenant_audit`] (target `audit`, message `tenant
/// change`), with `outcome` (`answered`, or `unknown` when the call may
/// have gone through unanswered) and what reconciles it ([`PaymentFacts`]).
pub fn payment_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    payment: &PaymentFacts<'_>,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = payment.phone_number_id,
        configuration_name = payment.configuration_name,
        reference_id = payment.reference_id,
        amount_value = payment.amount_value,
        amount_offset = payment.amount_offset,
        currency = payment.currency,
        refund_id = payment.refund_id,
        refund_status = payment.refund_status,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

/// Whether a [`payment_audit`] change went through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Meta answered it.
    Answered,
    /// It may have gone through unanswered (a timeout, a failure that
    /// `may_have_been_sent`): look it up.
    Unknown,
}

impl Outcome {
    /// Its name in the event.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Unknown => "unknown",
        }
    }
}

/// What a [`payment_audit`] event names besides its [`Subject`]: ids, an
/// order's reference and an amount; never an OAuth link or a token.
#[derive(Debug, Default, Clone, Copy)]
pub struct PaymentFacts<'a> {
    /// The number a lookup or a refund went through.
    pub phone_number_id: Option<&'a str>,
    /// The payment configuration's name.
    pub configuration_name: Option<&'a str>,
    /// A refunded order's `reference_id`.
    pub reference_id: Option<&'a str>,
    /// A refund's amount times its offset.
    pub amount_value: Option<u64>,
    /// A refund's offset (100 for INR).
    pub amount_offset: Option<u32>,
    /// A refund's currency.
    pub currency: Option<&'a str>,
    /// Meta's refund id, when Meta answered.
    pub refund_id: Option<&'a str>,
    /// Meta's refund status, when Meta answered.
    pub refund_status: Option<&'a str>,
}

/// What an [`audit`] event touched.
#[derive(Debug, Default, Clone, Copy)]
pub struct Subject<'a> {
    /// A tenant id.
    pub tenant: Option<&'a str>,
    /// A key's public id.
    pub key_id: Option<&'a str>,
    /// A WABA id.
    pub waba_id: Option<&'a str>,
}
