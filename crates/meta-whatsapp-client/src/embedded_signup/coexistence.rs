//! The coexistence sync as the last two steps of onboarding (roadmap L11d,
//! `OPEN_QUESTIONS.md` #7): right after a coexistence onboarding,
//! `sync_contacts` and then `sync_history` request the WhatsApp Business
//! app's contacts and message history
//! ([`PhoneNumber::sync_smb_app_data`](crate::phone_numbers::PhoneNumber::sync_smb_app_data)),
//! in the order of `embedded-signup/onboarding-business-app-users`
//! ("Synchronizing WhatsApp Business app data", steps 1 and 2).
//!
//! Meta allows each request **once** per number, within 24 hours of
//! onboarding (`support/error-codes`: `2593107`, "exceeded the maximum
//! number of times to call the synchronization api for this phone number";
//! `2593108`, "only within 24 hours of onboarding"); only offboarding and a
//! new signup allow it again. So each request is claimed in a record of
//! its own before it is posted, and [`EmbeddedSignup::resume`](super::EmbeddedSignup::resume)
//! reads that record: it never posts a request that was accepted, refused,
//! or whose answer was lost.
//!
//! The record is keyed by phone number under [`COEXISTENCE_NAMESPACE`] and
//! scoped to one stored token record (its WABA and `created_at`): a new
//! onboarding stores a new token record, and with it starts a new sync
//! (the one Meta allows after an offboarding and a new signup). The
//! vault keeps `created_at` in whole seconds, so two onboardings of one
//! number stored within the same second share a scope, and the second
//! reports the first's answers without posting (an offboarding and a new
//! signup in between take minutes). A record of a later onboarding than
//! the attempt's is never taken over: the attempt fails at the step and
//! posts nothing.
//!
//! At most once, not exactly once: the claim is written before the post,
//! so a process that stops between the two leaves a claim with no
//! request behind it, which `resume` treats as an answer lost (it cannot
//! tell them apart). The way out is manual, as for a timeout: the
//! data's webhooks show whether the request went through, and asking
//! again with [`PhoneNumber::sync_smb_app_data`](crate::phone_numbers::PhoneNumber::sync_smb_app_data)
//! is safe in Meta's terms (a repeat is refused, `2593107`).

use meta_whatsapp_core::error::{StorageError, ValidationError};
use meta_whatsapp_core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_core::store::{Expiry, StoreKey, Versioned};
use meta_whatsapp_core::{Error, ErrorKind, Result};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::onboard::steps::{SYNC_CONTACTS, SYNC_HISTORY};
use super::vault::{StoredBusinessToken, TokenVault, decode_json, encode_json};
use crate::Client;
use crate::phone_numbers::SmbSyncType;

/// Store namespace of the coexistence sync records (key: the phone number
/// id), on the [`TokenVault`]'s store. Not secret: a record holds the WABA
/// and number ids, when each request was made and Meta's `request_id`.
///
/// Added by roadmap L11d; **pending the owner's confirmation before the
/// first release** (`docs/architecture.md` § Stable identifiers).
pub const COEXISTENCE_NAMESPACE: &str = "wa.es.coexistence";

/// The record layout's version, its `v`.
const RECORD_VERSION: u8 = 1;

/// Reads and writes of the record tried before giving up on a store
/// another writer keeps changing (nothing is posted then).
const ATTEMPTS: usize = 4;

/// What the coexistence sync steps established for the number: both of
/// Meta's requests, each accepted or refused, in this onboarding or an
/// earlier attempt of it ([`Onboarded::coexistence_sync`](super::Onboarded::coexistence_sync)).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CoexistenceSync {
    /// The number synced: the coexistence onboarding's.
    pub phone_number_id: PhoneNumberId,
    /// The contacts request (`sync_type` `smb_app_state_sync`); the
    /// contacts arrive as `smb_app_state_sync` webhooks.
    pub contacts: SyncOutcome,
    /// The message history request (`sync_type` `history`); the history
    /// arrives as `history` webhooks, or one `history` webhook with error
    /// `2593109` when the business declined to share it.
    pub history: SyncOutcome,
}

/// How Meta answered one sync request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SyncOutcome {
    /// Accepted: the data arrives by webhook.
    Requested {
        /// Meta's `request_id`: keep it, Meta support asks for it.
        request_id: String,
        /// When the answer came (the vault's clock).
        at: OffsetDateTime,
    },
    /// Refused with [`ErrorKind::SyncNotAllowed`]: `2593107` (already
    /// requested for this number) or `2593108` (more than 24 hours after
    /// onboarding), the codes Meta documents for the request (`2593109`,
    /// history sharing turned off, is of that kind too, but Meta sends it
    /// as a `history` webhook). Only offboarding and a new signup allow it
    /// again, so it is recorded, reported here, and never asked again for
    /// this onboarding.
    Refused {
        /// Meta's error code.
        code: i64,
        /// When the answer came (the vault's clock).
        at: OffsetDateTime,
    },
}

/// The two requests, in Meta's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncKind {
    Contacts,
    History,
}

impl SyncKind {
    fn sync_type(self) -> SmbSyncType {
        match self {
            Self::Contacts => SmbSyncType::SmbAppStateSync,
            Self::History => SmbSyncType::History,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Contacts => "contacts",
            Self::History => "history",
        }
    }

    /// The webhook field the data arrives on.
    fn webhook(self) -> &'static str {
        match self {
            Self::Contacts => "smb_app_state_sync",
            Self::History => "history",
        }
    }
}

/// The stored record: version 1, JSON, plain (nothing in it is secret).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SyncRecord {
    v: u8,
    waba_id: WabaId,
    /// The stored token record's `created_at`, in Unix seconds (the
    /// vault's precision): the onboarding this sync belongs to.
    token_created_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contacts: Option<Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history: Option<Slot>,
}

/// One request's state. Absent: not requested yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Slot {
    /// Claimed and posted (or being posted); no answer recorded.
    Pending {
        #[serde(with = "time::serde::timestamp")]
        at: OffsetDateTime,
    },
    /// Meta accepted it.
    Requested {
        #[serde(with = "time::serde::timestamp")]
        at: OffsetDateTime,
        request_id: String,
    },
    /// Meta refused it ([`ErrorKind::SyncNotAllowed`]).
    Refused {
        #[serde(with = "time::serde::timestamp")]
        at: OffsetDateTime,
        code: i64,
    },
}

impl Slot {
    fn outcome(&self) -> Option<SyncOutcome> {
        match self {
            Self::Pending { .. } => None,
            Self::Requested { at, request_id } => Some(SyncOutcome::Requested {
                request_id: request_id.clone(),
                at: *at,
            }),
            Self::Refused { at, code } => Some(SyncOutcome::Refused {
                code: *code,
                at: *at,
            }),
        }
    }
}

/// The onboarding a record belongs to.
struct Scope<'a> {
    waba_id: &'a WabaId,
    /// In Unix seconds: `onboard` holds the clock's full precision, the
    /// record `resume` reads back from the vault whole seconds.
    token_created_at: Option<i64>,
}

/// `at` in whole seconds, as the record keeps it: an outcome reads the
/// same from this call as from a resume.
fn whole_seconds(at: OffsetDateTime) -> OffsetDateTime {
    at.replace_nanosecond(0).unwrap_or(at)
}

impl SyncRecord {
    fn new(scope: &Scope<'_>) -> Self {
        Self {
            v: RECORD_VERSION,
            waba_id: scope.waba_id.clone(),
            token_created_at: scope.token_created_at,
            contacts: None,
            history: None,
        }
    }

    fn belongs_to(&self, scope: &Scope<'_>) -> bool {
        &self.waba_id == scope.waba_id && self.token_created_at == scope.token_created_at
    }

    fn slot(&self, kind: SyncKind) -> Option<&Slot> {
        match kind {
            SyncKind::Contacts => self.contacts.as_ref(),
            SyncKind::History => self.history.as_ref(),
        }
    }

    fn slot_mut(&mut self, kind: SyncKind) -> &mut Option<Slot> {
        match kind {
            SyncKind::Contacts => &mut self.contacts,
            SyncKind::History => &mut self.history,
        }
    }
}

fn record_key(phone_number_id: &PhoneNumberId) -> StoreKey {
    StoreKey::new(COEXISTENCE_NAMESPACE, phone_number_id.as_str())
}

/// Decode a record; one of another layout version fails closed.
fn decode(key: &StoreKey, v: &Versioned) -> Result<SyncRecord> {
    let record: SyncRecord = decode_json(key, v)?;
    if record.v != RECORD_VERSION {
        return Err(ValidationError::new(
            "coexistence_sync",
            format!(
                "the sync record is of layout version {}, which this revision does not know",
                record.v
            ),
        )
        .into());
    }
    Ok(record)
}

/// Steps `sync_contacts` and `sync_history` for the onboarding of
/// `stored`, on the coexistence number `phone_number_id`, with the
/// merchant's business token (`business`).
pub(super) async fn sync_coexistence(
    business: &Client,
    vault: &TokenVault,
    stored: &StoredBusinessToken,
    phone_number_id: Option<&PhoneNumberId>,
    done: &mut Vec<&'static str>,
) -> Result<CoexistenceSync> {
    let phone = phone_number_id.ok_or_else(|| {
        Error::from(ValidationError::new(
            "phone_number_id",
            "the coexistence onboarding finished without a phone number to sync",
        ))
        .in_step(SYNC_CONTACTS)
    })?;
    let scope = Scope {
        waba_id: &stored.waba_id,
        token_created_at: stored.created_at.map(OffsetDateTime::unix_timestamp),
    };
    let contacts = sync_one(business, vault, &scope, phone, SyncKind::Contacts)
        .await
        .map_err(|e| e.in_step(SYNC_CONTACTS))?;
    done.push(SYNC_CONTACTS);
    let history = sync_one(business, vault, &scope, phone, SyncKind::History)
        .await
        .map_err(|e| e.in_step(SYNC_HISTORY))?;
    done.push(SYNC_HISTORY);
    Ok(CoexistenceSync {
        phone_number_id: phone.clone(),
        contacts,
        history,
    })
}

/// One request: what the record says when it says anything, else claim
/// it, post it once, and record Meta's answer.
async fn sync_one(
    business: &Client,
    vault: &TokenVault,
    scope: &Scope<'_>,
    phone: &PhoneNumberId,
    kind: SyncKind,
) -> Result<SyncOutcome> {
    let key = record_key(phone);
    if let Some(outcome) = claim(vault, &key, scope, kind).await? {
        return Ok(outcome);
    }

    // Not idempotent: `GraphRequest` resends it only on what
    // `Error::may_resend` allows (a refusal before processing), never
    // after a timeout.
    let sent = business
        .phone_number(phone.clone())
        .sync_smb_app_data(kind.sync_type())
        .await;
    let now = whole_seconds(vault.now());
    let (slot, result) = match sent {
        Ok(answer) => {
            let outcome = SyncOutcome::Requested {
                request_id: answer.request_id.clone(),
                at: now,
            };
            (
                Some(Slot::Requested {
                    at: now,
                    request_id: answer.request_id,
                }),
                Ok(outcome),
            )
        }
        Err(e) if e.kind() == ErrorKind::SyncNotAllowed => {
            let code = e.graph().map_or(0, |g| g.code);
            tracing::warn!(
                sync_type = ?kind.sync_type(),
                code,
                "coexistence sync refused by Meta: recorded, not asked again for this onboarding"
            );
            (
                Some(Slot::Refused { at: now, code }),
                Ok(SyncOutcome::Refused { code, at: now }),
            )
        }
        // Meta refused it before doing anything: a resume may ask again.
        Err(e) if !e.may_have_been_sent() => (None, Err(e)),
        // It may have reached Meta: the claim stays, and nothing posts it
        // again.
        Err(e) => return Err(e),
    };
    // After the post, a store failure is logged, never returned
    // (`StorageError::Busy`): the claim left behind keeps a resume from
    // posting again.
    if let Err(e) = settle(vault, &key, scope, kind, slot).await {
        tracing::warn!(
            kind = ?e.kind(),
            sync_type = ?kind.sync_type(),
            "coexistence sync: recording Meta's answer failed; the claim stays and resume will not post again"
        );
    }
    result
}

/// Read the record: its answer for `kind` when it has one for this
/// onboarding (`Some`); else write a pending `kind` (atomically: one claim
/// wins) and return `None`, the caller's to post. A pending `kind` is an
/// error: its answer was lost, or another attempt is posting it.
async fn claim(
    vault: &TokenVault,
    key: &StoreKey,
    scope: &Scope<'_>,
    kind: SyncKind,
) -> Result<Option<SyncOutcome>> {
    let kv = vault.kv();
    for _ in 0..ATTEMPTS {
        let (mut record, version) = match kv.get(key).await? {
            Some(v) => {
                let record = decode(key, &v)?;
                if record.belongs_to(scope) {
                    (record, Some(v.version))
                } else if record.token_created_at > scope.token_created_at {
                    // A later onboarding's: this attempt, of an earlier
                    // one (a resume racing a new signup, or a WABA the
                    // number left), must not take it over.
                    return Err(superseded());
                } else {
                    // An earlier onboarding's: this one starts afresh.
                    (SyncRecord::new(scope), Some(v.version))
                }
            }
            None => (SyncRecord::new(scope), None),
        };
        match record.slot(kind) {
            Some(slot) => return slot.outcome().map(Some).ok_or_else(|| pending(kind)),
            None => {
                *record.slot_mut(kind) = Some(Slot::Pending {
                    at: whole_seconds(vault.now()),
                });
            }
        }
        let bytes = encode_json(key, &record)?;
        let written = match version {
            None => kv.put_if_absent(key, bytes, Expiry::Never).await?,
            Some(v) => {
                kv.compare_and_swap(key, v, Some(bytes), Expiry::Never)
                    .await?
            }
        };
        if written.is_some() {
            return Ok(None);
        }
    }
    Err(StorageError::Busy.into())
}

/// Replace this onboarding's pending `kind` with `slot` (`None`: clear it,
/// so that a resume asks again). A record another onboarding replaced
/// meanwhile is left alone.
async fn settle(
    vault: &TokenVault,
    key: &StoreKey,
    scope: &Scope<'_>,
    kind: SyncKind,
    slot: Option<Slot>,
) -> Result<()> {
    let kv = vault.kv();
    for _ in 0..ATTEMPTS {
        let Some(v) = kv.get(key).await? else {
            return Ok(());
        };
        let mut record = decode(key, &v)?;
        if !record.belongs_to(scope) || !matches!(record.slot(kind), Some(Slot::Pending { .. })) {
            return Ok(());
        }
        record.slot_mut(kind).clone_from(&slot);
        let bytes = encode_json(key, &record)?;
        if kv
            .compare_and_swap(key, v.version, Some(bytes), Expiry::Keep)
            .await?
            .is_some()
        {
            return Ok(());
        }
    }
    Err(StorageError::Busy.into())
}

/// A record of a later onboarding of the number than this attempt's.
fn superseded() -> Error {
    ValidationError::new(
        "coexistence_sync",
        "the number's sync record belongs to a later onboarding of it (a newer stored token record): this attempt, of an earlier one, sends nothing",
    )
    .into()
}

/// A request claimed by an attempt that did not record Meta's answer.
fn pending(kind: SyncKind) -> Error {
    ValidationError::new(
        "coexistence_sync",
        format!(
            "a {} sync request for this number may have reached Meta without its answer being recorded (a timeout, a 5xx, a process stopped before the answer was recorded, or another attempt still in flight): Meta allows one per number, so it is not sent again. Its `{}` webhooks show whether it went through; resume with OnboardingRequest::coexistence_sync(false), and if none arrive request it yourself with PhoneNumber::sync_smb_app_data (Meta refuses a repeat with 2593107)",
            kind.label(),
            kind.webhook(),
        ),
    )
    .into()
}
