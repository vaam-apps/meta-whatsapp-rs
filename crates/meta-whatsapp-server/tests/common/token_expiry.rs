//! Roadmap M3e's decisive case, shared by `token_expiry.rs` (memory) and
//! `live_postgres.rs` (Postgres): the service's own token-expiry task
//! (`AppState::run_token_expiry`) on a fake clock the vault and the
//! authorizer share. A token inside the lead time is told to its tenant
//! once and to no other (polled over HTTP), a token outside it is told to
//! nobody, a later round tells nobody again, `/metrics` counts the event,
//! and no token's text is in an event or in `/metrics`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken;
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_server::model::Scope;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::{Call, Harness};

const A: &str = "merchant-a";
const B: &str = "merchant-b";
const WABA_A: &str = "102290129340398";
const WABA_B: &str = "102290129340399";
const PN_A: &str = "106540352242922";
const PN_B: &str = "106540352242923";
/// The tokens' text, which no event or metric may hold.
const TOKEN_A: &str = "EAAG-token-of-a-that-expires-soon";
const TOKEN_B: &str = "EAAG-token-of-b-that-expires-later";
const LEAD: Duration = Duration::from_hours(7 * 24);
const DAY: Duration = Duration::from_hours(24);

async fn store(h: &Harness, tenant: &str, waba: &str, pn: &str, token: &str, in_days: i64) {
    h.connect(tenant, waba, &[pn], token).await;
    let expires_at = h.clock.now() + time::Duration::days(in_days);
    h.vault
        .store(
            &StoredBusinessToken::new(WabaId::new(waba).unwrap(), AccessToken::new(token))
                .phone_number_ids([PhoneNumberId::new(pn).unwrap()])
                .expires_at(expires_at),
        )
        .await
        .unwrap();
}

/// The events `key` polls: `(type, data)`.
async fn polled(h: &Harness, key: &str) -> Vec<(String, Value)> {
    let reply = h.call(Call::get("/v1/events").key(key)).await;
    assert_eq!(reply.status.as_u16(), 200, "{}", reply.text);
    assert!(!reply.text.contains(TOKEN_A) && !reply.text.contains(TOKEN_B));
    reply.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["type"].as_str().unwrap().to_owned(), e["data"].clone()))
        .collect()
}

/// The value of the `recorded` counter in `/metrics`.
async fn recorded(h: &Harness) -> u64 {
    let reply = h.call(Call::get("/metrics")).await;
    assert!(!reply.text.contains(TOKEN_A) && !reply.text.contains(TOKEN_B));
    reply
        .text
        .lines()
        .find_map(|line| {
            line.strip_prefix("wa_server_token_expiry_events_total{outcome=\"recorded\"} ")
        })
        .map_or(0, |n| n.trim().parse().unwrap())
}

async fn until(what: &str, mut done: impl AsyncFnMut() -> bool) {
    let start = Instant::now();
    while !done().await {
        assert!(start.elapsed() < Duration::from_secs(10), "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// See the module docs. Each removal fails it: the comparison with the
/// lead (B told on the first round), the event's key (A told by a later
/// round), the routing (B polling A's event), the counter, the expired
/// case (A told again 25 days on).
pub async fn a_token_inside_the_lead_is_told_to_its_tenant_once(h: &Harness) {
    h.tenant(A).await;
    h.tenant(B).await;
    let (key_a, key_b) = (
        h.tenant_key(A, &[Scope::Events]).await,
        h.tenant_key(B, &[Scope::Events]).await,
    );
    store(h, A, WABA_A, PN_A, TOKEN_A, 3).await;
    store(h, B, WABA_B, PN_B, TOKEN_B, 30).await;
    let expected = (h.clock.now() + time::Duration::days(3))
        .replace_nanosecond(0)
        .unwrap()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();

    let stop = Arc::new(Notify::new());
    let task = {
        let (state, backend, stop) = (h.state.clone(), h.backend.clone(), stop.clone());
        tokio::spawn(async move {
            state
                .run_token_expiry(backend, LEAD, Duration::from_millis(20), async move {
                    stop.notified().await;
                })
                .await;
        })
    };
    until("A is told", async || !polled(h, &key_a).await.is_empty()).await;
    // Many rounds later (every 20 ms): still once.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        polled(h, &key_a).await,
        vec![(
            "token_expiring".to_owned(),
            json!({"event": "token_expiring", "waba_id": WABA_A, "expires_at": expected})
        )]
    );
    assert_eq!(polled(h, &key_b).await, vec![]);
    assert_eq!(recorded(h).await, 1);

    // 25 days on: A's token expired 22 days ago (not told again), B's has 5
    // days left (told).
    h.clock.advance(25 * DAY);
    until("B is told", async || !polled(h, &key_b).await.is_empty()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let of_b = polled(h, &key_b).await;
    assert_eq!(of_b.len(), 1, "{of_b:?}");
    assert_eq!(of_b[0].0, "token_expiring");
    assert_eq!(of_b[0].1["waba_id"], WABA_B);
    assert_eq!(polled(h, &key_a).await.len(), 1);
    assert_eq!(recorded(h).await, 2);

    stop.notify_one();
    task.await.unwrap();
}
