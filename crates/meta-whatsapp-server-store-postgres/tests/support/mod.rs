//! What this crate's live tests share: the test database's URL (a skip,
//! or a failure under `META_WHATSAPP_RS_REQUIRE_LIVE=1`), and a fresh
//! schema per test, dropped when it ends (a panicking one included). The
//! same as the service's `tests/common` (`crates/meta-whatsapp-server`),
//! whose harness the service's own live tests need; these need only a
//! pool, and the schema prefix `wa_server_test_` `just test-live-clean`
//! sweeps.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)] // test support

use std::sync::atomic::{AtomicUsize, Ordering};

/// `META_WHATSAPP_RS_TEST_POSTGRES_URL`, or `None` (after saying so) when it
/// is unset.
///
/// # Panics
///
/// When it is unset and `META_WHATSAPP_RS_REQUIRE_LIVE=1`: a skipped live
/// test must not pass as a green one in `just test-live`.
pub fn postgres_url() -> Option<String> {
    const VAR: &str = "META_WHATSAPP_RS_TEST_POSTGRES_URL";
    match std::env::var(VAR) {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ if std::env::var("META_WHATSAPP_RS_REQUIRE_LIVE").is_ok_and(|v| v == "1") => {
            panic!(
                "{VAR} is not set, and META_WHATSAPP_RS_REQUIRE_LIVE=1 turns a skipped live test into a failure"
            )
        }
        _ => {
            eprintln!(
                "skipping: {VAR} is not set (META_WHATSAPP_RS_REQUIRE_LIVE=1 makes this a failure)"
            );
            None
        }
    }
}

/// A lowercase `[a-z0-9_]` token unique to this call.
pub fn unique() -> String {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    format!("{t:x}_{}_{n}", std::process::id())
}

/// A fresh schema on the test database, and a way to open pools on it
/// (each pool is another "replica"). Dropping it drops the schema, when the
/// test panics too, so a failed assertion leaves nothing on the shared test
/// server.
pub struct TestDb {
    pub url: String,
    pub schema: String,
}

impl TestDb {
    /// A new schema, or `None` when no database is configured.
    pub async fn new() -> Option<Self> {
        use meta_whatsapp_rs::adapters::store::postgres::sqlx;
        let url = postgres_url()?;
        // Before the `CREATE`: `Drop` says `IF EXISTS`, so a `CREATE` that
        // failed (or whose reply was lost) is covered too.
        let db = Self {
            url,
            schema: format!("wa_server_test_{}", unique()),
        };
        let admin = sqlx::PgPool::connect(&db.url)
            .await
            .expect("connect to META_WHATSAPP_RS_TEST_POSTGRES_URL");
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {}", db.schema)))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        Some(db)
    }

    /// `DROP SCHEMA … CASCADE` on a connection of its own, in a runtime of
    /// its own: `Drop` cannot await, and the test's runtime may be the one
    /// unwinding (or, in `binary.rs`, not running at all).
    fn drop_schema(url: &str, schema: &str) -> Result<(), String> {
        use std::str::FromStr;

        use meta_whatsapp_rs::adapters::store::postgres::sqlx;
        use sqlx::{ConnectOptions, Connection};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            // A transaction a panicking test (or a service it started) left
            // open must not hang the drop.
            let mut conn = sqlx::postgres::PgConnectOptions::from_str(url)
                .map_err(|e| e.to_string())?
                .options([("lock_timeout", "10s")])
                .connect()
                .await
                .map_err(|e| e.to_string())?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE"
            )))
            .execute(&mut conn)
            .await
            .map_err(|e| e.to_string())?;
            conn.close().await.map_err(|e| e.to_string())
        })
    }

    /// A pool whose connections use the schema.
    pub async fn pool(
        &self,
        size: u32,
    ) -> meta_whatsapp_rs::adapters::store::postgres::sqlx::PgPool {
        use std::str::FromStr;

        use meta_whatsapp_rs::adapters::store::postgres::sqlx::postgres::{
            PgConnectOptions, PgPoolOptions,
        };
        let options = PgConnectOptions::from_str(&self.url)
            .unwrap()
            .options([("search_path", self.schema.as_str())]);
        PgPoolOptions::new()
            .max_connections(size)
            .connect_with(options)
            .await
            .unwrap()
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let (url, schema) = (self.url.clone(), self.schema.clone());
        let result = std::thread::spawn(move || Self::drop_schema(&url, &schema))
            .join()
            .unwrap_or_else(|_| Err("the cleanup thread panicked".to_owned()));
        if let Err(e) = result {
            // A second panic while unwinding would abort the test binary and
            // hide the first one: report it instead.
            if std::thread::panicking() {
                eprintln!("cleanup: dropping schema {} failed: {e}", self.schema);
            } else {
                panic!("cleanup: dropping schema {} failed: {e}", self.schema);
            }
        }
    }
}
