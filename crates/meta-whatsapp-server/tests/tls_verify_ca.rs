//! Postgres with `sslmode=verify-ca` in a build that links both of rustls'
//! providers (roadmap S10, design D20 (b)): the one Postgres TLS path that
//! reads rustls' process default. sqlx builds its configuration on a
//! provider it names (aws-lc-rs here), but for `verify-ca` (the CA checked,
//! not the host name) it builds the certificate verifier with
//! `WebPkiServerVerifier::builder`, which takes the process default; with
//! two providers linked and none installed, that panics. So the service's
//! connect, migrations included, needs `tls::install_default_provider`
//! first, which `cli::run_with` does and an integrator calling
//! `serve::serve*` directly does too.
//!
//! Its own test binary, because rustls' process default is set once per
//! process. A live test: skipped unless `META_WHATSAPP_RS_TEST_POSTGRES_URL`
//! and the CA, `META_WHATSAPP_RS_TEST_REDISS_CA` (tools/test-tls/generate.sh's
//! TEST-ONLY CA, which signs the test Postgres's certificate too), are set,
//! a failure instead with `META_WHATSAPP_RS_REQUIRE_LIVE=1`.
#![cfg(feature = "api-cratestack")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use meta_whatsapp_rs::adapters::store::postgres::sqlx;
use meta_whatsapp_server::config::{self, DatabaseUrl, Env, NotUnicode};
use meta_whatsapp_server::serve;
use meta_whatsapp_server::tls::install_default_provider;
use rustls::crypto::CryptoProvider;

/// `DATABASE_URL`, and nothing else.
struct Vars(String);

impl Env for Vars {
    fn var(&self, name: &str) -> Result<Option<String>, NotUnicode> {
        Ok((name == "DATABASE_URL").then(|| self.0.clone()))
    }
    fn read_file(&self, _: &str) -> std::io::Result<String> {
        Err(std::io::ErrorKind::NotFound.into())
    }
}

/// The test CA's path: skipped when it is not set, a failure instead with
/// `META_WHATSAPP_RS_REQUIRE_LIVE=1`.
fn test_ca() -> Option<String> {
    const VAR: &str = "META_WHATSAPP_RS_TEST_REDISS_CA";
    match std::env::var(VAR) {
        Ok(path) if !path.trim().is_empty() => Some(path),
        _ if std::env::var("META_WHATSAPP_RS_REQUIRE_LIVE").is_ok_and(|v| v == "1") => panic!(
            "{VAR} is not set: the live Postgres TLS test needs the test CA \
             (tools/test-tls/generate.sh; `just test-live` sets it)"
        ),
        _ => None,
    }
}

/// Whether a session on `database` connects and is encrypted; a panic
/// while connecting is caught and reported as `Err`.
async fn connect_encrypted(database: &DatabaseUrl) -> Result<bool, String> {
    let database = database.clone();
    let connected = tokio::spawn(async move { serve::connect(&database).await }).await;
    let pool = match connected {
        Ok(Ok(pool)) => pool,
        Ok(Err(error)) => return Err(format!("{error:#}")),
        Err(error) if error.is_panic() => return Err("panicked".to_owned()),
        Err(error) => return Err(error.to_string()),
    };
    let encrypted: bool =
        sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .unwrap();
    pool.close().await;
    Ok(encrypted)
}

/// Decisive (S10): before the install, `verify-ca` fails in this build
/// (rustls panics: the process default is ambiguous); after
/// `install_default_provider`, it connects, encrypted, the server's
/// certificate verified against the test CA. Removing the install from
/// `install_default_provider` fails the second half.
#[tokio::test]
async fn live_postgres_sslmode_verify_ca_needs_the_process_default() {
    let (Some(url), Some(ca)) = (common::postgres_url(), test_ca()) else {
        return;
    };
    // The last `sslmode` of the query wins.
    let separator = if url.contains('?') { '&' } else { '?' };
    let env = Vars(format!(
        "{url}{separator}sslmode=verify-ca&sslrootcert={ca}"
    ));
    let (database, _) = config::database(&env).unwrap();

    assert!(
        CryptoProvider::get_default().is_none(),
        "a default was installed before the test"
    );
    // rustls: "Could not automatically determine the process-level
    // CryptoProvider", from sqlx's handshake.
    assert_eq!(
        connect_encrypted(&database).await,
        Err("panicked".to_owned()),
        "verify-ca with no process default"
    );

    assert!(install_default_provider());
    assert_eq!(connect_encrypted(&database).await, Ok(true));
}
