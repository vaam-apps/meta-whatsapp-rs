//! Shared by the `live_*` test files.
#![allow(dead_code)] // each test binary uses a different subset

/// The service URL in `var`, or `None` (after saying so) when it is unset.
///
/// # Panics
///
/// When `var` is unset and `WA_RS_REQUIRE_LIVE=1`: a skipped live test must
/// not pass as a green one in `just test-live`.
pub fn service_url(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(url) if !url.trim().is_empty() => Some(url),
        _ if std::env::var("WA_RS_REQUIRE_LIVE").is_ok_and(|v| v == "1") => {
            panic!(
                "{var} is not set, and WA_RS_REQUIRE_LIVE=1 turns a skipped live test into a failure"
            )
        }
        _ => {
            eprintln!("skipping: {var} is not set (WA_RS_REQUIRE_LIVE=1 makes this a failure)");
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
