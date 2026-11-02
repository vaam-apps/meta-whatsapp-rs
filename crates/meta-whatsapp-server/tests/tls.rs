//! rustls' crypto providers in a build with CrateStack (roadmap S10,
//! design D20 (b)): the binary built with `api-cratestack` compiles both
//! of rustls' providers in, `ring` through CrateStack's HTTP client and
//! aws-lc-rs through the rest of the tree.
//!
//! Its own test binary, because rustls' process default is set once per
//! process: nothing else here installs one. The start-up case runs in
//! `just test`; the Postgres case is a live one (`live_`), skipped unless
//! `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set, a failure instead with
//! `META_WHATSAPP_RS_REQUIRE_LIVE=1` (`just test-live`), and needs a
//! Postgres that speaks TLS (compose.test.yaml's and the devcontainer's
//! do, with the test-only certificate of tools/test-tls/generate.sh).
#![cfg(feature = "api-cratestack")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use clap::Parser as _;
use meta_whatsapp_rs::adapters::store::postgres::sqlx;
use meta_whatsapp_server::cli::{Cli, run_with};
use meta_whatsapp_server::config::{self, Env, NotUnicode};
use meta_whatsapp_server::serve::{self, BuiltinBackends};
use meta_whatsapp_server::tls::install_default_provider;
use rustls::crypto::CryptoProvider;

/// Decisive (S10): a rustls client configuration built from the process
/// default after start-up. Before it, the default is ambiguous in this
/// build, and building one panics; the command line's start-up installs
/// aws-lc-rs, after which it builds. Removing the install from
/// `cli::run_with` makes the second build panic.
#[tokio::test]
async fn a_client_configuration_builds_from_the_process_default_after_start_up() {
    // Nothing installed a default, and rustls cannot pick one from its
    // features: both providers are compiled in.
    assert!(
        CryptoProvider::get_default().is_none(),
        "a default was installed before start-up"
    );
    let ambiguous = std::panic::catch_unwind(rustls::ClientConfig::builder);
    assert!(
        ambiguous.is_err(),
        "before start-up the process default resolved, so this build does not link both providers"
    );

    // Start-up: the command line, on a command that reads no configuration.
    run_with(
        Cli::parse_from(["meta-whatsapp-server", "openapi"]),
        &BuiltinBackends,
    )
    .await
    .unwrap();

    // Without the install, this is where it panics (rustls: "Could not
    // automatically determine the process-level CryptoProvider").
    let client = rustls::ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    let installed = CryptoProvider::get_default().expect("start-up installs a process default");
    assert!(std::sync::Arc::ptr_eq(client.crypto_provider(), installed));
    assert_eq!(format!("{:?}", installed.key_provider), "AwsLcRs");

    // A default already installed is kept.
    assert!(!install_default_provider());
    assert!(std::sync::Arc::ptr_eq(
        CryptoProvider::get_default().unwrap(),
        installed
    ));
}

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

/// Decisive (S10): Postgres with `sslmode=require` in this build, through
/// the service's own configuration and pool (`config::database`,
/// `serve::connect`). sqlx names its provider itself (aws-lc-rs in this
/// tree: CrateStack's `db = None` facade brings no sqlx), so its TLS does
/// not depend on the process default, installed or not.
#[tokio::test]
async fn live_postgres_sslmode_require_with_both_providers_linked() {
    let Some(url) = common::postgres_url() else {
        return;
    };
    // The last `sslmode` of the query wins.
    let separator = if url.contains('?') { '&' } else { '?' };
    let env = Vars(format!("{url}{separator}sslmode=require"));
    let (database, _) = config::database(&env).unwrap();
    let pool = serve::connect(&database)
        .await
        .expect("connect to META_WHATSAPP_RS_TEST_POSTGRES_URL with sslmode=require");
    let encrypted: bool =
        sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(encrypted, "the session is not encrypted");
    pool.close().await;
}
