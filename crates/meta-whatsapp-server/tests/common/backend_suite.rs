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
}
