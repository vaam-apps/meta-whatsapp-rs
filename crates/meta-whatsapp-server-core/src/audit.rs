//! Audit events (docs/design/server.md, section 6): what an operator, or a
//! tenant destroying something at Meta, changed. Each is a `tracing` event
//! with the target `audit`, whichever crate emits it, so a subscriber
//! routes them by target alone; the services of [`crate::admin`] emit
//! theirs, and an API adapter emits the tenant's.
//!
//! An event names ids only (a tenant, a key's public id, a WABA): never a
//! secret or a phone number.

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
