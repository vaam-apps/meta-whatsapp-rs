//! Erasing a customer on a number (roadmap M2f; docs/design/server.md,
//! section 4.2, decisions D10, D30, D31), as a service an API adapter
//! calls on a number the caller's tenant owns: [`Erasures::erase`].
//!
//! **Only when asked.** Which requests to honour is the deployer's
//! decision (D10, "Legal acts"): the service erases when a caller with the
//! scope `erasure` asks, for the identities it names, on the one number it
//! names, and never on its own (no schedule, no second erasure after
//! Meta's redelivery window, no replay of the journal after a restore).
//!
//! **What one erasure does**, in this order, so that a failure at any step
//! leaves the request whole to repeat:
//!
//! 1. The local checks, before anything is read: the deployment has an
//!    erasure key ([`ErasureDeployment`], else `409
//!    erasure_not_configured`), `confirm` is the number's id again (`422`
//!    on `confirm`), and `contacts` holds 1 to [`MAX_ERASURE_CONTACTS`]
//!    identities the inbox's routes accept (`422` on `contacts`, never
//!    quoting one).
//! 2. The number's binding, read again ([`OwnedNumber`]'s `still_held`):
//!    a number that moved since the request was authorized is `503
//!    storage_unavailable`, nothing erased.
//! 3. The binding's cut (decision D90, narrow by default): a number with a
//!    history cut ([`crate::authz::history_from`]: its WABA, or the number
//!    itself, came from another tenant) is `409 erasure_crosses_binding`,
//!    nothing erased. The inbox's reads stop at the cut, and the erasure
//!    must reach no further than they do, but the library's erasure
//!    (`ConversationStore::erase_all`, and `Inbox::erase_all` and
//!    `Inbox::erase` over it) takes no time bound: it would delete the
//!    previous holder's hidden records of the person too. Whether an
//!    erasure may reach them is the owner's (roadmap § Owner touchpoints);
//!    until then the operator erases on such a number through the library.
//! 4. Every identity the inbox connects to each contact on the number
//!    (the library's `Inbox::identities`, D30: the synced contacts and the
//!    identity links), before anything is deleted (the erasure deletes the
//!    links that connect them).
//! 5. The journal entry ([`JournalEntry`], through the deployment's
//!    [`ErasureJournal`]): for each identity, the hex HMAC-SHA256 of
//!    `<phone_number_id>|<contact>` under the deployment's key, and the
//!    time. A journal that fails stops the erasure (`503`), nothing
//!    deleted: an erasure is never done without its entry.
//! 6. The tenant's own outbox rows naming one of the identities on the
//!    number ([`Outbox::erase_contacts`], D89, narrow by default): never
//!    the number's operator-only rows (the operator's, which expire with
//!    the outbox's retention), nor another tenant's.
//! 7. The inbox's records of them on the number (the library's
//!    `Inbox::erase_all`: their conversations, window events, ownership
//!    records, synced contacts, identity links, their group messages
//!    redacted or deleted per the store's mode, D31): on a number with no
//!    cut (step 3), every record of the number is the tenant's to read.
//! 8. The tenant's audit event `customer_erased` (target `audit`): the
//!    key's id, the tenant, the WABA and the number, the erasure's id and
//!    what each store deleted, by count.
//!
//! The outbox goes before the inbox: a repeat after a failure between them
//! finds the same identities (the inbox's links are still there), whereas
//! the other order would leave the outbox's rows of every identity but the
//! named ones out of a repeat's reach.
//!
//! **Idempotent.** A repeat deletes nothing more and answers the same way;
//! it writes a journal entry and an audit event of its own. So no
//! `Idempotency-Key` (nothing is sent, and an idempotency record would keep
//! a fingerprint of the request), and the answer ([`Erasure`]) says
//! nothing of what was found: not whether the person was known, nor how
//! many records went (the operator's audit event has the counts).
//!
//! **Nothing names the customer in clear**: not the answer, the journal,
//! the audit event, an error, a log line at any level, a metric, or an
//! idempotency record (there is none). The journal's HMAC is a keyed
//! digest: without the key a phone number cannot be found from it by
//! trying them all.
//!
//! **What it does not reach** (the guides list them for the deployer):
//! the number's operator-only outbox rows and another tenant's (a previous
//! holder's), both kept by the outbox's retention; a number with a cut
//! (step 3: refused whole); rows inserted before the outbox kept contacts (migration
//! 8: a rolling deploy's previous release, and every row before it), a
//! customer quoted in someone else's message, a body the library could not
//! parse, the 24-hour idempotency answers of sends to them, the webhook
//! dedup markers (keyed hashes, kept on purpose), what was forwarded
//! (live streams' clients), Postgres's dead rows, WAL, replicas and
//! backups, and Meta's side (the contact book: the library's
//! `PhoneNumber::delete_contact_book_entry`). What arrives after the
//! erasure is recorded again: erase again once Meta's 7-day redelivery
//! window has passed.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::error::{ConfigError, CryptoError, StorageError};
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::core::secret::SecretBytes;
use meta_whatsapp_rs::core::store::{ConversationStore, Erased};
use meta_whatsapp_rs::inbox::Inbox;
use sha2::Sha256;
use time::OffsetDateTime;

use crate::audit::{ErasureFacts, Subject, erasure_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::error::{ErrorCode, ServiceError};
use crate::model::TenantId;
use crate::outbox::Outbox;

/// Most identities one request names (`contacts`): a person's BSUIDs and
/// phone numbers, the ones the caller holds itself. Each is closed over
/// the inbox's identities on the number, which may add more.
pub const MAX_ERASURE_CONTACTS: usize = 16;

/// Fewest bytes of an erasure key (`WA_SERVER_ERASURE_KEY`), as the OTP
/// pepper's.
pub const ERASURE_KEY_MIN_BYTES: usize = 32;

/// The `tracing` target of [`LogJournal`]'s entries (a stable identifier,
/// pending the owner's confirmation: docs/architecture.md, § Stable
/// identifiers): an operator routes it to a store outside the database it
/// restores.
pub const JOURNAL_TARGET: &str = "erasure_journal";

/// The version of [`JournalEntry`]'s layout and of its HMAC's input
/// (`<phone_number_id>|<contact>`, under the key, hex), written `v` in
/// each entry.
pub const JOURNAL_VERSION: u32 = 1;

/// The audit event's action.
pub const ERASURE_ACTION: &str = "customer_erased";

/// The key the journal's HMACs are made with (`WA_SERVER_ERASURE_KEY`):
/// at least [`ERASURE_KEY_MIN_BYTES`], from the operator's secret manager,
/// kept as long as the backups an entry may have to be replayed over.
/// `Debug` shows nothing of it.
#[derive(Clone)]
pub struct ErasureKey(SecretBytes);

impl fmt::Debug for ErasureKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ErasureKey([REDACTED])")
    }
}

impl ErasureKey {
    /// `key`, of at least [`ERASURE_KEY_MIN_BYTES`].
    ///
    /// # Errors
    ///
    /// A shorter key: a configuration error, which never quotes it.
    pub fn new(key: SecretBytes) -> Result<Self, Error> {
        if key.expose_secret().len() < ERASURE_KEY_MIN_BYTES {
            return Err(ConfigError::new("the erasure key must be at least 32 bytes").into());
        }
        Ok(Self(key))
    }

    /// A key of 32 random bytes for this process: the development service
    /// on memory storage, whose records die with it anyway.
    ///
    /// # Errors
    ///
    /// The OS random number generator failed.
    pub fn throwaway() -> Result<Self, Error> {
        let mut bytes = vec![0u8; ERASURE_KEY_MIN_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| Error::from(CryptoError::Rng))?;
        Self::new(SecretBytes::new(bytes))
    }

    /// The journal's HMAC of `contact` on `phone_number_id`: hex
    /// HMAC-SHA256, under this key, of `<phone_number_id>|<contact>` (a
    /// phone number id is digits, so the first `|` splits it): what an
    /// operator computes over a restored database's identities to find the
    /// ones to erase again (docs/guides/production.md, § 8).
    pub fn journal_hmac(&self, phone_number_id: &PhoneNumberId, contact: &str) -> String {
        // An HMAC takes a key of any length: this cannot fail.
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(self.0.expose_secret()) else {
            return String::new();
        };
        mac.update(phone_number_id.as_str().as_bytes());
        mac.update(b"|");
        mac.update(contact.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

/// One erasure's journal entry: no identity in clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    /// The erasure's id (`era_` and 32 hex digits), also in its audit
    /// event and its answer.
    pub erasure_id: String,
    /// When it was made, on the service's clock.
    pub at: OffsetDateTime,
    /// The tenant that asked.
    pub tenant: TenantId,
    /// The business number it erased on.
    pub phone_number_id: PhoneNumberId,
    /// [`ErasureKey::journal_hmac`] of each identity erased, in byte
    /// order.
    pub identities: Vec<String>,
}

/// Where the journal's entries go: a store outside the database the
/// service restores (docs/guides/production.md, § 8: after a restore, the
/// operator erases again every identity whose HMAC an entry lists). The
/// service's is [`LogJournal`]; a deployment of its own composition
/// writes elsewhere (an object store, another database) by implementing
/// this.
#[async_trait]
pub trait ErasureJournal: Send + Sync + 'static {
    /// Record `entry`, durably, before the erasure deletes anything.
    ///
    /// # Errors
    ///
    /// The entry could not be recorded: the erasure stops, nothing
    /// deleted (`503 storage_unavailable`).
    async fn record(&self, entry: &JournalEntry) -> Result<(), StorageError>;
}

/// The service's journal: each entry one `info` event with the target
/// [`JOURNAL_TARGET`] and the message `erasure journal entry`, its fields
/// `v` ([`JOURNAL_VERSION`]), `erasure_id`, `at` (RFC 3339), `tenant`,
/// `phone_number_id` and `identities` (the HMACs, comma-separated). The
/// operator's log pipeline keeps the target outside the database (and
/// past the logs' own retention). It never fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogJournal;

#[async_trait]
impl ErasureJournal for LogJournal {
    async fn record(&self, entry: &JournalEntry) -> Result<(), StorageError> {
        let at = entry
            .at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        tracing::info!(
            target: JOURNAL_TARGET,
            v = JOURNAL_VERSION,
            erasure_id = %entry.erasure_id,
            at = %at,
            tenant = %entry.tenant.as_str(),
            phone_number_id = %entry.phone_number_id.as_str(),
            identities = %entry.identities.join(","),
            "erasure journal entry"
        );
        Ok(())
    }
}

/// What the deployment sets for erasure: the journal's key and where its
/// entries go. `None` in the service's settings: every erasure is `409
/// erasure_not_configured`. `Debug` shows no key.
#[derive(Clone)]
pub struct ErasureDeployment {
    key: ErasureKey,
    journal: Arc<dyn ErasureJournal>,
}

impl fmt::Debug for ErasureDeployment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ErasureDeployment").finish_non_exhaustive()
    }
}

impl ErasureDeployment {
    /// `key`, its entries in the service's [`LogJournal`].
    pub fn new(key: ErasureKey) -> Self {
        Self {
            key,
            journal: Arc::new(LogJournal),
        }
    }

    /// The entries in `journal` instead.
    #[must_use]
    pub fn with_journal(mut self, journal: Arc<dyn ErasureJournal>) -> Self {
        self.journal = journal;
        self
    }

    /// The journal's key.
    pub fn key(&self) -> &ErasureKey {
        &self.key
    }
}

/// What [`Erasures::erase`] answers: the erasure's id and time, nothing of
/// what it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Erasure {
    /// Its id, in its journal entry and its audit event.
    pub erasure_id: String,
    /// When it was made.
    pub erased_at: OffsetDateTime,
}

/// The erasure on a number the caller's tenant owns (see the
/// [module](self)).
pub struct Erasures<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    store: Arc<dyn ConversationStore>,
    outbox: &'a dyn Outbox,
    deployment: Option<&'a ErasureDeployment>,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for Erasures<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Erasures")
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> Erasures<'a> {
    /// The erasure for `caller` on `number`, once `authz` admits both,
    /// over the bundle's inbox (`store`) and `outbox`, with the
    /// deployment's settings (`None`: not set up) and the service's clock.
    ///
    /// # Errors
    ///
    /// `caller` or `number` another [`Authorizer`] made: `403 forbidden`,
    /// and nothing is read.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        number: &'a OwnedNumber,
        store: Arc<dyn ConversationStore>,
        outbox: &'a dyn Outbox,
        deployment: Option<&'a ErasureDeployment>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_number(number)?;
        Ok(Self {
            authz,
            caller,
            number,
            store,
            outbox,
            deployment,
            clock,
        })
    }

    /// Erase the person `contacts` name on the number: `confirm` is the
    /// number's id again. The steps, their order and what they leave are
    /// the [module](self)'s.
    ///
    /// # Errors
    ///
    /// `409 erasure_not_configured`; `422 invalid_request` on `confirm` or
    /// `contacts` (never quoting a contact), nothing read; `503
    /// storage_unavailable` (the number moved, the journal or a store
    /// failed): repeat it, an erasure is idempotent; `409
    /// erasure_crosses_binding` (the number has a history cut), nothing
    /// erased.
    pub async fn erase(&self, confirm: &str, contacts: &[String]) -> Result<Erasure, ServiceError> {
        let deployment = self
            .deployment
            .ok_or_else(|| ServiceError::new(ErrorCode::ERASURE_NOT_CONFIGURED))?;
        let pn = self.number.phone_number_id();
        crate::registration::confirmed(pn, confirm)?;
        let keys = checked_contacts(pn, contacts)?;
        self.number.still_held(self.authz).await?;
        // D90, narrow by default: the library's erasure has no time bound,
        // so on a number with a cut it would reach the previous holder's
        // hidden records (step 3 of the module's).
        if self.number.history_from().is_some() {
            return Err(ServiceError::new(ErrorCode::ERASURE_CROSSES_BINDING));
        }
        let inbox = Inbox::new(
            self.number.client().clone(),
            pn.clone(),
            Arc::clone(&self.store),
        )
        .with_clock(Arc::clone(&self.clock));
        let mut identities = BTreeSet::new();
        for key in &keys {
            identities.extend(inbox.identities(key).await?);
        }
        let identities: Vec<String> = identities.into_iter().collect();
        let erasure_id = erasure_id()?;
        let at = self.clock.now();
        let mut hmacs: Vec<String> = identities
            .iter()
            .map(|contact| deployment.key.journal_hmac(pn, contact))
            .collect();
        hmacs.sort();
        deployment
            .journal
            .record(&JournalEntry {
                erasure_id: erasure_id.clone(),
                at,
                tenant: self.caller.tenant().clone(),
                phone_number_id: pn.clone(),
                identities: hmacs,
            })
            .await?;
        let events = self
            .outbox
            .erase_contacts(self.caller.tenant(), pn, &identities)
            .await?;
        let erased = inbox.erase_all(&identities).await?;
        self.audit(&erasure_id, identities.len(), events, &erased);
        Ok(Erasure {
            erasure_id,
            erased_at: at,
        })
    }

    /// The tenant's audit event of this erasure: ids and counts only.
    fn audit(&self, erasure_id: &str, identities: usize, events: u64, erased: &Erased) {
        erasure_audit(
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.number.waba_id().as_str()),
                phone_number_id: Some(self.number.phone_number_id().as_str()),
                ..Subject::default()
            },
            &ErasureFacts {
                erasure_id,
                identities: u64::try_from(identities).unwrap_or(u64::MAX),
                events,
                erased,
            },
        );
    }
}

/// The request's contacts as the number's conversation keys
/// ([`crate::inbox::conversation_key`]): 1 to [`MAX_ERASURE_CONTACTS`],
/// each one the inbox's routes accept, duplicates once.
///
/// # Errors
///
/// `422 invalid_request` on `contacts`, never quoting one.
fn checked_contacts(
    pn: &PhoneNumberId,
    contacts: &[String],
) -> Result<Vec<meta_whatsapp_rs::core::store::ConversationKey>, ServiceError> {
    if contacts.is_empty() || contacts.len() > MAX_ERASURE_CONTACTS {
        return Err(ServiceError::invalid("contacts"));
    }
    let unique: BTreeSet<&String> = contacts.iter().collect();
    unique
        .into_iter()
        .map(|contact| {
            crate::inbox::conversation_key(pn, contact)
                .map_err(|_| ServiceError::invalid("contacts"))
        })
        .collect()
}

/// A new erasure id: `era_` and 16 random bytes in hex.
fn erasure_id() -> Result<String, ServiceError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ServiceError::from(Error::from(CryptoError::Rng)))?;
    Ok(format!("era_{}", hex::encode(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(bytes: &[u8]) -> ErasureKey {
        ErasureKey::new(SecretBytes::new(bytes.to_vec())).unwrap()
    }

    /// The journal's HMAC is HMAC-SHA256 of `<phone_number_id>|<contact>`,
    /// hex: a known answer computed apart from the code (RFC 2104 with
    /// SHA-256, `printf '106540352242922|16505551234' | openssl dgst
    /// -sha256 -hmac 0123456789abcdef0123456789abcdef`). Decisive: the
    /// input's order and separator, the key, the hex.
    #[test]
    fn the_journal_hmac_is_pinned() {
        let k = key(b"0123456789abcdef0123456789abcdef");
        let pn = PhoneNumberId::new("106540352242922").unwrap();
        assert_eq!(
            k.journal_hmac(&pn, "16505551234"),
            "f9a639b1c98d67d3b869639cec40ac51b43fee8e0e4c4415ed946f1fd59678f8"
        );
        assert_ne!(
            k.journal_hmac(&pn, "16505551234"),
            key(b"0123456789abcdef0123456789abcdeX").journal_hmac(&pn, "16505551234"),
            "another key, another digest"
        );
        assert_eq!(k.journal_hmac(&pn, "x").len(), 64);
    }

    /// A key under 32 bytes is refused, and no key is ever shown.
    #[test]
    fn a_short_key_is_refused_and_none_is_shown() {
        let short = ErasureKey::new(SecretBytes::new(
            b"0123456789abcdef0123456789abcde".to_vec(),
        ));
        assert!(short.is_err());
        let k = key(b"0123456789abcdef0123456789abcdef");
        assert_eq!(format!("{k:?}"), "ErasureKey([REDACTED])");
        let deployment = ErasureDeployment::new(k);
        assert!(!format!("{deployment:?}").contains("0123"));
        assert!(ErasureKey::throwaway().is_ok());
    }

    /// The contacts a request may name: 1 to 16, each one the inbox's
    /// routes accept, refused on `contacts` otherwise. Decisive: each bound.
    #[test]
    fn the_contacts_are_checked_before_anything_is_read() {
        let pn = PhoneNumberId::new("106540352242922").unwrap();
        let named = |n: usize| (0..n).map(|i| format!("1650555{i:04}")).collect::<Vec<_>>();
        assert_eq!(checked_contacts(&pn, &named(1)).unwrap().len(), 1);
        assert_eq!(
            checked_contacts(&pn, &named(MAX_ERASURE_CONTACTS))
                .unwrap()
                .len(),
            MAX_ERASURE_CONTACTS
        );
        for (contacts, why) in [
            (Vec::new(), "none"),
            (named(MAX_ERASURE_CONTACTS + 1), "too many"),
            (vec![" ".to_owned()], "blank"),
            (vec!["a\nb".to_owned()], "a control character"),
            (
                vec!["1".repeat(crate::inbox::MAX_CONTACT_BYTES + 1)],
                "too long",
            ),
        ] {
            let refused = checked_contacts(&pn, &contacts).unwrap_err();
            assert_eq!(refused.code(), "invalid_request", "{why}");
            assert_eq!(refused.field(), Some("contacts"), "{why}");
        }
        let twice = vec!["16505551234".to_owned(), "16505551234".to_owned()];
        assert_eq!(checked_contacts(&pn, &twice).unwrap().len(), 1);
    }

    /// An erasure id is `era_` and 32 hex digits, a new one each time.
    #[test]
    fn erasure_ids_are_random() {
        let (a, b) = (erasure_id().unwrap(), erasure_id().unwrap());
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert!(a.starts_with("era_"));
        assert!(a[4..].bytes().all(|b| b.is_ascii_hexdigit()));
    }
}
