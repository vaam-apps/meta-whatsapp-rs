//! Acceptance test M1.7, in process: the captured `tracing` output of
//! every operation of the committed document (admin, numbers, sends,
//! media, templates, events) and of Meta's webhook deliveries (signed,
//! unsigned, forged, refused), at `TRACE` for every target (the library's
//! included), holds no secret, key, token, message text, phone number or
//! contact. `live_logs.rs` repeats it on Postgres.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::Harness;
use common::capture::{Captured, check, exercise, subscriber};

#[tokio::test]
async fn every_route_logs_no_secret_key_message_text_phone_number_or_contact() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    let secrets = exercise(&h).await;
    check(&captured.text(), &secrets);
}

/// The request span names the tenant and the key a request acted as, for
/// the operator's logs: the authorization order records both on it once it
/// knows them (`Authorizer::tenant_caller`, in the core). Decisive: each
/// `record` there.
#[tokio::test]
async fn the_request_span_names_the_tenant_and_the_key() {
    use common::Call;
    use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
    use meta_whatsapp_server::model::Scope;

    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    h.tenant("merchant-7").await;
    let key = h.tenant_key("merchant-7", &[Scope::Numbers]).await;
    let reply = h.call(Call::get("/v1/numbers").key(&key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let line = captured
        .text()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|e| e["fields"]["message"] == "request" && e["span"]["route"] == "/v1/numbers")
        .expect("the request's log line");
    assert_eq!(line["span"]["tenant"], "merchant-7", "{line}");
    let key_id = key.split('_').nth(1).unwrap();
    assert_eq!(line["span"]["key_id"], key_id, "{line}");
}

/// Roadmap S7 moved the service's modules into crates of their own; their
/// log lines keep the targets they had (`meta_whatsapp_server::<module>`,
/// named explicitly), which an operator's filters and alerts name. Over
/// every route and webhook delivery of `exercise`: no line's target is a
/// moved module's new path, and the request line, the refused deliveries'
/// lines and the webhook pipeline's keep theirs. Decisive: an implicit
/// target in a moved module (its new crate's path).
#[tokio::test]
async fn every_line_keeps_its_target_from_before_the_crate_split() {
    let captured = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&captured));
    let h = Harness::new();
    exercise(&h).await;
    let targets: std::collections::BTreeSet<String> = captured
        .text()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|e| e["target"].as_str().map(str::to_owned))
        .collect();
    let moved: Vec<&String> = targets
        .iter()
        .filter(|t| {
            [
                "meta_whatsapp_server_http",
                "meta_whatsapp_server_api_axum",
                "meta_whatsapp_server_store_",
            ]
            .iter()
            .any(|krate| t.starts_with(krate))
        })
        .collect();
    assert!(moved.is_empty(), "a moved module's path: {moved:?}");
    for kept in [
        "meta_whatsapp_server::telemetry",
        "meta_whatsapp_server::api::webhooks",
        "meta_whatsapp_server::events",
    ] {
        assert!(targets.contains(kept), "{kept} missing from {targets:?}");
    }
}

/// Each module roadmap S7 moved that logs, under the crates' directory,
/// and the target its lines had before the split: the module path it had
/// in `meta-whatsapp-server` (dda06c9).
const MOVED_LOG_TARGETS: [(&str, &str); 8] = [
    (
        "meta-whatsapp-server-http/src/api/mod.rs",
        "meta_whatsapp_server::api",
    ),
    (
        "meta-whatsapp-server-http/src/api/webhooks.rs",
        "meta_whatsapp_server::api::webhooks",
    ),
    (
        "meta-whatsapp-server-http/src/auth.rs",
        "meta_whatsapp_server::auth",
    ),
    (
        "meta-whatsapp-server-http/src/listen.rs",
        "meta_whatsapp_server::listen",
    ),
    (
        "meta-whatsapp-server-http/src/telemetry.rs",
        "meta_whatsapp_server::telemetry",
    ),
    (
        "meta-whatsapp-server-api-axum/src/api/events.rs",
        "meta_whatsapp_server::api::events",
    ),
    (
        "meta-whatsapp-server-store-memory/src/events.rs",
        "meta_whatsapp_server::store::events",
    ),
    (
        "meta-whatsapp-server-store-postgres/src/events.rs",
        "meta_whatsapp_server::store::events_postgres",
    ),
];

/// The `tracing` macros that log or open a span.
const LOG_MACROS: [&str; 12] = [
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "event",
    "span",
    "trace_span",
    "debug_span",
    "info_span",
    "warn_span",
    "error_span",
];

/// Every `.rs` file under `dir`, by path relative to `root`.
fn sources(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let relative = path.strip_prefix(root).unwrap().to_string_lossy();
            out.push((
                relative.replace('\\', "/"),
                std::fs::read_to_string(&path).unwrap(),
            ));
        }
    }
}

/// [`every_line_keeps_its_target_from_before_the_crate_split`] sees only
/// the lines its scenario logs; this reads every log line of the crates
/// roadmap S7 made, logged or not (an accept error, a deadlocked insert, a
/// route that is no tenant operation): each is `tracing::<macro>!` with
/// `target: LOG_TARGET` first, no `#[instrument]` and no bare macro, and
/// each file that logs is one of [`MOVED_LOG_TARGETS`], whose
/// `LOG_TARGET` is the module path it had before the split. A new module
/// that logs is added there with the target an operator will filter on.
/// Decisive: a `LOG_TARGET` changed, or a line without it.
#[test]
fn every_moved_log_line_names_its_target_from_before_the_crate_split() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let mut files = Vec::new();
    for krate in [
        "meta-whatsapp-server-http",
        "meta-whatsapp-server-api-axum",
        "meta-whatsapp-server-store-memory",
        "meta-whatsapp-server-store-postgres",
    ] {
        sources(crates, &crates.join(krate).join("src"), &mut files);
    }
    let mut problems = Vec::new();
    let mut logging = std::collections::BTreeSet::new();
    for (file, source) in &files {
        for (at, _) in source.match_indices("!(") {
            let before = &source[..at];
            let name_at = before
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map_or(0, |i| i + 1);
            let name = &before[name_at..];
            if !LOG_MACROS.contains(&name) {
                continue;
            }
            let line_start = before.rfind('\n').map_or(0, |i| i + 1);
            if before[line_start..].trim_start().starts_with("//") {
                continue;
            }
            let line = before.matches('\n').count() + 1;
            logging.insert(file.clone());
            if !before[..name_at].ends_with("tracing::") {
                problems.push(format!("{file}:{line}: `{name}!` without `tracing::`"));
            } else if !source[at + 2..]
                .trim_start()
                .starts_with("target: LOG_TARGET,")
            {
                problems.push(format!(
                    "{file}:{line}: `tracing::{name}!` without `target: LOG_TARGET` first"
                ));
            }
        }
        if source.contains("#[instrument") || source.contains("#[tracing::instrument") {
            problems.push(format!(
                "{file}: an `#[instrument]`, whose target is the new crate's"
            ));
        }
    }
    let expected: std::collections::BTreeSet<String> = MOVED_LOG_TARGETS
        .iter()
        .map(|(file, _)| (*file).to_owned())
        .collect();
    assert_eq!(logging, expected, "the moved modules that log");
    for (file, target) in MOVED_LOG_TARGETS {
        let (_, source) = files.iter().find(|(f, _)| f == file).unwrap();
        let declared = format!("const LOG_TARGET: &str = \"{target}\";");
        if source.matches(&declared).count() != 1 {
            problems.push(format!("{file}: not exactly one `{declared}`"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}
