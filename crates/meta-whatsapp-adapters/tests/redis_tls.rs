//! `rediss://` through `RedisTls` (roadmap L21c), in a binary that links
//! **both** rustls providers (the dev-dependency adds `ring` to the
//! aws-lc-rs the library uses) and never installs a process default: the
//! case in which `rustls::ClientConfig::builder()`, what redis-rs's own TLS
//! calls, panics (`tests/redis_tls_process_default.rs` shows it does).
//!
//! The `live_*` tests need a TLS Redis that requires client certificates
//! (`tls-auth-clients yes`), as compose.test.yaml's `redis-tls` service:
//! `META_WHATSAPP_RS_TEST_REDISS_URL`, with the PEM files of the CA the test
//! trusts and of the client certificate and key in
//! `META_WHATSAPP_RS_TEST_REDISS_CA`, `…_CLIENT_CERT` and `…_CLIENT_KEY`
//! (tools/test-tls/generate.sh writes them). Skipped unless the URL is set;
//! `META_WHATSAPP_RS_REQUIRE_LIVE=1` (as in `just test-live`) turns the skip
//! into a failure.
#![cfg(feature = "redis")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::time::Duration;

use meta_whatsapp_adapters::store::{RedisKvStore, RedisTls, RedisTlsConnection, conformance};
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey};
use rustls::crypto::CryptoProvider;

/// The precondition of every test here: `ring` is linked next to aws-lc-rs,
/// and nothing installed a process default.
fn assert_both_providers_and_no_default() {
    // Compiles only with rustls' `ring` feature, which no library build
    // enables: this binary links both providers.
    let ring = rustls::crypto::ring::default_provider();
    assert_eq!(format!("{:?}", ring.key_provider), "Ring");
    assert!(
        CryptoProvider::get_default().is_none(),
        "a process-wide CryptoProvider is installed: RedisTls must never need, use or install one"
    );
}

/// What a `RedisTls` was built on.
fn key_provider(tls: &RedisTls) -> String {
    format!("{:?}", tls.client_config().crypto_provider().key_provider)
}

#[test]
fn builds_on_aws_lc_rs_with_both_providers_linked_and_no_default() {
    assert_both_providers_and_no_default();
    // The platform's trust store: CI's runner and the devcontainer have one.
    let tls = RedisTls::builder()
        .build()
        .expect("RedisTls on the platform's roots (a system CA bundle is needed)");
    assert_eq!(key_provider(&tls), "AwsLcRs");
    assert_both_providers_and_no_default();
}

/// The live TLS Redis and the test's PEM files.
struct Rediss {
    url: String,
    ca: Vec<u8>,
    client_cert: Vec<u8>,
    client_key: Vec<u8>,
}

impl Rediss {
    fn from_env() -> Option<Self> {
        let url = common::service_url("META_WHATSAPP_RS_TEST_REDISS_URL")?;
        let read = |var: &str| {
            let path = std::env::var(var).unwrap_or_else(|_| {
                panic!("META_WHATSAPP_RS_TEST_REDISS_URL is set but {var} is not")
            });
            std::fs::read(&path).unwrap_or_else(|e| panic!("{var}={path}: {e}"))
        };
        Some(Self {
            url,
            ca: read("META_WHATSAPP_RS_TEST_REDISS_CA"),
            client_cert: read("META_WHATSAPP_RS_TEST_REDISS_CLIENT_CERT"),
            client_key: read("META_WHATSAPP_RS_TEST_REDISS_CLIENT_KEY"),
        })
    }

    /// The test CA and the client certificate: what the server accepts.
    fn tls(&self) -> RedisTls {
        RedisTls::builder()
            .root_certificates_pem(self.ca.clone())
            .client_auth_pem(self.client_cert.clone(), self.client_key.clone())
            .build()
            .unwrap()
    }

    async fn connect(&self) -> RedisTlsConnection {
        self.tls().connect(&self.url).await.unwrap()
    }

    /// A key prefix of this test's own, and the guard that deletes its keys.
    fn prefix(&self) -> (String, RedissCleanup) {
        let prefix = format!("wa-test:{}:", common::unique());
        let cleanup = RedissCleanup {
            tls: self.tls(),
            url: self.url.clone(),
            prefix: prefix.clone(),
        };
        (prefix, cleanup)
    }
}

/// `common::RedisCleanup` over TLS: deletes `prefix*` when it goes out of
/// scope, a panicking test's unwinding included, on a thread and runtime
/// of its own.
struct RedissCleanup {
    tls: RedisTls,
    url: String,
    prefix: String,
}

impl Drop for RedissCleanup {
    fn drop(&mut self) {
        let (tls, url, prefix) = (self.tls.clone(), self.url.clone(), self.prefix.clone());
        let result = std::thread::spawn(move || -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async {
                let mut conn = tls.connect(&url).await.map_err(|e| e.to_string())?;
                let keys: Vec<Vec<u8>> = redis::cmd("KEYS")
                    .arg(format!("{prefix}*"))
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| e.to_string())?;
                if !keys.is_empty() {
                    redis::cmd("DEL")
                        .arg(keys)
                        .query_async::<i64>(&mut conn)
                        .await
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            })
        })
        .join()
        .unwrap_or_else(|_| Err("the cleanup thread panicked".to_owned()));
        if let Err(e) = result {
            if std::thread::panicking() {
                eprintln!("rediss cleanup of `{}*` failed: {e}", self.prefix);
            } else {
                panic!("rediss cleanup of `{}*` failed: {e}", self.prefix);
            }
        }
    }
}

/// The decisive test: a `rediss://` store connects, and keeps the whole
/// `KvStore` contract, where redis-rs's own TLS would panic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_rediss_store_connects_with_both_providers_and_no_default() {
    let Some(rediss) = Rediss::from_env() else {
        return;
    };
    assert_both_providers_and_no_default();
    let (prefix, _cleanup) = rediss.prefix();
    let store = RedisKvStore::new(rediss.connect().await).with_prefix(prefix);
    conformance::run_with_real_time(&store, Duration::from_millis(500)).await;
    assert_both_providers_and_no_default();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_rediss_refuses_a_server_it_does_not_trust() {
    let Some(rediss) = Rediss::from_env() else {
        return;
    };
    // Trust only an unrelated certificate (the client's own): the server's
    // chain then ends at an unknown issuer.
    let tls = RedisTls::builder()
        .root_certificates_pem(rediss.client_cert.clone())
        .client_auth_pem(rediss.client_cert.clone(), rediss.client_key.clone())
        .build()
        .unwrap();
    let error = tls.connect(&rediss.url).await.unwrap_err();
    assert!(
        error.to_string().contains("UnknownIssuer"),
        "an untrusted server certificate must fail the handshake: {error}"
    );
    assert_both_providers_and_no_default();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_rediss_without_a_client_certificate_is_refused() {
    let Some(rediss) = Rediss::from_env() else {
        return;
    };
    let tls = RedisTls::builder()
        .root_certificates_pem(rediss.ca.clone())
        .build()
        .unwrap();
    // The server requires one: the handshake, or the first exchange right
    // after it under TLS 1.3, fails, and `connect` reports it.
    assert!(tls.connect(&rediss.url).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_rediss_reconnects_after_the_connection_drops() {
    let Some(rediss) = Rediss::from_env() else {
        return;
    };
    let (prefix, _cleanup) = rediss.prefix();
    let conn = rediss.connect().await;
    let store = RedisKvStore::new(conn.clone()).with_prefix(prefix);
    let key = StoreKey::new("wa.tls", "reconnect");
    store
        .put(&key, b"before".to_vec(), Expiry::Never)
        .await
        .unwrap();

    let client_id = |mut conn: RedisTlsConnection| async move {
        redis::cmd("CLIENT")
            .arg("ID")
            .query_async::<i64>(&mut conn)
            .await
    };
    let first = client_id(conn.clone()).await.unwrap();
    // Another connection kills this one, as a server restart or an idle
    // timeout would.
    let killed: i64 = redis::cmd("CLIENT")
        .arg("KILL")
        .arg("ID")
        .arg(first)
        .query_async(&mut rediss.connect().await)
        .await
        .unwrap();
    assert_eq!(killed, 1);

    // The command that meets the dead connection fails (it is not
    // replayed); the next one runs on a new connection.
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        let outcome = client_id(conn.clone()).await;
        let done = outcome.is_ok();
        outcomes.push(outcome);
        if done {
            break;
        }
    }
    let second = outcomes
        .last()
        .and_then(|o| o.as_ref().ok().copied())
        .unwrap_or_else(|| panic!("no reconnection after the kill: {outcomes:?}"));
    assert_ne!(second, first, "a new connection: {outcomes:?}");
    assert!(outcomes.len() <= 2, "one failure at most: {outcomes:?}");
    // Every clone, the store's included, uses the new connection.
    assert_eq!(store.get(&key).await.unwrap().unwrap().value, b"before");
}
