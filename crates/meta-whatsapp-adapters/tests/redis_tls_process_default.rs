//! The case `RedisTls` exists for (roadmap L21c): in a binary linking both
//! rustls providers with no process default installed,
//! `rustls::ClientConfig::builder()`, the call redis-rs's own TLS makes for
//! every `rediss://` connection, panics. This proves the environment of
//! `tests/redis_tls.rs` is the hostile one: were `ring` no longer linked,
//! `builder()` would quietly install aws-lc-rs instead, this test would
//! fail, and a `RedisTls` built on the process default would pass there.
//! A binary of its own, so that outcome cannot install a default under the
//! other tests.
#![cfg(feature = "redis")]

use std::sync::Arc;

use meta_whatsapp_adapters::store::RedisTls;

#[test]
#[should_panic(expected = "process-level CryptoProvider")]
fn handing_redis_tls_the_process_default_panics_with_both_providers_linked() {
    assert!(rustls::crypto::CryptoProvider::get_default().is_none());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    let _ = RedisTls::from_client_config(Arc::new(config));
}
