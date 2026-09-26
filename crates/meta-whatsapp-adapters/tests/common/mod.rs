//! Shared by the `live_*` test files.
#![allow(dead_code)] // each test binary uses a different subset

/// The service URL in `var`, or `None` (after saying so) when it is unset.
///
/// # Panics
///
/// When `var` is unset and `META_WHATSAPP_RS_REQUIRE_LIVE=1`: a skipped live test must
/// not pass as a green one in `just test-live`.
pub fn service_url(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ if std::env::var("META_WHATSAPP_RS_REQUIRE_LIVE").is_ok_and(|v| v == "1") => {
            panic!(
                "{var} is not set, and META_WHATSAPP_RS_REQUIRE_LIVE=1 turns a skipped live test into a failure"
            )
        }
        _ => {
            eprintln!(
                "skipping: {var} is not set (META_WHATSAPP_RS_REQUIRE_LIVE=1 makes this a failure)"
            );
            None
        }
    }
}

/// A lowercase `[a-z0-9_]` token unique to this call, usable in SQL
/// identifiers and Redis key prefixes, so parallel runs never collide.
pub fn unique() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    format!("{t:x}_{}_{n}", std::process::id())
}

/// Drops a Postgres schema or database when it goes out of scope, a
/// panicking test's unwinding included, so a failed assertion leaves
/// nothing behind on the shared test server.
///
/// Create it *before* the `CREATE`: the statement is `DROP … IF EXISTS`, so
/// a `CREATE` that failed (or whose reply was lost) is covered too. `Drop`
/// cannot await, and the test's runtime may be the one unwinding, so the
/// statement runs on a thread of its own, on a new connection, in a
/// current-thread runtime of its own.
#[cfg(feature = "postgres")]
pub struct PgCleanup {
    url: String,
    statement: String,
}

#[cfg(feature = "postgres")]
impl PgCleanup {
    /// `DROP SCHEMA … CASCADE` of `schema`, on the database of `url`.
    pub fn schema(url: &str, schema: &str) -> Self {
        Self {
            url: url.to_owned(),
            statement: format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
        }
    }

    /// `DROP DATABASE … WITH (FORCE)` of `name` (its connections, a pool the
    /// test still holds included, are terminated), issued from the database
    /// of `url`.
    pub fn database(url: &str, name: &str) -> Self {
        Self {
            url: url.to_owned(),
            statement: format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
        }
    }

    fn run(url: &str, statement: String) -> Result<(), String> {
        use sqlx::{ConnectOptions, Connection};
        use std::str::FromStr;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            // A transaction a panicking test left open must not hang the drop.
            let mut conn = sqlx::postgres::PgConnectOptions::from_str(url)
                .map_err(|e| e.to_string())?
                .options([("lock_timeout", "10s")])
                .connect()
                .await
                .map_err(|e| e.to_string())?;
            sqlx::query(sqlx::AssertSqlSafe(statement))
                .execute(&mut conn)
                .await
                .map_err(|e| e.to_string())?;
            conn.close().await.map_err(|e| e.to_string())
        })
    }
}

#[cfg(feature = "postgres")]
impl Drop for PgCleanup {
    fn drop(&mut self) {
        let (url, statement) = (self.url.clone(), self.statement.clone());
        let what = statement.clone();
        let result = std::thread::spawn(move || Self::run(&url, statement))
            .join()
            .unwrap_or_else(|_| Err("the cleanup thread panicked".to_owned()));
        if let Err(e) = result {
            // A second panic while unwinding would abort the test binary and
            // hide the first one: report it instead.
            if std::thread::panicking() {
                eprintln!("cleanup `{what}` failed: {e}");
            } else {
                panic!("cleanup `{what}` failed: {e}");
            }
        }
    }
}
