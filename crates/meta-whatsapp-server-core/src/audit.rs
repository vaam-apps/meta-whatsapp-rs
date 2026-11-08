//! Audit events (docs/design/server.md, section 6): what an operator, or a
//! tenant destroying something at Meta, changed. Each is a `tracing` event
//! with the target `audit`, whichever crate emits it, so a subscriber
//! routes them by target alone; the services of [`crate::admin`] emit
//! theirs, and an API adapter emits the tenant's.
//!
//! An event names ids only (a tenant, a key's public id, a WABA, a phone
//! number id): never a secret or a phone number. A payment change's
//! ([`payment_audit`]) adds what reconciles it: the number's id, the
//! configuration's name, an order's `reference_id`, an amount and Meta's
//! refund id, and never an OAuth link (docs/design/server.md, D58). An
//! erasure's ([`erasure_audit`], roadmap M2f) adds its id and what each
//! store deleted, by count: never an identity of the person erased.

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
        phone_number_id = subject.phone_number_id,
        "admin change"
    );
}

/// A tenant's change that destroys something at Meta (a template
/// deletion, a number deregistered) or a number's security (its two-step
/// verification PIN set), logged like [`audit`] (target `audit`) with the
/// public id of the tenant or platform key that made it.
pub fn tenant_audit(action: &'static str, key_id: &str, subject: &Subject<'_>) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
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

/// A tenant's block or unblock of people on a number (roadmap M5g;
/// docs/design/server.md, D143), logged like [`tenant_audit`] (target
/// `audit`, message `tenant change`) with `outcome` and the counts of
/// [`ContactFacts`]. Never who: the people blocked are the caller's and
/// Meta's (a phone number, a BSUID), in no log of the service.
pub fn contacts_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    contacts: &ContactFacts,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
        users = contacts.users,
        done = contacts.done,
        failed = contacts.failed,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

/// What a [`contacts_audit`] event names besides its [`Subject`]: counts.
#[derive(Debug, Default, Clone, Copy)]
pub struct ContactFacts {
    /// How many people the request named.
    pub users: u64,
    /// How many Meta blocked (or unblocked); `0` when it did not answer.
    pub done: u64,
    /// How many Meta refused, one by one.
    pub failed: u64,
}

/// A tenant's thread action on a number (roadmap M5l; docs/design/server.md,
/// D221), logged like [`tenant_audit`] (target `audit`, message `tenant
/// change`) with `outcome` and [`RoutingFacts`]. Never who: the user whose
/// thread it is (a phone number, a BSUID) and the `metadata` forwarded to
/// the new owner are in no log of the service.
pub fn routing_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    routing: &RoutingFacts<'_>,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
        target_role = routing.target_role,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

/// What a [`routing_audit`] event names besides its [`Subject`].
#[derive(Debug, Default, Clone, Copy)]
pub struct RoutingFacts<'a> {
    /// The role a pass named (one of `routing::TARGET_ROLES`); `None` for
    /// an untargeted pass and for a take or a release.
    pub target_role: Option<&'a str>,
}

/// A tenant's broadcast, created or cancelled (roadmap M5k; D423), logged
/// like [`tenant_audit`] (target `audit`, message `tenant change`) with the
/// broadcast's id and a count. Never who: no recipient (a phone number, a
/// BSUID), template parameter or message text is in any log of the service.
pub fn broadcast_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    broadcast: &BroadcastFacts<'_>,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
        broadcast_id = broadcast.broadcast_id,
        recipients = broadcast.recipients,
        "tenant change"
    );
}

/// What a [`broadcast_audit`] event names besides its [`Subject`].
#[derive(Debug, Clone, Copy)]
pub struct BroadcastFacts<'a> {
    /// The broadcast's id.
    pub broadcast_id: &'a str,
    /// How many people it was for (a count, never who).
    pub recipients: usize,
}

/// A customer erased on a number (roadmap M2f: `crate::erasure`), logged
/// like [`tenant_audit`] (target `audit`, message `tenant change`, action
/// `customer_erased`) with the erasure's id (its journal entry's) and
/// counts: the identities erased, the outbox rows, and each of the
/// inbox's records ([`Erased`](meta_whatsapp_rs::core::store::Erased)).
/// Never an identity, in clear or hashed: the journal has the HMACs.
pub fn erasure_audit(key_id: &str, subject: &Subject<'_>, erasure: &ErasureFacts<'_>) {
    let erased = erasure.erased;
    tracing::info!(
        target: "audit",
        action = crate::erasure::ERASURE_ACTION,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
        erasure_id = erasure.erasure_id,
        identities = erasure.identities,
        events = erasure.events,
        messages = erased.messages,
        conversations = erased.conversations,
        window_events = erased.window_events,
        thread_owners = erased.thread_owners,
        contacts = erased.contacts,
        identity_links = erased.identity_links,
        group_messages = erased.group_messages,
        "tenant change"
    );
}

/// What an [`erasure_audit`] event names besides its [`Subject`].
#[derive(Debug, Clone, Copy)]
pub struct ErasureFacts<'a> {
    /// The erasure's id.
    pub erasure_id: &'a str,
    /// How many identities it erased.
    pub identities: u64,
    /// The outbox rows it deleted.
    pub events: u64,
    /// What the inbox deleted.
    pub erased: &'a meta_whatsapp_rs::core::store::Erased,
}

/// Whether a [`payment_audit`] change went through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Meta answered it.
    Answered,
    /// It may have gone through unanswered (a timeout, a failure that
    /// `may_have_been_sent`): look it up. Written `unknown`.
    Unanswered,
}

impl Outcome {
    /// Its name in the event.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Unanswered => "unknown",
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

/// A tenant's change to a group (roadmap M5f; docs/design/server.md,
/// D155), logged like [`tenant_audit`] (target `audit`, message `tenant
/// change`) with `outcome` and the counts of [`GroupFacts`]. Never who
/// (the participants are in no log) and never which group (its id embeds
/// the business number): the number's id, the counts and the outcome.
pub fn groups_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    facts: GroupFacts,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
        users = facts.users,
        done = facts.done,
        failed = facts.failed,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

/// What a [`groups_audit`] event names besides its [`Subject`]: counts.
#[derive(Debug, Default, Clone, Copy)]
pub struct GroupFacts {
    /// How many participants or join requests the request named.
    pub users: u64,
    /// How many Meta answered as done; `0` when it did not answer.
    pub done: u64,
    /// How many Meta refused, one by one.
    pub failed: u64,
}

/// A tenant's calling change or call action (roadmap M5e; docs/design/server.md,
/// D216), logged like [`tenant_audit`] (target `audit`, message `tenant
/// change`) with `outcome` and the facts of [`CallFacts`]. Never who is
/// called, never which call (its id is Meta's and names the number's
/// traffic), never an SDP, a SIP host or a SIP password: the number's id,
/// whether the call is recorded or transcribed (the consent the other
/// participant is asked for), the settings' section names and the outcome.
pub fn calls_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    facts: CallFacts<'_>,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
        recording = facts.recording,
        transcription = facts.transcription,
        sections = facts.sections,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

/// What a [`calls_audit`] event names besides its [`Subject`].
#[derive(Debug, Default, Clone, Copy)]
pub struct CallFacts<'a> {
    /// The call was asked to be recorded (`recording.status` `ENABLED`).
    pub recording: bool,
    /// The call was asked to be transcribed (`transcription.status`
    /// `ENABLED`).
    pub transcription: bool,
    /// The names of the calling settings' sections a change set, comma
    /// separated (`call_hours,sip`); never their values.
    pub sections: Option<&'a str>,
}

/// A tenant's request to change a number's display name (roadmap M5c2;
/// docs/design/server.md, D164), logged like [`tenant_audit`] (target
/// `audit`, message `tenant change`) with `outcome` and the length of the
/// name asked for. The change starts Meta's review and, once approved and
/// the number registered again, shows to every customer: so who asked and
/// for which number is kept. The name itself is not (it is the business's
/// to read back from the status route).
pub fn display_name_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    name_chars: u64,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
        name_chars,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

/// A tenant's change to a number's settings, username, compliance
/// information or contact book, or an application for Official Business
/// Account status (roadmap M5c3; docs/design/server.md, D192), logged like
/// [`tenant_audit`] (target `audit`, message `tenant change`) with
/// `outcome` and, when the action covers several settings, which one
/// (`setting`, a name the service chose, never a value). Never a key, a
/// username, a contact's id or what a declaration says.
pub fn number_setting_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    setting: Option<&'static str>,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
        setting,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

/// A tenant's change of a number's Flows business public key (roadmap
/// M5d2; docs/design/server.md, D453), logged like [`tenant_audit`]
/// (target `audit`, message `tenant change`) with `outcome` and the SHA-256
/// (hex) of the key's canonical PEM, `public_key_sha256`: a name for a
/// public key, so a change can be reconciled with the key
/// ([`crate::flow_key`] answers the same digest). Never the key.
/// `flow_key_override` is `true` when the key is not the public half of the
/// tenant's Flow key and the caller allowed that: a deliberate act, named in
/// its own field.
pub fn business_key_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    public_key_sha256: &str,
    flow_key_override: bool,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
        public_key_sha256,
        flow_key_override,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

/// A tenant's download of a Flow's uploaded media (roadmap M5d2; D456),
/// logged like [`tenant_audit`] (target `audit`, message `tenant change`)
/// with the WABA and the size of what was returned: never a key, a URL, a
/// name, a hash of the file or a byte of it.
pub fn flow_media_audit(action: &'static str, key_id: &str, subject: &Subject<'_>, bytes: usize) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        bytes,
        "tenant change"
    );
}

/// A tenant's change refused before anything was sent to Meta, where the
/// refusal is itself worth a record (a number's webhook override asked for
/// while the deployment has not enabled it, or at a destination the
/// allow-list does not admit; roadmap M5c6). Logged at `warn` with the
/// target `audit`: `action`, the key's public id, the ids, and `reason`, a
/// name the service chose. Never the URL, the token or any value asked for.
pub fn number_setting_refused_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    reason: &'static str,
) {
    tracing::warn!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        phone_number_id = subject.phone_number_id,
        reason,
        "tenant change refused"
    );
}

/// A tenant's marketing change (roadmap M5h; docs/design/server.md, D232):
/// a template duplicated at another max price or its max price set or
/// cleared, the Cloud API marketing switch, a welcome message sequence
/// created, changed or deleted. Logged like [`tenant_audit`] (target
/// `audit`, message `tenant change`) with `outcome` and what reconciles it
/// ([`MarketingFacts`]): ids, a price, a flag. Never a welcome message's
/// text, a name, a template's parameters or a recipient.
pub fn marketing_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    facts: &MarketingFacts<'_>,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        template_id = facts.template_id,
        new_template_id = facts.new_template_id,
        sequence_id = facts.sequence_id,
        bid_amount = facts.bid_amount,
        disabled = facts.disabled,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

/// What a [`marketing_audit`] event names besides its [`Subject`].
#[derive(Debug, Default, Clone, Copy)]
pub struct MarketingFacts<'a> {
    /// The template changed or duplicated (the source).
    pub template_id: Option<&'a str>,
    /// The duplicate Meta made.
    pub new_template_id: Option<&'a str>,
    /// The welcome message sequence.
    pub sequence_id: Option<&'a str>,
    /// The max price set, per 1,000 deliveries in the currency's smallest
    /// unit.
    pub bid_amount: Option<u32>,
    /// The Cloud API marketing switch's new value (`true`: blocked).
    pub disabled: Option<bool>,
}

/// A tenant's analytics switch (roadmap M5g2; docs/design/server.md,
/// D286): template insights confirmed for a WABA, or a template opted out
/// of, or back into, button click tracking. Logged like [`tenant_audit`]
/// (target `audit`, message `tenant change`) with `outcome` and the ids and
/// flag of [`AnalyticsFacts`]. Never a template's name or content, and
/// none of Meta's texts.
pub fn analytics_audit(
    action: &'static str,
    key_id: &str,
    subject: &Subject<'_>,
    facts: &AnalyticsFacts<'_>,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id,
        tenant = subject.tenant,
        waba_id = subject.waba_id,
        template_id = facts.template_id,
        opted_out = facts.opted_out,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

/// What an [`analytics_audit`] event names besides its [`Subject`].
#[derive(Debug, Default, Clone, Copy)]
pub struct AnalyticsFacts<'a> {
    /// The template whose click tracking changed.
    pub template_id: Option<&'a str>,
    /// Whether click tracking is now opted out of (`true`: off).
    pub opted_out: Option<bool>,
}

/// The operator's signing of Meta's max price beta agreement (roadmap
/// M5h; docs/design/server.md, D231; `OPEN_QUESTIONS.md` #45): a legal act,
/// logged like [`audit`] (target `audit`, message `admin change`) with the
/// admin key's public id, the business portfolio's id and `outcome`. The
/// signer's name and email are personal data and in no event.
pub fn max_price_agreement_audit(
    action: &'static str,
    admin_key_id: &str,
    business_id: &str,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        admin_key_id,
        business_id,
        outcome = outcome.as_str(),
        "admin change"
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
    /// A phone number id (Meta's id of the number, never the number).
    pub phone_number_id: Option<&'a str>,
}
