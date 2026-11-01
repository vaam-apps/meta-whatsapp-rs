//! `RedisTls` when the application installed **`ring`** as its process
//! default (roadmap L21c): the library still uses aws-lc-rs, the provider
//! it names, never the process default. A binary of its own: installing a
//! default is process-wide, and `tests/redis_tls.rs` needs none.
//!
//! The live test uses the same TLS Redis and variables as
//! `tests/redis_tls.rs`.
#![cfg(feature = "redis")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::{Arc, Once};

use meta_whatsapp_adapters::store::{RedisKvStore, RedisTls};
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey};
use rustls::crypto::CryptoProvider;

/// Installs `ring` as the process default, once for the binary.
fn install_ring_default() -> &'static Arc<CryptoProvider> {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("no default was installed before");
    });
    CryptoProvider::get_default().expect("installed above")
}

fn assert_not_the_default(tls: &RedisTls, default: &Arc<CryptoProvider>) {
    let provider = tls.client_config().crypto_provider();
    assert!(
        !Arc::ptr_eq(provider, default),
        "RedisTls must not use the process default"
    );
    assert_eq!(format!("{:?}", default.key_provider), "Ring");
    assert_eq!(format!("{:?}", provider.key_provider), "AwsLcRs");
}

#[test]
fn uses_aws_lc_rs_when_the_process_default_is_ring() {
    let default = install_ring_default();
    let tls = RedisTls::builder().build().unwrap();
    assert_not_the_default(&tls, default);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_rediss_uses_aws_lc_rs_when_the_process_default_is_ring() {
    let Some(url) = common::service_url("META_WHATSAPP_RS_TEST_REDISS_URL") else {
        return;
    };
    let read = |var: &str| {
        let path = std::env::var(var)
            .unwrap_or_else(|_| panic!("META_WHATSAPP_RS_TEST_REDISS_URL is set but {var} is not"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("{var}={path}: {e}"))
    };
    let default = install_ring_default();
    let tls = RedisTls::builder()
        .root_certificates_pem(read("META_WHATSAPP_RS_TEST_REDISS_CA"))
        .client_auth_pem(
            read("META_WHATSAPP_RS_TEST_REDISS_CLIENT_CERT"),
            read("META_WHATSAPP_RS_TEST_REDISS_CLIENT_KEY"),
        )
        .build()
        .unwrap();
    assert_not_the_default(&tls, default);

    let mut conn = tls.connect(&url).await.unwrap();
    let store =
        RedisKvStore::new(conn.clone()).with_prefix(format!("wa-test:{}:", common::unique()));
    let key = StoreKey::new("wa.tls", "ring-default");
    let version = store.put(&key, b"v".to_vec(), Expiry::Never).await.unwrap();
    assert_eq!(store.get(&key).await.unwrap().unwrap().version, version);
    assert!(store.delete(&key).await.unwrap());
    // The namespace counter is the one key left: delete it too.
    let counter = format!("{}{{6:wa.tls}}#version", store.prefix());
    let deleted: i64 = redis::cmd("DEL")
        .arg(counter)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(deleted, 1);
}
