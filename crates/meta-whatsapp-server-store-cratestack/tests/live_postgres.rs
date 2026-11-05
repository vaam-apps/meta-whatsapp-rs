//! Roadmap S11, live: the store calls its model as the system principal.
//!
//! Skipped unless `META_WHATSAPP_RS_TEST_POSTGRES_URL` is set;
//! `META_WHATSAPP_RS_REQUIRE_LIVE=1` (which `just test-live` sets) turns the
//! skip into a failure. Each test makes a `wa_test_*` database of its own and
//! drops it when it ends, a panic included (`just test-live-clean` drops what
//! a killed run left).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};

use cratestack::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use cratestack::sqlx::{AssertSqlSafe, Connection, PgConnection, PgPool, query};
use cratestack::{CratestackContext, Value};
use meta_whatsapp_server_store_cratestack::{
    Probe, Probes, SCHEMA, SYSTEM_SERVICE, system_context, tenant_context,
};

/// The test server's URL, or `None` (after saying so) when it is unset.
///
/// # Panics
///
/// When it is unset and `META_WHATSAPP_RS_REQUIRE_LIVE=1`.
fn postgres_url() -> Option<String> {
    const VAR: &str = "META_WHATSAPP_RS_TEST_POSTGRES_URL";
    match std::env::var(VAR) {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ if std::env::var("META_WHATSAPP_RS_REQUIRE_LIVE").is_ok_and(|v| v == "1") => {
            panic!("{VAR} is not set, and META_WHATSAPP_RS_REQUIRE_LIVE=1 makes a skip a failure")
        }
        _ => {
            eprintln!("skipping: {VAR} is not set");
            None
        }
    }
}

/// A database of this test's own, dropped with it.
struct TestDb {
    admin: PgConnectOptions,
    name: String,
    pool: PgPool,
}

impl TestDb {
    async fn create(url: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("wa_test_cs_{}_{n}", std::process::id());
        let admin: PgConnectOptions = url.parse().unwrap();
        let mut conn = PgConnection::connect_with(&admin).await.unwrap();
        query(AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&mut conn)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(admin.clone().database(&name))
            .await
            .unwrap();
        // The table the model reads, by hand: this spike has no migrations
        // (roadmap S14 decides how the models' tables are made).
        query("CREATE TABLE wa_probes (id BIGINT PRIMARY KEY, tenant_id TEXT NOT NULL, label TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        Self { admin, name, pool }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let admin = self.admin.clone();
        let name = self.name.clone();
        // On a thread and runtime of its own: `drop` cannot await, and the
        // test's runtime may be the one unwinding. `WITH (FORCE)` ends the
        // pool's sessions: closing the pool here would wait on the test's
        // runtime, which this thread is blocking.
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                if let Ok(mut conn) = PgConnection::connect_with(&admin).await {
                    let _ = query(AssertSqlSafe(format!(
                        "DROP DATABASE IF EXISTS {name} WITH (FORCE)"
                    )))
                    .execute(&mut conn)
                    .await;
                }
            });
        })
        .join();
    }
}

fn probe(id: i64, tenant: &str) -> Probe {
    Probe {
        id,
        tenant_id: tenant.to_owned(),
        label: format!("probe {id}"),
    }
}

#[test]
fn the_model_names_the_system_principal_in_every_allow() {
    // The schema is the contract: every `@@allow` of the model is system-only.
    let allows: Vec<&str> = SCHEMA
        .lines()
        .filter(|l| l.trim_start().starts_with("@@allow"))
        .collect();
    assert_eq!(allows.len(), 5, "{allows:?}");
    for action in ["list", "detail", "create", "update", "delete"] {
        let mine: Vec<&&str> = allows
            .iter()
            .filter(|l| l.contains(&format!("(\"{action}\",")))
            .collect();
        assert_eq!(mine.len(), 1, "{action}: {allows:?}");
        assert!(
            mine[0].trim_end().ends_with(", auth().isSystem())"),
            "{action}: {mine:?}"
        );
    }
}

#[test]
fn only_the_system_context_is_the_system_principal() {
    assert!(system_context().is_system());
    let forged = CratestackContext::authenticated([
        (
            "service".to_owned(),
            Value::String(SYSTEM_SERVICE.to_owned()),
        ),
        (
            "id".to_owned(),
            Value::String(format!("system:{SYSTEM_SERVICE}")),
        ),
        ("system".to_owned(), Value::Bool(true)),
        ("isSystem".to_owned(), Value::Bool(true)),
    ]);
    for ctx in [
        tenant_context("tenant-a"),
        forged,
        CratestackContext::default(),
        CratestackContext::anonymous(),
    ] {
        assert!(!ctx.is_system(), "{ctx:?}");
    }
}

#[tokio::test]
async fn live_postgres_a_system_find_many_returns_the_rows() {
    let Some(url) = postgres_url() else { return };
    let db = TestDb::create(&url).await;
    let probes = Probes::new(db.pool.clone());

    probes
        .create(&system_context(), probe(1, "tenant-a"))
        .await
        .unwrap();
    probes
        .create(&system_context(), probe(2, "tenant-b"))
        .await
        .unwrap();

    let mut rows = probes.find_many(&system_context()).await.unwrap();
    rows.sort_by_key(|p| p.id);
    assert_eq!(rows, vec![probe(1, "tenant-a"), probe(2, "tenant-b")]);
}

#[tokio::test]
async fn live_postgres_the_same_call_under_a_tenant_returns_none() {
    let Some(url) = postgres_url() else { return };
    let db = TestDb::create(&url).await;
    let probes = Probes::new(db.pool.clone());
    probes
        .create(&system_context(), probe(1, "tenant-a"))
        .await
        .unwrap();

    // Even the tenant whose id the row names sees nothing, and cannot write.
    let own = probes.find_many(&tenant_context("tenant-a")).await.unwrap();
    assert!(own.is_empty(), "{own:?}");
    let other = probes.find_many(&tenant_context("tenant-b")).await.unwrap();
    assert!(other.is_empty(), "{other:?}");
    let denied = probes
        .create(&tenant_context("tenant-a"), probe(2, "tenant-a"))
        .await;
    assert!(denied.is_err(), "{denied:?}");
    // The row the tenant tried to add is not there.
    assert_eq!(probes.find_many(&system_context()).await.unwrap().len(), 1);
}

/// Every context that is not the system principal, forged claims included.
fn non_system_contexts() -> Vec<CratestackContext> {
    vec![
        tenant_context("tenant-a"),
        tenant_context("tenant-b"),
        CratestackContext::authenticated([
            (
                "service".to_owned(),
                Value::String(SYSTEM_SERVICE.to_owned()),
            ),
            (
                "id".to_owned(),
                Value::String(format!("system:{SYSTEM_SERVICE}")),
            ),
        ]),
        CratestackContext::default(),
        CratestackContext::anonymous(),
    ]
}

#[tokio::test]
async fn live_postgres_a_system_reads_updates_and_deletes_one_row() {
    let Some(url) = postgres_url() else { return };
    let db = TestDb::create(&url).await;
    let probes = Probes::new(db.pool.clone());
    let system = system_context();
    probes.create(&system, probe(1, "tenant-a")).await.unwrap();

    assert_eq!(
        probes.find_one(&system, 1).await.unwrap(),
        Some(probe(1, "tenant-a"))
    );
    let updated = probes.set_label(&system, 1, "renamed").await.unwrap();
    assert_eq!(updated.label, "renamed");
    assert_eq!(probes.delete(&system, 1).await.unwrap().id, 1);
    assert_eq!(probes.find_one(&system, 1).await.unwrap(), None);
}

#[tokio::test]
async fn live_postgres_no_other_context_may_detail_update_or_delete() {
    let Some(url) = postgres_url() else { return };
    let db = TestDb::create(&url).await;
    let probes = Probes::new(db.pool.clone());
    probes
        .create(&system_context(), probe(1, "tenant-a"))
        .await
        .unwrap();

    for ctx in non_system_contexts() {
        // list
        let listed = probes.find_many(&ctx).await;
        assert!(listed.as_ref().map_or(true, Vec::is_empty), "{listed:?}");
        // detail: refused, or nothing found
        let found = probes.find_one(&ctx, 1).await;
        assert!(found.as_ref().map_or(true, Option::is_none), "{found:?}");
        // create
        let created = probes.create(&ctx, probe(2, "tenant-a")).await;
        assert!(created.is_err(), "{created:?}");
        // update
        let updated = probes.set_label(&ctx, 1, "hijacked").await;
        assert!(updated.is_err(), "{updated:?}");
        // delete
        let deleted = probes.delete(&ctx, 1).await;
        assert!(deleted.is_err(), "{deleted:?}");
    }

    // Nothing changed: one row, its own label.
    let system = system_context();
    assert_eq!(
        probes.find_many(&system).await.unwrap(),
        vec![probe(1, "tenant-a")]
    );
}
