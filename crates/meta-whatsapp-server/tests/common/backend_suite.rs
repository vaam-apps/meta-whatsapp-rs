//! What every `Backend` must do across calls to its accessors, run on
//! memory (`tests/store.rs`) and on Postgres (`tests/live_postgres.rs`).

use meta_whatsapp_rs::core::store::{Expiry, StoreKey};
use meta_whatsapp_server::model::TenantId;
use meta_whatsapp_server::store::Backend;

/// Each accessor hands out the same data on every call: what one handle
/// writes, another reads. Decisive: a backend that builds a fresh
/// in-process store per call (`kv()`, `records()`).
pub async fn run(backend: &dyn Backend) {
    let key = StoreKey::new("wa.test.backend", "same-data");
    let written = backend
        .kv()
        .put(&key, b"value".to_vec(), Expiry::Never)
        .await
        .unwrap();
    let read = backend
        .kv()
        .get(&key)
        .await
        .unwrap()
        .expect("written through another kv() handle");
    assert_eq!(read.value, b"value");
    assert_eq!(read.version, written);

    let tenant = TenantId::parse("backend-same-data").unwrap();
    backend
        .records()
        .create_tenant(&tenant, "")
        .await
        .unwrap()
        .expect("a new tenant");
    assert!(
        backend.records().tenant(&tenant).await.unwrap().is_some(),
        "created through another records() handle"
    );
    every_other_accessor(backend, &tenant).await;
}

/// Roadmap S2, the rest of the bundle: the idempotency records, the
/// outbox, the leader lock and the conversation store also hand out the
/// same data on every call, and the outbox reads the bindings `records()`
/// wrote (its insert keeps a tenant only while its guarded binding holds:
/// a binding in another database would leave the row operator-only).
/// Decisive: a backend building any of them anew per call.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn every_other_accessor(backend: &dyn Backend, tenant: &TenantId) {
    use meta_whatsapp_rs::core::ids::{MessageId, PhoneNumberId, WabaId};
    use meta_whatsapp_rs::core::store::{ConversationKey, Direction, StoredMessage};
    use meta_whatsapp_server::model::{BindOutcome, IdempotencyClaim, IdempotencyKey};
    use meta_whatsapp_server::store::HOUSEKEEPING_LEASE;
    use meta_whatsapp_server::store::events::{EventQuery, GuardedBinding, NewEvent, RouteGuard};

    let key = IdempotencyKey::parse("order:same-data").unwrap();
    let minute = std::time::Duration::from_secs(60);
    let claim = |claim_id: &'static str| {
        let key = key.clone();
        async move {
            backend
                .idempotency()
                .claim_idempotency_key(tenant, &key, &[1; 32], claim_id, minute, minute)
                .await
                .unwrap()
        }
    };
    assert_eq!(claim("first").await, IdempotencyClaim::Claimed);
    assert!(
        matches!(claim("second").await, IdempotencyClaim::Existing(_)),
        "claimed through another idempotency() handle"
    );

    let waba = WabaId::new("backend-same-data-waba");
    assert_eq!(
        backend
            .records()
            .bind_waba(tenant, &waba, &[])
            .await
            .unwrap(),
        BindOutcome::Bound
    );
    let event = NewEvent {
        id: format!("evt_same_data_{}", super::unique()),
        dedup_key: None,
        dedup_window: None,
        tenant: Some(tenant.clone()),
        route_guard: Some(RouteGuard {
            binding: GuardedBinding::Waba(waba.clone()),
            not_after: None,
        }),
        phone_number_id: None,
        waba_id: Some(waba),
        event_type: "template_status_updated".to_owned(),
        data: "{}".to_owned(),
    };
    backend.outbox().insert(&event).await.unwrap().unwrap();
    let page = backend
        .outbox()
        .page(&EventQuery {
            tenant: tenant.clone(),
            after: None,
            types: None,
            phone_number_id: None,
            limit: 10,
            max_bytes: 1024,
        })
        .await
        .unwrap();
    let ids: Vec<&str> = page.events.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        ids,
        [event.id.as_str()],
        "inserted through another outbox() handle, checked against records()"
    );

    // Advisory locks are the database's, not a schema's: a name of its own.
    let name = format!("backend-same-data-{}", super::unique());
    let turn = backend
        .leader_lock()
        .try_exclusive(&name, HOUSEKEEPING_LEASE)
        .await
        .unwrap()
        .expect("nobody holds it");
    assert!(
        backend
            .leader_lock()
            .try_exclusive(&name, HOUSEKEEPING_LEASE)
            .await
            .unwrap()
            .is_none(),
        "held through another leader_lock() handle"
    );
    turn.release().await.unwrap();

    let conversation = ConversationKey::new(PhoneNumberId::new("106540352242922"), "16505551234");
    let message = StoredMessage::tombstone(
        &conversation,
        &MessageId::new("wamid.same-data"),
        Direction::Inbound,
        time::OffsetDateTime::now_utc(),
    );
    assert!(backend.conversations().append(message).await.unwrap());
    let stored = backend
        .conversations()
        .messages(&conversation, None, 10)
        .await
        .unwrap();
    assert_eq!(
        stored.len(),
        1,
        "appended through another conversations() handle"
    );
}
