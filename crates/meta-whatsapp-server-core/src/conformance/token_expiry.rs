//! The token-expiry event ([`crate::token_expiry`], roadmap M3e) over a
//! backend's bundle: a round walks the bindings and the vault the bundle
//! shares, records `token_expiring` through [`NumberEvents`](crate::number_events::NumberEvents) into the
//! bundle's outbox for the token's tenant only, once per token and expiry
//! (a second round, and a round on another replica, record nothing), again
//! for a token stored anew, never for a token outside the lead time, one
//! that never expires or one already expired; the turn is one replica's at
//! a time; and a token's text is nowhere in what is recorded. The clock is
//! a fake one the vault and the authorizer share, moved across the lead.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::clock::{Clock, ManualClock};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use serde_json::json;
use time::OffsetDateTime;

use super::number_events::{number_events, polled};
use super::webhooks::{Holder, hold};
use crate::authz::Authorizer;
use crate::backend::Backend;
use crate::observe::Observer;
use crate::store::{TOKEN_EXPIRY, TOKEN_EXPIRY_LEASE};
use crate::token_expiry::{Round, round};

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    a_token_inside_the_lead_is_told_to_its_tenant_once(backend).await;
    a_token_stored_again_is_told_again_and_one_expired_is_not(backend).await;
    a_round_without_the_turn_does_nothing(backend).await;
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

/// What a case runs on: the vault and the authorizer over one fake clock.
struct Rig {
    clock: ManualClock,
    vault: TokenVault,
    authz: Authorizer,
    observer: Arc<Counting>,
}

fn rig(backend: &dyn Backend) -> Rig {
    let clock = ManualClock::new(OffsetDateTime::now_utc());
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("token-expiry-suite").unwrap()),
    )
    .unwrap()
    .with_clock(Arc::new(clock.clone()));
    let client = Client::builder()
        .transport(ScriptedTransport::new())
        .build()
        .unwrap();
    let authz = Authorizer::new(backend.records(), vault.clone(), client)
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
