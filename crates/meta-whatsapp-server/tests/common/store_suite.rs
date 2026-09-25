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

/// Every case.
pub async fn run(store: &dyn Store) {
    store.ping().await.unwrap();
    tenants(store).await;
    keys(store).await;
    bindings(store).await;
}
