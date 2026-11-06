//! The token-expiry event ([`crate::token_expiry`], roadmap M3e) over a
//! backend's bundle: a round walks the bindings and the vault the bundle
//! shares, records `token_expiring` through [`NumberEvents`](crate::number_events::NumberEvents) into the
//! bundle's outbox for the token's tenant only, once per token and expiry
//! (a second round, and a round on another replica, record nothing), again
//! for a token stored anew, never for a token outside the lead time, one
//! that never expires or one already expired; the turn is one replica's at
//! a time; and a token's text is nowhere in what is recorded. A WABA that
//! moves to another tenant while a round reads its token is told to
//! nobody by that round (and to its new holder by the next), one whose
//! record cannot be read neither stops the round nor is told, and a walk
//! reaches the WABAs past its first page. The clock is a fake one the vault
//! and the authorizer share, moved across the lead.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::clock::{Clock, ManualClock};
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use serde_json::json;
use time::OffsetDateTime;

use super::number_events::{number_events, polled};
use super::webhooks::{Holder, hold};
use crate::authz::Authorizer;
use crate::backend::Backend;
use crate::model::TenantId;
use crate::observe::Observer;
use crate::store::{TOKEN_EXPIRY, TOKEN_EXPIRY_LEASE};
use crate::token_expiry::TokenExpiring;
use crate::token_expiry::{Round, round};

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    a_token_inside_the_lead_is_told_to_its_tenant_once(backend).await;
    a_token_stored_again_is_told_again_and_one_expired_is_not(backend).await;
    a_round_without_the_turn_does_nothing(backend).await;
    a_waba_that_moves_while_its_token_is_read_is_told_to_nobody(backend).await;
    a_waba_moved_to_another_tenant_is_told_to_its_holder_for_the_same_expiry(backend).await;
    an_event_for_a_binding_that_moved_before_the_insert_reaches_neither_tenant(backend).await;
    an_unreadable_record_neither_stops_the_round_nor_is_told(backend).await;
    a_walk_reaches_the_wabas_past_its_first_page(backend).await;
    a_purged_event_is_told_again_which_is_why_the_lead_may_not_exceed_the_retention(backend).await;
}

const LEAD: Duration = Duration::from_hours(7 * 24);
const DAY: time::Duration = time::Duration::DAY;
/// A token's text, which must reach no event.
const SECRET: &str = "EAAG-the-token-the-expiry-event-must-never-quote";

/// An observer that remembers what it was told.
#[derive(Default)]
struct Counting(Mutex<Vec<&'static str>>);

impl Counting {
    fn told(&self, outcome: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|told| **told == outcome)
            .count()
    }
}

impl Observer for Counting {
    fn graph_error(&self, _: &'static str) {}
    fn idempotency(&self, _: &'static str) {}
    fn webhook_event(&self, _: &'static str, _: &'static str) {}
    fn webhook_duplicates(&self, _: &'static str, _: u64) {}
    fn webhook_failure(&self, _: &'static str) {}
    fn token_expiry(&self, outcome: &'static str) {
        self.0.lock().unwrap().push(outcome);
    }
}

type Hook = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A key/value store that misbehaves on the vault's record of one WABA:
/// runs `after_read` once the record was read (the WABA moves "meanwhile"),
/// or answers garbage for it.
struct Hooked {
    inner: Arc<dyn KvStore>,
    waba: String,
    after_read: Mutex<Option<Hook>>,
    garbage: bool,
}

impl std::fmt::Debug for Hooked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooked").finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl KvStore for Hooked {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        if key.namespace() != "wa.token" || !key.key().ends_with(&self.waba) {
            return self.inner.get(key).await;
        }
        if self.garbage {
            return Ok(Some(Versioned {
                value: b"not a record".to_vec(),
                version: 1,
                expires_at: None,
            }));
        }
        let read = self.inner.get(key).await;
        let hook = self.after_read.lock().unwrap().take();
        if let Some(hook) = hook {
            hook.await;
        }
        read
    }

    async fn put(&self, key: &StoreKey, v: Vec<u8>, e: Expiry) -> Result<u64, StorageError> {
        self.inner.put(key, v, e).await
    }

    async fn put_if_absent(
        &self,
        key: &StoreKey,
        v: Vec<u8>,
        e: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.inner.put_if_absent(key, v, e).await
    }

    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        e: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.inner.compare_and_swap(key, expected, new, e).await
    }

    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

/// What a case runs on: the vault and the authorizer over one fake clock.
struct Rig {
    clock: ManualClock,
    vault: TokenVault,
    authz: Authorizer,
    observer: Arc<Counting>,
}

fn rig(backend: &dyn Backend) -> Rig {
    rig_reading(backend, backend.kv())
}

/// [`rig`], the authorizer reading the vault through `kv` (the rig's own
/// vault, which stores, goes straight to the backend's).
fn rig_reading(backend: &dyn Backend, kv: Arc<dyn KvStore>) -> Rig {
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let keys = || VaultKeys::new(VaultKey::generate("token-expiry-suite").unwrap());
    let vault = TokenVault::new(backend.kv(), keys())
        .unwrap()
        .with_clock(Arc::new(clock.clone()));
    let reading = TokenVault::new(kv, keys())
        .unwrap()
        .with_clock(Arc::new(clock.clone()));
    let client = Client::builder()
        .transport(ScriptedTransport::new())
        .build()
        .unwrap();
    let authz = Authorizer::new(backend.records(), reading, client)
        .unwrap()
        .with_clock(Arc::new(clock.clone()))
        .with_number_events(number_events(backend));
    Rig {
        clock,
        vault,
        authz,
        observer: Arc::new(Counting::default()),
    }
}

impl Rig {
    /// Store `holder`'s token, expiring `after` from now (`None`: never).
    async fn store(&self, holder: Holder, after: Option<time::Duration>) {
        let mut token =
            StoredBusinessToken::new(WabaId::new(holder.waba).unwrap(), AccessToken::new(SECRET))
                .phone_number_ids([PhoneNumberId::new(holder.pn).unwrap()]);
        if let Some(after) = after {
            token = token.expires_at(self.clock.now() + after);
        }
        self.vault.store(&token).await.unwrap();
    }

    async fn round(&self, backend: &dyn Backend) -> Round {
        round(&self.authz, backend, self.observer.as_ref(), LEAD)
            .await
            .expect("the turn was free")
    }
}

/// Roadmap M3e's decisive case. Tenant A's token expires in 3 days, inside
/// the 7-day lead; tenant B's in 30 days, outside; tenant C's never.
/// Round one tells A, once, with the data the module pins (ids and the
/// expiry, no token), and tells B and C nothing: B polls no event of its
/// own and none of A's; round two tells nobody, and neither does a third
/// after a day has passed; the counter holds one `recorded`; 25 days on, B
/// is inside its lead and is told, A (expired by then) is told nothing more.
/// Decisive: the comparison with the lead (a token outside it told), the
/// event's key (A told again by round two), its routing (B polls A's),
/// and the expired branch (A told again at day 25).
pub async fn a_token_inside_the_lead_is_told_to_its_tenant_once(backend: &dyn Backend) {
    const A: Holder = Holder {
        id: "te-tenant-a",
        waba: "102290129346001",
        pn: "106540352246011",
    };
    const B: Holder = Holder {
        id: "te-tenant-b",
        waba: "102290129346002",
        pn: "106540352246012",
    };
    const C: Holder = Holder {
        id: "te-tenant-c",
        waba: "102290129346003",
        pn: "106540352246013",
    };
    let records = backend.records();
    for holder in [A, B, C] {
        hold(records.as_ref(), holder).await;
    }
    let rig = rig(backend);
    rig.store(A, Some(3 * DAY)).await;
    rig.store(B, Some(30 * DAY)).await;
    rig.store(C, None).await;
    let expires_at: OffsetDateTime = rig.clock.now() + 3 * DAY;

    let first = rig.round(backend).await;
    assert_eq!(
        (first.due, first.recorded, first.failed),
        (1, 1, 0),
        "{first:?}"
    );
    assert!(first.wabas >= 3, "{first:?}");
    let of_a = polled(backend, A.id).await;
    assert_eq!(of_a.len(), 1, "{of_a:?}");
    assert_eq!(of_a[0].event_type, "token_expiring");
    let expected = expires_at
        .replace_nanosecond(0)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&of_a[0].data).unwrap(),
        json!({"event": "token_expiring", "waba_id": A.waba, "expires_at": expected})
    );
    assert!(!of_a[0].data.contains(SECRET), "{}", of_a[0].data);
    assert_eq!(of_a[0].phone_number_id, None);
    assert!(polled(backend, B.id).await.is_empty());
    assert!(polled(backend, C.id).await.is_empty());

    // A second round, and a day on: told already.
    let second = rig.round(backend).await;
    assert_eq!(
        (second.due, second.recorded),
        (1, 0),
        "a second round told it again: {second:?}"
    );
    rig.clock.advance(Duration::from_hours(24));
    assert_eq!(rig.round(backend).await.recorded, 0);
    assert_eq!(polled(backend, A.id).await.len(), 1);
    assert_eq!(rig.observer.told("recorded"), 1);
    assert_eq!(rig.observer.told("failed"), 0);

    // 25 days on (A expired 23 days ago, B has 4 left): B is told, A not
    // again.
    rig.clock.advance(Duration::from_hours(24 * 24));
    let later = rig.round(backend).await;
    assert_eq!((later.due, later.recorded), (1, 1), "{later:?}");
    let of_b = polled(backend, B.id).await;
    assert_eq!(of_b.len(), 1, "{of_b:?}");
    assert!(of_b[0].data.contains(B.waba), "{of_b:?}");
    assert_eq!(polled(backend, A.id).await.len(), 1);
    assert!(polled(backend, C.id).await.is_empty());
    assert_eq!(rig.observer.told("recorded"), 2);
}

/// A token stored again (the merchant ran Embedded Signup again: a new
/// expiry) is told again when it nears its new one; the same token stored
/// again with the same expiry is not; a token stored already expired is
/// told never. Decisive: the expiry in the key (a refreshed token told
/// not at all), and its absence from the binding's generation (the same
/// token told twice).
pub async fn a_token_stored_again_is_told_again_and_one_expired_is_not(backend: &dyn Backend) {
    const D: Holder = Holder {
        id: "te-tenant-d",
        waba: "102290129346004",
        pn: "106540352246014",
    };
    const E: Holder = Holder {
        id: "te-tenant-e",
        waba: "102290129346005",
        pn: "106540352246015",
    };
    let records = backend.records();
    hold(records.as_ref(), D).await;
    hold(records.as_ref(), E).await;
    let rig = rig(backend);
    rig.store(E, Some(-DAY)).await;
    rig.store(D, Some(2 * DAY)).await;
    assert_eq!(rig.round(backend).await.recorded, 1);
    // The same token again (same expiry), its WABA attached again to the
    // same tenant (a new binding generation): nothing more.
    rig.store(D, Some(2 * DAY)).await;
    let before = records
        .waba(&WabaId::new(D.waba).unwrap())
        .await
        .unwrap()
        .unwrap();
    records
        .bind_waba(
            &before.tenant_id,
            &before.waba_id,
            &[PhoneNumberId::new(D.pn).unwrap()],
        )
        .await
        .unwrap();
    let after = records.waba(&before.waba_id).await.unwrap().unwrap();
    assert_ne!(before.generation, after.generation);
    assert_eq!(rig.round(backend).await.recorded, 0);
    // A new token a week after, expiring 3 days from then.
    rig.clock.advance(Duration::from_hours(7 * 24));
    rig.store(D, Some(3 * DAY)).await;
    let again = rig.round(backend).await;
    assert_eq!(again.recorded, 1, "{again:?}");
    assert_eq!(polled(backend, D.id).await.len(), 2);
    // The one stored expired is never told, however many rounds.
    assert!(polled(backend, E.id).await.is_empty());
    assert_eq!(rig.round(backend).await.recorded, 0);
    assert_eq!(rig.observer.told("recorded"), 2);
}

/// One replica's turn at a time: a round on a replica that does not have
/// it records nothing and says so (`None`); the event does not depend on
/// it, since the key records it once: a round that does have it after the
/// first's turn ended finds the token told already. Decisive: the lock's
/// name, and the key (a second replica's round telling it again).
pub async fn a_round_without_the_turn_does_nothing(backend: &dyn Backend) {
    const F: Holder = Holder {
        id: "te-tenant-f",
        waba: "102290129346006",
        pn: "106540352246016",
    };
    let records = backend.records();
    hold(records.as_ref(), F).await;
    let rig = rig(backend);
    rig.store(F, Some(DAY)).await;
    let turn = backend
        .leader_lock()
        .try_exclusive(TOKEN_EXPIRY, TOKEN_EXPIRY_LEASE)
        .await
        .unwrap()
        .expect("the turn was free");
    assert_eq!(
        round(&rig.authz, backend, rig.observer.as_ref(), LEAD).await,
        None
    );
    assert!(polled(backend, F.id).await.is_empty());
    turn.release().await.unwrap();
    assert_eq!(rig.round(backend).await.recorded, 1);
    // Another replica: its own authorizer and vault over the same bundle.
    let other = self::rig(backend);
    other.clock.set(rig.clock.now());
    let told = other.round(backend).await;
    assert!(told.due >= 1, "{told:?}");
    assert_eq!(told.recorded, 0, "{told:?}");
    assert_eq!(polled(backend, F.id).await.len(), 1);
}

/// Tenant A's WABA, whose token is due, is unbound and attached to tenant B
/// (with a token of its own, expiring at the very second A's did) after the
/// round read the binding and the token: the round tells nobody (nothing
/// recorded, not even operator-only), and the next round tells B alone.
/// Decisive: the binding read again after the vault in
/// `Authorizer::tokens_expiring` (deleted: the round records A's token for
/// A's tenant on a binding that is B's now).
pub async fn a_waba_that_moves_while_its_token_is_read_is_told_to_nobody(backend: &dyn Backend) {
    const A: Holder = Holder {
        id: "te-mover-a",
        waba: "102290129346101",
        pn: "106540352246111",
    };
    const B: Holder = Holder {
        id: "te-mover-b",
        waba: "102290129346109",
        pn: "106540352246119",
    };
    let records = backend.records();
    hold(records.as_ref(), A).await;
    let tenant_b = TenantId::parse(B.id).unwrap();
    records.create_tenant(&tenant_b, "").await.unwrap();
    let plain = rig(backend);
    plain.store(A, Some(2 * DAY)).await;
    let waba = WabaId::new(A.waba).unwrap();
    let expiry = plain.clock.now() + 2 * DAY;
    let moved: Hook = {
        let (records, vault) = (backend.records(), plain.vault.clone());
        let (waba, pn) = (waba.clone(), PhoneNumberId::new(A.pn).unwrap());
        Box::pin(async move {
            // An unbind deletes the token, then the binding; an attach binds,
            // then stores.
            vault.delete(&waba).await.unwrap();
            assert!(records.unbind_waba(&waba).await.unwrap());
            records
                .bind_waba(&tenant_b, &waba, std::slice::from_ref(&pn))
                .await
                .unwrap();
            vault
                .store(
                    &StoredBusinessToken::new(waba, AccessToken::new(SECRET))
                        .phone_number_ids([pn])
                        .expires_at(expiry),
                )
                .await
                .unwrap();
        })
    };
    let hooked = Arc::new(Hooked {
        inner: backend.kv(),
        waba: A.waba.to_owned(),
        after_read: Mutex::new(Some(moved)),
        garbage: false,
    });
    let rig = rig_reading(backend, hooked);
    rig.clock.set(plain.clock.now());
    let during = rig.round(backend).await;
    // (`due` also counts the tokens of the earlier cases, told already.)
    assert_eq!((during.recorded, during.failed), (0, 0), "{during:?}");
    assert!(polled(backend, A.id).await.is_empty());
    assert!(polled(backend, B.id).await.is_empty());
    // The next round: B's, at the same second A's token expired.
    let next = rig.round(backend).await;
    assert_eq!((next.recorded, next.failed), (1, 0), "{next:?}");
    assert!(polled(backend, A.id).await.is_empty());
    let of_b = polled(backend, B.id).await;
    assert_eq!(of_b.len(), 1, "{of_b:?}");
    assert!(of_b[0].data.contains(A.waba), "{of_b:?}");
    assert_eq!(rig.observer.told("recorded"), 1);
}

/// A WABA whose record cannot be read (corrupt) is counted `failed`, once
/// per round, and told to nobody; the round goes on to the others, whose
/// events are recorded, whichever side of it they sort on. Decisive: the
/// `Err` arm of `tokens_expiring` (a `return` or `?` there ends the round
/// before the later WABA), and the count.
pub async fn an_unreadable_record_neither_stops_the_round_nor_is_told(backend: &dyn Backend) {
    const BAD: Holder = Holder {
        id: "te-bad",
        waba: "102290129346202",
        pn: "106540352246212",
    };
    const BEFORE: Holder = Holder {
        id: "te-before",
        waba: "102290129346201",
        pn: "106540352246211",
    };
    const AFTER: Holder = Holder {
        id: "te-after",
        waba: "102290129346203",
        pn: "106540352246213",
    };
    let records = backend.records();
    for holder in [BAD, BEFORE, AFTER] {
        hold(records.as_ref(), holder).await;
    }
    let plain = rig(backend);
    for holder in [BAD, BEFORE, AFTER] {
        plain.store(holder, Some(DAY)).await;
    }
    let hooked = Arc::new(Hooked {
        inner: backend.kv(),
        waba: BAD.waba.to_owned(),
        after_read: Mutex::new(None),
        garbage: true,
    });
    let rig = rig_reading(backend, hooked);
    rig.clock.set(plain.clock.now());
    let done = rig.round(backend).await;
    assert_eq!((done.recorded, done.failed), (2, 1), "{done:?}");
    assert!(polled(backend, BAD.id).await.is_empty());
    assert_eq!(polled(backend, BEFORE.id).await.len(), 1);
    assert_eq!(polled(backend, AFTER.id).await.len(), 1);
    assert_eq!(rig.observer.told("failed"), 1);
    // Every round counts it again, and records nothing more.
    let again = rig.round(backend).await;
    assert_eq!((again.recorded, again.failed), (0, 1), "{again:?}");
    assert_eq!(rig.observer.told("failed"), 2);
}

/// More WABAs than a page (100) are all walked: the last one in byte order,
/// the only one due, is told. Decisive: the cursor of the walk (stopping
/// after a page tells nobody).
pub async fn a_walk_reaches_the_wabas_past_its_first_page(backend: &dyn Backend) {
    let tenant = TenantId::parse("te-bulk").unwrap();
    let records = backend.records();
    records.create_tenant(&tenant, "").await.unwrap();
    let rig = rig(backend);
    // 102 WABAs, sorting after every other case's; only the last is due.
    let ids: Vec<String> = (0..102).map(|n| format!("1022901293480{n:02}")).collect();
    for (n, id) in ids.iter().enumerate() {
        let waba = WabaId::new(id.as_str()).unwrap();
        let pn = PhoneNumberId::new(format!("1065403522480{n:02}")).unwrap();
        records
            .bind_waba(&tenant, &waba, std::slice::from_ref(&pn))
            .await
            .unwrap();
        let after = if n == 101 { 2 * DAY } else { 60 * DAY };
        rig.vault
            .store(
                &StoredBusinessToken::new(waba, AccessToken::new(SECRET))
                    .phone_number_ids([pn])
                    .expires_at(rig.clock.now() + after),
            )
            .await
            .unwrap();
    }
    let done = rig.round(backend).await;
    assert!(done.wabas >= 102, "{done:?}");
    // (The earlier cases' WABAs are walked too: one not told yet, the one
    // whose record was unreadable, is told now.)
    assert!(done.recorded >= 1 && done.failed == 0, "{done:?}");
    let told = polled(backend, "te-bulk").await;
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(told[0].data.contains(ids[101].as_str()), "{told:?}");
}

/// The WABA of tenant A, told its token's expiry, is moved to tenant B
/// (unbound, attached, a token stored expiring at the very second A's did):
/// B is told, for the tenant is in the event's key, and A is not told again.
/// Decisive: the tenant in `TokenExpiring::outbox_key` (left out, B's event
/// is A's key and is never recorded: B is not told).
pub async fn a_waba_moved_to_another_tenant_is_told_to_its_holder_for_the_same_expiry(
    backend: &dyn Backend,
) {
    const A: Holder = Holder {
        id: "te-moved-a",
        waba: "102290129346301",
        pn: "106540352246311",
    };
    const B: Holder = Holder {
        id: "te-moved-b",
        waba: "102290129346301",
        pn: "106540352246311",
    };
    let records = backend.records();
    hold(records.as_ref(), A).await;
    let rig = rig(backend);
    rig.store(A, Some(2 * DAY)).await;
    let expires_at = rig.clock.now() + 2 * DAY;
    assert_eq!(rig.round(backend).await.recorded, 1);
    assert_eq!(polled(backend, A.id).await.len(), 1);
    let waba = WabaId::new(A.waba).unwrap();
    let pn = PhoneNumberId::new(A.pn).unwrap();
    rig.vault.delete(&waba).await.unwrap();
    assert!(records.unbind_waba(&waba).await.unwrap());
    let tenant_b = TenantId::parse(B.id).unwrap();
    records.create_tenant(&tenant_b, "").await.unwrap();
    records
        .bind_waba(&tenant_b, &waba, std::slice::from_ref(&pn))
        .await
        .unwrap();
    rig.vault
        .store(
            &StoredBusinessToken::new(waba, AccessToken::new(SECRET))
                .phone_number_ids([pn])
                .expires_at(expires_at),
        )
        .await
        .unwrap();
    let after = rig.round(backend).await;
    assert_eq!((after.recorded, after.failed), (1, 0), "{after:?}");
    assert_eq!(polled(backend, A.id).await.len(), 1);
    let of_b = polled(backend, B.id).await;
    assert_eq!(of_b.len(), 1, "{of_b:?}");
    assert_eq!(rig.round(backend).await.recorded, 0);
}

/// The outbox's own check, at the write: an event built under a binding
/// that is not the WABA's any more (the round's two reads passed, then the
/// WABA moved to tenant B, or was unbound and attached to its own tenant
/// again, a binding made after the one the token was read under) is
/// recorded operator-only: neither tenant polls it. Decisive: the guard's
/// WABA and its date in `TokenExpiring::row` (`route_guard: None` makes it
/// A's, and `not_after: None` admits the binding made after).
pub async fn an_event_for_a_binding_that_moved_before_the_insert_reaches_neither_tenant(
    backend: &dyn Backend,
) {
    const A: Holder = Holder {
        id: "te-late-a",
        waba: "102290129346401",
        pn: "106540352246411",
    };
    const C: Holder = Holder {
        id: "te-late-c",
        waba: "102290129346402",
        pn: "106540352246412",
    };
    const B: &str = "te-late-b";
    let records = backend.records();
    hold(records.as_ref(), A).await;
    hold(records.as_ref(), C).await;
    records
        .create_tenant(&TenantId::parse(B).unwrap(), "")
        .await
        .unwrap();
    let events = number_events(backend);
    let expires_at = OffsetDateTime::now_utc() + 2 * DAY;
    let under = |waba: &str| {
        let records = backend.records();
        let waba = WabaId::new(waba).unwrap();
        async move { records.waba(&waba).await.unwrap().unwrap().epoch() }
    };
    let (of_a, of_c) = (under(A.waba).await, under(C.waba).await);

    // A's WABA goes to tenant B; C's is unbound and attached to C again,
    // a second on (the binding's date is in whole seconds).
    for waba in [A.waba, C.waba] {
        records
            .unbind_waba(&WabaId::new(waba).unwrap())
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(2100)).await;
    records
        .bind_waba(
            &TenantId::parse(B).unwrap(),
            &WabaId::new(A.waba).unwrap(),
            &[PhoneNumberId::new(A.pn).unwrap()],
        )
        .await
        .unwrap();
    records
        .bind_waba(
            &TenantId::parse(C.id).unwrap(),
            &WabaId::new(C.waba).unwrap(),
            &[PhoneNumberId::new(C.pn).unwrap()],
        )
        .await
        .unwrap();
    for epoch in [of_a, of_c] {
        let tenant = epoch.tenant_id.clone();
        let row = TokenExpiring { epoch, expires_at }
            .row(events.ids())
            .unwrap();
        // Stored (a sequence), and told to nobody.
        assert!(events.record_row(&row, &tenant).await.unwrap().is_some());
    }
    for tenant in [A.id, B, C.id] {
        let told = polled(backend, tenant).await;
        assert!(told.is_empty(), "{tenant}: {told:?}");
    }
}

/// Why the lead is bound by the outbox's retention (`lead_fits_retention`):
/// the event's key lives as long as its row, so a purge of the row (here
/// all of them, as one that ran with a retention shorter than the lead
/// would, mid-way through a token's lead time) lets a token still due be
/// told again by the next round. Pins the premise of the
/// service's configuration check. Decisive: the key being kept anywhere but
/// in the outbox row (the second round would record nothing).
pub async fn a_purged_event_is_told_again_which_is_why_the_lead_may_not_exceed_the_retention(
    backend: &dyn Backend,
) {
    const P: Holder = Holder {
        id: "te-purge-p",
        waba: "102290129346501",
        pn: "106540352246511",
    };
    let records = backend.records();
    hold(records.as_ref(), P).await;
    let rig = rig(backend);
    rig.store(P, Some(2 * DAY)).await;
    assert!(rig.round(backend).await.recorded >= 1);
    assert_eq!(rig.round(backend).await.recorded, 0);
    backend.outbox().purge(Duration::ZERO).await.unwrap();
    assert!(polled(backend, P.id).await.is_empty());
    // (The purge took every case's rows: the others' tokens still due are
    // told again too.)
    assert!(rig.round(backend).await.recorded >= 1);
    assert_eq!(polled(backend, P.id).await.len(), 1);
}
