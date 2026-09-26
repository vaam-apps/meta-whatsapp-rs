//! What every `Store` must do, run on memory (`tests/store.rs`) and on
//! Postgres (`tests/live_postgres.rs`).

use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_server::model::{
    AllowedTenants, BindOutcome, DeleteTenantOutcome, KeyOwner, KeyScope, NewApiKey, NumberStatus,
    PageRequest, Scope, TenantId, TenantStatus,
};
use meta_whatsapp_server::store::Store;
use time::{Duration, OffsetDateTime};

fn id(s: &str) -> TenantId {
    TenantId::parse(s).unwrap()
}

fn page(after: Option<&str>, limit: usize) -> PageRequest {
    PageRequest {
        after: after.map(str::to_owned),
        limit,
    }
}

fn pns(ids: &[&str]) -> Vec<PhoneNumberId> {
    ids.iter().map(|n| PhoneNumberId::new(*n)).collect()
}

fn key(key_id: &str, owner: KeyOwner) -> NewApiKey {
    NewApiKey {
        key_id: key_id.to_owned(),
        secret_sha256: [7; 32],
        owner,
        scopes: vec![Scope::Numbers, Scope::Send],
        name: "label".to_owned(),
        expires_at: None,
    }
}

/// Tenants: create once, read, page, update, suspend, delete with keys.
pub async fn tenants(store: &dyn Store) {
    let a = id("tenant-a");
    let created = store.create_tenant(&a, "A").await.unwrap().unwrap();
    assert_eq!(created.id, a);
    assert_eq!(created.name, "A");
    assert_eq!(created.status, TenantStatus::Active);
    assert!(
        store.create_tenant(&a, "again").await.unwrap().is_none(),
        "ids are unique"
    );
    assert_eq!(store.tenant(&a).await.unwrap().unwrap().name, "A");
    assert!(store.tenant(&id("nobody")).await.unwrap().is_none());

    for t in ["tenant-b", "tenant-c", "tenant-d"] {
        store.create_tenant(&id(t), "").await.unwrap().unwrap();
    }
    let first = store.tenants(&page(None, 2)).await.unwrap();
    let ids: Vec<&str> = first.items.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["tenant-a", "tenant-b"]);
    assert_eq!(first.next_after.as_deref(), Some("tenant-b"));
    let second = store
        .tenants(&page(first.next_after.as_deref(), 2))
        .await
        .unwrap();
    let ids: Vec<&str> = second.items.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["tenant-c", "tenant-d"]);
    assert_eq!(second.next_after, None, "no page after the last");

    let suspended = store
        .update_tenant(&a, None, Some(TenantStatus::Suspended))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(suspended.status, TenantStatus::Suspended);
    assert_eq!(suspended.name, "A", "a status change keeps the name");
    let renamed = store
        .update_tenant(&a, Some("Renamed"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renamed.name, "Renamed");
    assert_eq!(renamed.status, TenantStatus::Suspended);
    assert!(
        store
            .update_tenant(&id("nobody"), Some("x"), None)
            .await
            .unwrap()
            .is_none()
    );

    // Deleting takes its keys, and needs its WABAs gone.
    store
        .insert_key(&key("k-of-d", KeyOwner::Tenant(id("tenant-d"))))
        .await
        .unwrap()
        .unwrap();

    store
        .bind_waba(&id("tenant-d"), &WabaId::new("waba-d"), &pns(&["pn-d"]))
        .await
        .unwrap();
    assert_eq!(
        store.delete_tenant(&id("tenant-d")).await.unwrap(),
        DeleteTenantOutcome::HasWabas
    );
    assert!(store.unbind_waba(&WabaId::new("waba-d")).await.unwrap());
    assert_eq!(
        store.delete_tenant(&id("tenant-d")).await.unwrap(),
        DeleteTenantOutcome::Deleted
    );
    assert!(
        store.key("k-of-d").await.unwrap().is_none(),
        "keys go with the tenant"
    );

    assert_eq!(
        store.delete_tenant(&id("tenant-d")).await.unwrap(),
        DeleteTenantOutcome::NotFound
    );
}

/// Keys: stored as given, unique ids, listed by scope, revoked once.
pub async fn keys(store: &dyn Store) {
    let t = id("keys-tenant");
    store.create_tenant(&t, "").await.unwrap().unwrap();
    let expires = OffsetDateTime::now_utc() + Duration::days(1);
    let mut tenant_key = key("k1", KeyOwner::Tenant(t.clone()));
    tenant_key.expires_at = Some(expires);
    let stored = store.insert_key(&tenant_key).await.unwrap().unwrap();
    assert_eq!(stored.key_id, "k1");
    assert_eq!(stored.secret_sha256, [7; 32]);
    assert_eq!(stored.owner, KeyOwner::Tenant(t.clone()));
    assert_eq!(stored.scopes, [Scope::Numbers, Scope::Send]);
    assert_eq!(stored.name, "label");
    assert_eq!(
        stored.expires_at.map(OffsetDateTime::unix_timestamp),
        Some(expires.unix_timestamp())
    );
    assert!(stored.revoked_at.is_none());
    assert!(
        store
            .insert_key(&key("k1", KeyOwner::Admin))
            .await
            .unwrap()
            .is_none(),
        "ids are unique"
    );

    let platform = KeyOwner::Platform(AllowedTenants::Only(vec![id("p1"), id("p2")]));
    store
        .insert_key(&key("k2", platform.clone()))
        .await
        .unwrap()
        .unwrap();
    store
        .insert_key(&key("k3", KeyOwner::Platform(AllowedTenants::All)))
        .await
        .unwrap()
        .unwrap();
    store
        .insert_key(&key("k4", KeyOwner::Admin))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(store.key("k2").await.unwrap().unwrap().owner, platform);
    assert_eq!(
        store.key("k3").await.unwrap().unwrap().owner,
        KeyOwner::Platform(AllowedTenants::All)
    );

    let listed = |scope: KeyScope| async move {
        store
            .keys(&scope, &page(None, 10))
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|k| k.key_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(listed(KeyScope::Tenant(t.clone())).await, ["k1"]);
    assert_eq!(listed(KeyScope::Platform).await, ["k2", "k3"]);
    assert_eq!(listed(KeyScope::Admin).await, ["k4"]);

    // Revocation is scoped: a tenant's revocation cannot touch a platform
    // key, and the first revocation time stays.
    assert!(
        !store
            .revoke_key(&KeyScope::Tenant(t.clone()), "k2")
            .await
            .unwrap()
    );
    assert!(store.key("k2").await.unwrap().unwrap().revoked_at.is_none());
    assert!(store.revoke_key(&KeyScope::Platform, "k2").await.unwrap());
    let first = store.key("k2").await.unwrap().unwrap().revoked_at.unwrap();
    assert!(store.revoke_key(&KeyScope::Platform, "k2").await.unwrap());
    assert_eq!(
        store.key("k2").await.unwrap().unwrap().revoked_at,
        Some(first)
    );
    assert!(!store.revoke_key(&KeyScope::Admin, "missing").await.unwrap());

    store.touch_key("k4").await.unwrap();
    let used = store
        .key("k4")
        .await
        .unwrap()
        .unwrap()
        .last_used_at
        .unwrap();
    store.touch_key("k4").await.unwrap();
    assert_eq!(
        store.key("k4").await.unwrap().unwrap().last_used_at,
        Some(used),
        "at most once a minute"
    );
}

/// Bindings: bound once, refused to another tenant (D4), refreshed,
/// unbound, statuses.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
pub async fn bindings(store: &dyn Store) {
    let a = id("bind-a");
    let b = id("bind-b");
    store.create_tenant(&a, "").await.unwrap().unwrap();
    store.create_tenant(&b, "").await.unwrap().unwrap();
    let waba = WabaId::new("w1");
    assert_eq!(
        store
            .bind_waba(&a, &waba, &pns(&["n1", "n2"]))
            .await
            .unwrap(),
        BindOutcome::Bound
    );
    let bound = store.waba(&waba).await.unwrap().unwrap();
    assert_eq!(bound.tenant_id, a);
    let n1 = store
        .number(&PhoneNumberId::new("n1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (n1.waba_id.as_str(), &n1.tenant_id, n1.status),
        ("w1", &a, NumberStatus::Connected)
    );

    // D4: another tenant is refused, and nothing changes: whether it
    // names A's numbers, only numbers nobody has, or none (the WABA's own
    // check, not only the numbers').
    for numbers in [&["n1", "n9"][..], &["n9"][..], &[][..]] {
        assert_eq!(
            store.bind_waba(&b, &waba, &pns(numbers)).await.unwrap(),
            BindOutcome::OwnedByAnotherTenant,
            "{numbers:?}"
        );
    }
    assert_eq!(store.waba(&waba).await.unwrap().unwrap().tenant_id, a);
    assert!(
        store
            .number(&PhoneNumberId::new("n9"))
            .await
            .unwrap()
            .is_none()
    );
    // A number of A's under another WABA claimed by B: refused too.
    assert_eq!(
        store
            .bind_waba(&b, &WabaId::new("w2"), &pns(&["n2"]))
            .await
            .unwrap(),
        BindOutcome::OwnedByAnotherTenant
    );
    assert!(
        store.waba(&WabaId::new("w2")).await.unwrap().is_none(),
        "rolled back"
    );
    assert_eq!(
        store
            .number(&PhoneNumberId::new("n2"))
            .await
            .unwrap()
            .unwrap()
            .tenant_id,
        a
    );

    // Status, then a re-bind by the owner: the listed numbers are exactly
    // the bound ones, connected again.
    store
        .set_waba_status(&waba, NumberStatus::ReconnectRequired)
        .await
        .unwrap();
    assert_eq!(
        store
            .number(&PhoneNumberId::new("n1"))
            .await
            .unwrap()
            .unwrap()
            .status,
        NumberStatus::ReconnectRequired
    );
    assert_eq!(
        store
            .bind_waba(&a, &waba, &pns(&["n2", "n3"]))
            .await
            .unwrap(),
        BindOutcome::Bound
    );
    assert!(
        store
            .number(&PhoneNumberId::new("n1"))
            .await
            .unwrap()
            .is_none(),
        "dropped"
    );
    assert_eq!(
        store
            .number(&PhoneNumberId::new("n2"))
            .await
            .unwrap()
            .unwrap()
            .status,
        NumberStatus::Connected
    );
    let numbers: Vec<String> = store
        .numbers(&a, &page(None, 10))
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|n| n.phone_number_id.into_inner())
        .collect();
    assert_eq!(numbers, ["n2", "n3"]);
    assert!(
        store
            .numbers(&b, &page(None, 10))
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let wabas = store.wabas(&a, &page(None, 10)).await.unwrap();
    assert_eq!(wabas.items.len(), 1);
    let of_waba: Vec<String> = store
        .waba_numbers(&waba)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.phone_number_id.into_inner())
        .collect();
    assert_eq!(of_waba, ["n2", "n3"]);
    // Every tenant's, paged.
    store.bind_waba(&b, &WabaId::new("w0"), &[]).await.unwrap();
    let first = store.all_wabas(&page(None, 1)).await.unwrap();
    assert_eq!(first.items[0].waba_id.as_str(), "w0");
    let rest = store
        .all_wabas(&page(first.next_after.as_deref(), 10))
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 1);
    assert_eq!(rest.items[0].waba_id, waba);
    assert!(store.unbind_waba(&WabaId::new("w0")).await.unwrap());

    // Unbind frees the WABA for B.
    assert!(store.unbind_waba(&waba).await.unwrap());
    assert!(!store.unbind_waba(&waba).await.unwrap());
    assert!(
        store
            .number(&PhoneNumberId::new("n2"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.bind_waba(&b, &waba, &pns(&["n2"])).await.unwrap(),
        BindOutcome::Bound
    );
    assert_eq!(store.waba(&waba).await.unwrap().unwrap().tenant_id, b);
}

/// Deleting a tenant takes it out of every platform key's allowed tenants
/// (security review M4): a tenant created later with the same id is not
/// theirs. A key allowed every tenant stays so.
pub async fn deleting_a_tenant_revokes_platform_allowances(store: &dyn Store) {
    for t in ["gone", "stays"] {
        store.create_tenant(&id(t), "").await.unwrap().unwrap();
    }
    for (key_id, allowed) in [
        (
            "p-both",
            AllowedTenants::Only(vec![id("stays"), id("gone")]),
        ),
        ("p-gone", AllowedTenants::Only(vec![id("gone")])),
        ("p-all", AllowedTenants::All),
    ] {
        store
            .insert_key(&key(key_id, KeyOwner::Platform(allowed)))
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        store.delete_tenant(&id("gone")).await.unwrap(),
        DeleteTenantOutcome::Deleted
    );
    store.create_tenant(&id("gone"), "").await.unwrap().unwrap();
    for (key_id, allowed) in [
        ("p-both", AllowedTenants::Only(vec![id("stays")])),
        ("p-gone", AllowedTenants::Only(Vec::new())),
        ("p-all", AllowedTenants::All),
    ] {
        assert_eq!(
            store.key(key_id).await.unwrap().unwrap().owner,
            KeyOwner::Platform(allowed),
            "{key_id}"
        );
    }
}

/// Every case.
/// Idempotency keys: claimed once, found by a repeat, completed with the
/// answer byte for byte, released, scoped to their tenant, leased,
/// expired, purged, and gone with their tenant.
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
pub async fn idempotency(store: &dyn Store) {
    use std::time::Duration as StdDuration;

    use meta_whatsapp_server::model::{
        IdempotencyClaim, IdempotencyKey, IdempotencyRecord, IdempotencyState,
    };
    let a = id("idem-a");
    let b = id("idem-b");
    for t in [&a, &b] {
        store.create_tenant(t, "").await.unwrap().unwrap();
    }
    let key = IdempotencyKey::parse("order:1234:shipped").unwrap();
    let hour = StdDuration::from_secs(3600);
    let minute = StdDuration::from_secs(60);
    let claim = |tenant, fingerprint: [u8; 32], claim_id: &'static str, lease, ttl| {
        let key = key.clone();
        async move {
            store
                .claim_idempotency_key(tenant, &key, &fingerprint, claim_id, lease, ttl)
                .await
                .unwrap()
        }
    };
    assert_eq!(
        claim(&a, [1; 32], "c1", minute, hour).await,
        IdempotencyClaim::Claimed
    );
    // A repeat finds it running, with the first request's fingerprint.
    assert_eq!(
        claim(&a, [2; 32], "c2", minute, hour).await,
        IdempotencyClaim::Existing(IdempotencyRecord {
            fingerprint: [1; 32],
            state: IdempotencyState::InProgress {
                lease_expired: false
            },
        })
    );
    // Another tenant's same key is another record.
    assert_eq!(
        claim(&b, [1; 32], "c3", minute, hour).await,
        IdempotencyClaim::Claimed
    );
    // Only the holder settles it.
    assert!(
        !store
            .complete_idempotency_key(&a, &key, "c2", 202, b"{}")
            .await
            .unwrap()
    );
    assert!(!store.release_idempotency_key(&a, &key, "c3").await.unwrap());
    let body = br#"{"message_id": "wamid.X", "contacts": []}"#;
    assert!(
        store
            .complete_idempotency_key(&a, &key, "c1", 202, body)
            .await
            .unwrap()
    );
    assert_eq!(
        claim(&a, [1; 32], "c4", minute, hour).await,
        IdempotencyClaim::Existing(IdempotencyRecord {
            fingerprint: [1; 32],
            state: IdempotencyState::Completed {
                status: 202,
                body: body.to_vec()
            },
        })
    );
    // Released: free again.
    assert!(store.release_idempotency_key(&b, &key, "c3").await.unwrap());
    assert_eq!(
        claim(&b, [9; 32], "c5", minute, hour).await,
        IdempotencyClaim::Claimed
    );
    // A lease that ran out: the outcome is unknown.
    let leased = IdempotencyKey::parse("leased").unwrap();
    assert_eq!(
        store
            .claim_idempotency_key(
                &a,
                &leased,
                &[1; 32],
                "c6",
                StdDuration::from_millis(1),
                hour
            )
            .await
            .unwrap(),
        IdempotencyClaim::Claimed
    );
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    assert_eq!(
        store
            .claim_idempotency_key(&a, &leased, &[1; 32], "c7", minute, hour)
            .await
            .unwrap(),
        IdempotencyClaim::Existing(IdempotencyRecord {
            fingerprint: [1; 32],
            state: IdempotencyState::InProgress {
                lease_expired: true
            },
        })
    );
    // An expired record is replaced, then purged when none replaces it.
    let short = IdempotencyKey::parse("short").unwrap();
    let old = IdempotencyKey::parse("old").unwrap();
    for (k, c) in [(&short, "c8"), (&old, "c9")] {
        store
            .claim_idempotency_key(
                &a,
                k,
                &[1; 32],
                c,
                StdDuration::from_millis(1),
                StdDuration::from_millis(20),
            )
            .await
            .unwrap();
    }
    assert!(
        store
            .complete_idempotency_key(&a, &short, "c8", 504, b"{}")
            .await
            .unwrap()
    );
    tokio::time::sleep(StdDuration::from_millis(60)).await;
    assert_eq!(
        store
            .claim_idempotency_key(&a, &short, &[3; 32], "c10", minute, hour)
            .await
            .unwrap(),
        IdempotencyClaim::Claimed
    );
    assert!(
        store.purge_idempotency_keys().await.unwrap() >= 1,
        "`old` expired"
    );
    assert_eq!(
        store
            .claim_idempotency_key(&a, &old, &[3; 32], "c11", minute, hour)
            .await
            .unwrap(),
        IdempotencyClaim::Claimed
    );
    // Live records survive the purge.
    assert_eq!(
        claim(&a, [1; 32], "c12", minute, hour).await,
        IdempotencyClaim::Existing(IdempotencyRecord {
            fingerprint: [1; 32],
            state: IdempotencyState::Completed {
                status: 202,
                body: body.to_vec()
            },
        })
    );
    // A deleted tenant's records go with it, kept answers included: a
    // tenant created again under the same id (ids are the integrator's)
    // never gets the deleted one's answers.
    assert!(
        store
            .complete_idempotency_key(&b, &key, "c5", 202, body)
            .await
            .unwrap()
    );
    let kept = IdempotencyClaim::Existing(IdempotencyRecord {
        fingerprint: [9; 32],
        state: IdempotencyState::Completed {
            status: 202,
            body: body.to_vec(),
        },
    });
    assert_eq!(claim(&b, [9; 32], "c15", minute, hour).await, kept);
    assert_eq!(
        store.delete_tenant(&b).await.unwrap(),
        DeleteTenantOutcome::Deleted
    );
    store.create_tenant(&b, "").await.unwrap().unwrap();
    assert_eq!(
        claim(&b, [9; 32], "c13", minute, hour).await,
        IdempotencyClaim::Claimed
    );
    // The other tenant's records are untouched.
    assert!(matches!(
        claim(&a, [1; 32], "c14", minute, hour).await,
        IdempotencyClaim::Existing(_)
    ));
}

pub async fn run(store: &dyn Store) {
    store.ping().await.unwrap();
    tenants(store).await;
    keys(store).await;
    bindings(store).await;
    deleting_a_tenant_revokes_platform_allowances(store).await;
    idempotency(store).await;
}
