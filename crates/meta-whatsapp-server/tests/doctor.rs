//! `meta-whatsapp-server doctor` (roadmap L22a): each scripted
//! misconfiguration is reported by name and fails the run (the command's
//! exit is non-zero: `cli::run_with` errs on a failed report, and the real
//! binary's exit code is checked below); a healthy deployment passes with
//! exactly the reads asserted, all `GET`s; no secret reaches the report.
//! The Graph answers are Meta's examples (`webhooks/subscriptions-edge`,
//! the Graph reference's `application/subscriptions`,
//! `solution-providers/manage-accounts`, `solution-providers/manage-webhooks`),
//! adapted where a test says so.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::store::{ConversationStore, KvStore};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use meta_whatsapp_server::config::{Env, NotUnicode, Storage};
use meta_whatsapp_server::doctor::{Doctor, Report, Status};
use meta_whatsapp_server::model::TenantId;
use meta_whatsapp_server::serve::{BackendFactory, BuiltinBackends};
use meta_whatsapp_server::store::{
    Backend, BackendKind, IdempotencyRecords, Janitor, LeaderLock, MemoryBackend, Outbox,
    RecordStore, SchemaMigrator,
};
use serde_json::{Value, json};

// ─── Fixtures ──────────────────────────────────────────────────────────

/// The app of `solution-providers/manage-accounts`' `debug_token` example.
const APP_ID: &str = "670843887433847";
const APP_SECRET: &str = "SENTINEL-APP-SECRET";
const VERIFY_TOKEN: &str = "SENTINEL-VERIFY-TOKEN";
const DB_PASSWORD: &str = "SENTINEL-DB-PASSWORD";
const VAULT_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="; // 32 bytes
const OTHER_VAULT_KEY: &str = "Hx4dHBsaGRgXFhUUExIREA8ODQwLCgkIBwYFBAMCAQA="; // 32 bytes
const PEPPER: &str = "SENTINEL-PEPPER-of-at-least-thirty-two-bytes";
const PARTNER_TOKEN: &str = "SENTINEL-PARTNER-TOKEN";
/// The first WABA of the same example.
const WABA: &str = "102289599326934";
const WABA_TOKEN: &str = "SENTINEL-WABA-TOKEN";

/// Every value the report must never show.
const SECRETS: [&str; 7] = [
    APP_SECRET,
    VERIFY_TOKEN,
    DB_PASSWORD,
    VAULT_KEY,
    PEPPER,
    PARTNER_TOKEN,
    WABA_TOKEN,
];

/// Variables and files, instead of the process's.
#[derive(Default, Clone)]
struct Vars(HashMap<String, String>);

impl Env for Vars {
    fn var(&self, name: &str) -> Result<Option<String>, NotUnicode> {
        Ok(self.0.get(name).cloned())
    }
    fn read_file(&self, _path: &str) -> std::io::Result<String> {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }
}

impl Vars {
    fn set(mut self, name: &str, value: &str) -> Self {
        self.0.insert(name.to_owned(), value.to_owned());
        self
    }
    fn unset(mut self, name: &str) -> Self {
        self.0.remove(name);
        self
    }
}

/// A valid production configuration on Postgres, with the app id.
fn production() -> Vars {
    Vars::default()
        .set(
            "DATABASE_URL",
            &format!("postgres://wa:{DB_PASSWORD}@127.0.0.1:5432/wa"),
        )
        .set("WA_APP_ID", APP_ID)
        .set("WA_APP_SECRET", APP_SECRET)
        .set("WA_VERIFY_TOKEN", VERIFY_TOKEN)
        .set("WA_VAULT_KEY", VAULT_KEY)
        .set("WA_OTP_PEPPER", PEPPER)
}

/// A memory backend that does not say it is one, so the doctor walks its
/// WABAs as it walks a shared database's.
struct Shared(MemoryBackend);

#[async_trait]
impl Backend for Shared {
    fn kind(&self) -> BackendKind {
        BackendKind::Other("test")
    }
    fn records(&self) -> Arc<dyn RecordStore> {
        self.0.records()
    }
    fn idempotency(&self) -> Arc<dyn IdempotencyRecords> {
        self.0.idempotency()
    }
    fn outbox(&self) -> Arc<dyn Outbox> {
        self.0.outbox()
    }
    fn leader_lock(&self) -> Arc<dyn LeaderLock> {
        self.0.leader_lock()
    }
    fn janitor(&self) -> Arc<dyn Janitor> {
        self.0.janitor()
    }
    fn migrator(&self) -> Arc<dyn SchemaMigrator> {
        self.0.migrator()
    }
    fn kv(&self) -> Arc<dyn KvStore> {
        self.0.kv()
    }
    fn conversations(&self) -> Arc<dyn ConversationStore> {
        self.0.conversations()
    }
    async fn close(&self) {}
}

/// Opens the same backend whatever the storage.
struct Opens(Arc<dyn Backend>);

#[async_trait]
impl BackendFactory for Opens {
    async fn open(&self, _storage: &Storage) -> anyhow::Result<Arc<dyn Backend>> {
        Ok(Arc::clone(&self.0))
    }
}

/// A database that does not answer, as the built-in factory says it.
struct Unreachable;

#[async_trait]
impl BackendFactory for Unreachable {
    async fn open(&self, _storage: &Storage) -> anyhow::Result<Arc<dyn Backend>> {
        anyhow::bail!("cannot connect to DATABASE_URL: connection failed")
    }
}

/// A shared backend with `WABA` bound to a tenant, its token sealed under
/// `vault_key` (or none stored).
async fn deployment(token: Option<(&str, &str)>) -> Arc<dyn Backend> {
    let backend = Arc::new(Shared(MemoryBackend::new()));
    let tenant = TenantId::parse("merchant-42").unwrap();
    let records = backend.records();
    records
        .create_tenant(&tenant, "Merchant 42")
        .await
        .unwrap()
        .unwrap();
    records
        .bind_waba(&tenant, &WabaId::new(WABA).unwrap(), &[])
        .await
        .unwrap();
    if let Some((vault_key, value)) = token {
        let keys = VaultKeys::new(VaultKey::from_base64("k1", vault_key).unwrap());
        TokenVault::new(backend.kv(), keys)
            .unwrap()
            .store(&StoredBusinessToken::new(
                WabaId::new(WABA).unwrap(),
                AccessToken::new(value),
            ))
            .await
            .unwrap();
    }
    backend
}

/// The subscriptions-edge guide's example answer, for WhatsApp: object
/// `whatsapp_business_account` (`webhooks/create-webhook-endpoint`) with
/// `fields`.
fn subscriptions(fields: &[&str]) -> Value {
    json!({"data": [{
        "object": "whatsapp_business_account",
        "callback_url": "https://wa.example.com/webhooks/meta?route=SENTINEL-QUERY",
        "active": true,
        "fields": fields.iter().map(|f| json!({"name": f, "version": "v25.0"})).collect::<Vec<_>>(),
    }]})
}

/// Every field the doctor asks for.
fn all_fields() -> Vec<&'static str> {
    meta_whatsapp_server::doctor::REQUIRED_FIELDS
        .iter()
        .chain(meta_whatsapp_server::doctor::RECOMMENDED_FIELDS.iter())
        .copied()
        .collect()
}

/// `solution-providers/manage-accounts`' `debug_token` example, with
/// `expires_at` set to 0 (never: the example's date has passed).
fn debug_ok() -> Value {
    let mut debug = debug_example();
    debug["data"]["expires_at"] = json!(0);
    debug
}

/// `solution-providers/manage-accounts`' `debug_token` example, verbatim
/// (the page's comment removed).
fn debug_example() -> Value {
    json!({
      "data": {
        "app_id": "670843887433847",
        "application": "JaspersMarket",
        "data_access_expires_at": 1672092840,
        "expires_at": 1665090000,
        "granular_scopes": [
          {"scope": "whatsapp_business_management", "target_ids": ["102289599326934", "101569239400667"]},
          {"scope": "whatsapp_business_messaging", "target_ids": ["102289599326934", "101569239400667"]}
        ],
        "is_valid": true,
        "scopes": ["whatsapp_business_management", "whatsapp_business_messaging", "public_profile"],
        "type": "USER",
        "user_id": "10222270944537964"
      }
    })
}

/// `solution-providers/manage-webhooks`' `subscribed_apps` example, the
/// first app's id completed (the page truncates it as `67084...`).
fn subscribed_ok() -> Value {
    json!({"data": [
        {"whatsapp_business_api_data": {"id": APP_ID, "link": "https://www.facebook.com/games/?app_id=670843887433847", "name": "Jaspers Market"}},
        {"whatsapp_business_api_data": {"id": "52565...", "link": "https://www.facebook.com/games/?app_id=52565...", "name": "Jaspers Fresh Finds"}}
    ]})
}

/// Run the doctor on `env` and `backends`, Graph scripted by `graph`.
async fn doctor(env: &Vars, backends: &dyn BackendFactory, graph: &ScriptedTransport) -> Report {
    let report = Doctor::new(backends)
        .transport(Arc::new(graph.clone()))
        .run(env)
        .await;
    let text = report.to_string();
    for secret in SECRETS {
        assert!(!text.contains(secret), "a secret in the report:\n{text}");
    }
    assert!(
        !text.contains("SENTINEL"),
        "a sentinel in the report:\n{text}"
    );
    for request in graph.requests() {
        assert_eq!(request.method.as_str(), "GET", "doctor only reads");
    }
    report
}

/// `name`'s status and detail.
fn check(report: &Report, name: &str) -> (Status, String) {
    let check = report
        .get(name)
        .unwrap_or_else(|| panic!("no check {name} in:\n{report}"));
    (check.status, check.detail.clone())
}

/// `name` failed, saying `says`, and so did the run.
fn assert_fails(report: &Report, name: &str, says: &str) {
    let (status, detail) = check(report, name);
    assert_eq!(status, Status::Fail, "{name}: {detail}\n{report}");
    assert!(detail.contains(says), "{name}: `{detail}` lacks `{says}`");
    assert!(report.failed());
}

// ─── A healthy deployment ──────────────────────────────────────────────

#[tokio::test]
async fn a_healthy_deployment_passes_with_exactly_these_reads() {
    let backend = deployment(Some((VAULT_KEY, WABA_TOKEN))).await;
    let graph = ScriptedTransport::new();
    graph.push_json(200, subscriptions(&all_fields()));
    graph.push_json(200, debug_ok());
    graph.push_json(200, subscribed_ok());
    let report = doctor(&production(), &Opens(backend), &graph).await;
    assert!(!report.failed(), "{report}");
    assert_eq!(report.count(Status::Warn), 0, "{report}");
    for name in [
        "config",
        "config.WA_APP_SECRET",
        "config.WA_VERIFY_TOKEN",
        "config.binds",
        "config.WA_GRAPH_ENDPOINT",
        "config.WA_VAULT_KEY",
        "config.WA_APP_ID",
        "database",
        "webhooks.subscription",
        "webhooks.fields",
        "token.wabas",
        "token.waba.102289599326934",
        "webhooks.waba.102289599326934",
    ] {
        assert_eq!(check(&report, name).0, Status::Ok, "{name}\n{report}");
    }
    // Statuses padded to one column.
    let text = report.to_string();
    assert!(
        text.lines()
            .any(|l| l == format!("OK   config.WA_APP_ID: {APP_ID}")),
        "{text}"
    );
    // The callback URL, its query's value hidden.
    assert!(
        check(&report, "webhooks.subscription")
            .1
            .contains("https://wa.example.com/webhooks/meta?route=<redacted>"),
        "{report}"
    );
    // An integrator's backend: its migrations are its own.
    assert_eq!(check(&report, "database.migrations").0, Status::Skip);

    let app_token = format!("{APP_ID}|{APP_SECRET}");
    let requests = graph.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].path(), format!("/v25.0/{APP_ID}/subscriptions"));
    assert_eq!(requests[0].bearer(), Some(app_token.as_str()));
    assert_eq!(requests[1].path(), "/v25.0/debug_token");
    assert_eq!(
        requests[1].query("input_token").as_deref(),
        Some(WABA_TOKEN)
    );
    assert_eq!(requests[1].bearer(), Some(app_token.as_str()));
    assert_eq!(requests[2].path(), format!("/v25.0/{WABA}/subscribed_apps"));
    assert_eq!(requests[2].bearer(), Some(WABA_TOKEN));
    assert_eq!(graph.remaining(), 0);
}

#[tokio::test]
async fn the_report_is_one_line_per_check_then_the_counts() {
    let graph = ScriptedTransport::new();
    let env = production().unset("WA_APP_SECRET");
    let report = doctor(&env, &Unreachable, &graph).await;
    let text = report.to_string();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), report.checks.len() + 1, "{text}");
    assert_eq!(
        lines[0],
        "FAIL config.WA_APP_SECRET: WA_APP_SECRET is required"
    );
    assert_eq!(
        lines.last().copied(),
        Some(
            format!(
                "{} failed, 0 warnings, 0 ok, {} skipped",
                report.count(Status::Fail),
                report.count(Status::Skip)
            )
            .as_str()
        )
    );
}

// ─── The configuration ─────────────────────────────────────────────────

#[tokio::test]
async fn every_refusal_is_reported_by_name_not_only_the_first() {
    let graph = ScriptedTransport::new();
    let env = production()
        .unset("WA_APP_SECRET")
        .set("WA_VERIFY_TOKEN", "  ")
        .set("WA_OTP_PEPPER", "short")
        .set("WA_SERVER_MIGRATE", "sometimes")
        .set("WA_SERVER_RATE_SEND", "fast");
    let report = doctor(&env, &Unreachable, &graph).await;
    assert_fails(&report, "config.WA_APP_SECRET", "WA_APP_SECRET is required");
    assert_fails(
        &report,
        "config.WA_VERIFY_TOKEN",
        "WA_VERIFY_TOKEN is blank",
    );
    assert_fails(&report, "config.WA_OTP_PEPPER", "at least 32 bytes");
    assert_fails(&report, "config.WA_SERVER_MIGRATE", "expected auto or skip");
    assert_fails(
        &report,
        "config.WA_SERVER_RATE_SEND",
        "positive whole number",
    );
    // A refused configuration: no Graph call is made with it.
    assert_eq!(check(&report, "webhooks.subscription").0, Status::Skip);
    assert_eq!(check(&report, "token.wabas").0, Status::Skip);
    assert!(graph.requests().is_empty());
}

#[tokio::test]
async fn production_without_a_database_is_named_and_the_database_skipped() {
    let graph = ScriptedTransport::new();
    let report = doctor(&production().unset("DATABASE_URL"), &Unreachable, &graph).await;
    assert_fails(&report, "config.DATABASE_URL", "set DATABASE_URL");
    assert_eq!(check(&report, "database").0, Status::Skip);
}

#[tokio::test]
async fn identical_binds_and_a_bad_vault_key_are_both_named() {
    let graph = ScriptedTransport::new();
    let env = production()
        .set("WA_SERVER_PUBLIC_BIND", "127.0.0.1:9000")
        .set("WA_SERVER_INTERNAL_BIND", "127.0.0.1:9000")
        .set("WA_VAULT_KEY", "not-32-bytes");
    let report = doctor(&env, &Unreachable, &graph).await;
    assert_fails(&report, "config.WA_SERVER_PUBLIC_BIND", "must differ");
    assert_fails(&report, "config.WA_VAULT_KEY", "exactly 32 bytes");
}

#[tokio::test]
async fn solution_partner_mode_without_its_settings_names_each() {
    let graph = ScriptedTransport::new();
    let env = production().set("WA_ONBOARDING_MODE", "solution_partner");
    let report = doctor(&env, &Unreachable, &graph).await;
    for name in [
        "WA_PARTNER_SYSTEM_TOKEN",
        "WA_PARTNER_SYSTEM_USER_ID",
        "WA_CREDIT_LINE_ID",
        "WA_WABA_CURRENCY",
    ] {
        assert_fails(&report, &format!("config.{name}"), name);
    }
}

#[tokio::test]
async fn a_refused_configuration_still_checks_the_database() {
    let graph = ScriptedTransport::new();
    let env = production().unset("WA_VERIFY_TOKEN");
    let report = doctor(&env, &Unreachable, &graph).await;
    assert_fails(&report, "config.WA_VERIFY_TOKEN", "required");
    assert_fails(&report, "database", "cannot connect to DATABASE_URL");
}

#[tokio::test]
async fn without_an_app_id_the_graph_checks_are_skipped_with_a_warning() {
    let graph = ScriptedTransport::new();
    let backend = deployment(Some((VAULT_KEY, WABA_TOKEN))).await;
    let report = doctor(&production().unset("WA_APP_ID"), &Opens(backend), &graph).await;
    assert_eq!(check(&report, "config.WA_APP_ID").0, Status::Warn);
    assert_eq!(check(&report, "webhooks.subscription").0, Status::Skip);
    assert_eq!(check(&report, "token.wabas").0, Status::Skip);
    assert!(!report.failed(), "{report}");
    assert!(graph.requests().is_empty());
}

#[tokio::test]
async fn development_on_memory_warns_and_skips_what_it_cannot_see() {
    let graph = ScriptedTransport::new();
    graph.push_json(200, subscriptions(&all_fields()));
    let env = Vars::default()
        .set("WA_SERVER_ENV", "development")
        .set("WA_APP_ID", APP_ID)
        .set("WA_APP_SECRET", APP_SECRET)
        .set("WA_VERIFY_TOKEN", VERIFY_TOKEN)
        .set("WA_GRAPH_ENDPOINT", "http://127.0.0.1:9999/");
    let report = doctor(&env, &BuiltinBackends, &graph).await;
    assert_eq!(check(&report, "config.WA_VAULT_KEY").0, Status::Warn);
    assert_eq!(check(&report, "config.WA_GRAPH_ENDPOINT").0, Status::Warn);
    assert!(
        check(&report, "config.WA_GRAPH_ENDPOINT")
            .1
            .contains("127.0.0.1:9999")
    );
    assert_eq!(check(&report, "database").0, Status::Warn);
    assert_eq!(check(&report, "token.wabas").0, Status::Skip);
    assert!(!report.failed(), "{report}");
    assert_eq!(graph.remaining(), 0);
    // The configured endpoint, not Meta's.
    assert_eq!(
        graph.requests()[0].url.host_str(),
        Some("127.0.0.1"),
        "{report}"
    );
}

// ─── The database ──────────────────────────────────────────────────────

#[tokio::test]
async fn an_unreachable_database_fails_and_skips_what_needs_it() {
    let graph = ScriptedTransport::new();
    graph.push_json(200, subscriptions(&all_fields()));
    let report = doctor(&production(), &Unreachable, &graph).await;
    assert_fails(&report, "database", "cannot connect to DATABASE_URL");
    assert_eq!(check(&report, "database.migrations").0, Status::Skip);
    assert_eq!(check(&report, "token.wabas").0, Status::Skip);
    assert_eq!(graph.remaining(), 0);
}

// ─── Meta's webhook subscription ───────────────────────────────────────

#[tokio::test]
async fn an_app_without_a_whatsapp_subscription_fails() {
    let graph = ScriptedTransport::new();
    // The subscriptions-edge guide's example, verbatim: a `user` object.
    graph.push_json(
        200,
        json!({"data": [{
            "object": "user",
            "callback_url": "https://your-clever-domain-name.com/webhooks",
            "active": true,
            "fields": [{"name": "photos", "version": "v2.10"}, {"name": "feed", "version": "v2.10"}]
        }]}),
    );
    let report = doctor(&production(), &Unreachable, &graph).await;
    assert_fails(
        &report,
        "webhooks.subscription",
        "no `whatsapp_business_account` subscription",
    );
    assert_fails(&report, "webhooks.fields", "messages, account_update");
}

#[tokio::test]
async fn an_inactive_subscription_fails() {
    let graph = ScriptedTransport::new();
    let mut answer = subscriptions(&all_fields());
    answer["data"][0]["active"] = json!(false);
    graph.push_json(200, answer);
    let report = doctor(&production(), &Unreachable, &graph).await;
    assert_fails(&report, "webhooks.subscription", "inactive");
}

#[tokio::test]
async fn a_missing_required_field_is_named() {
    let graph = ScriptedTransport::new();
    let fields: Vec<&str> = all_fields()
        .into_iter()
        .filter(|f| *f != "messages" && *f != "security")
        .collect();
    graph.push_json(200, subscriptions(&fields));
    let report = doctor(&production(), &Unreachable, &graph).await;
    assert_fails(
        &report,
        "webhooks.fields",
        "not subscribed: messages; nor security",
    );
}

#[tokio::test]
async fn a_missing_recommended_field_warns_by_name() {
    let graph = ScriptedTransport::new();
    let backend = deployment(Some((VAULT_KEY, WABA_TOKEN))).await;
    let fields: Vec<&str> = all_fields()
        .into_iter()
        .filter(|f| *f != "message_template_status_update")
        .collect();
    graph.push_json(200, subscriptions(&fields));
    graph.push_json(200, debug_ok());
    graph.push_json(200, subscribed_ok());
    let report = doctor(&production(), &Opens(backend), &graph).await;
    let (status, detail) = check(&report, "webhooks.fields");
    assert_eq!(status, Status::Warn, "{report}");
    assert_eq!(detail, "not subscribed: message_template_status_update");
    assert!(!report.failed(), "{report}");
}

/// The Graph reference's table types `fields` as `string[]`.
#[tokio::test]
async fn fields_as_strings_are_read_too() {
    let graph = ScriptedTransport::new();
    graph.push_json(
        200,
        json!({"data": [{"object": "whatsapp_business_account", "active": true,
            "callback_url": "https://wa.example.com/webhooks/meta", "fields": all_fields()}]}),
    );
    let report = doctor(&production(), &Unreachable, &graph).await;
    assert_eq!(check(&report, "webhooks.fields").0, Status::Ok, "{report}");
}

#[tokio::test]
async fn a_graph_refusal_is_reported_by_kind_and_code_never_by_metas_text() {
    let graph = ScriptedTransport::new();
    graph.push_json(
        400,
        json!({"error": {"message": "SENTINEL-META-TEXT Invalid OAuth access token.",
            "type": "OAuthException", "code": 190, "error_subcode": 463,
            "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let report = doctor(&production(), &Unreachable, &graph).await;
    assert_fails(&report, "webhooks.subscription", "(code 190, subcode 463)");
    assert_eq!(check(&report, "webhooks.fields").0, Status::Skip);
}

// ─── The tokens' reach ─────────────────────────────────────────────────

/// The report of a deployment whose WABA's token Meta describes as
/// `debug`, its app subscribed.
async fn with_debug(debug: Value) -> Report {
    let graph = ScriptedTransport::new();
    let backend = deployment(Some((VAULT_KEY, WABA_TOKEN))).await;
    graph.push_json(200, subscriptions(&all_fields()));
    graph.push_json(200, debug);
    graph.push_json(200, subscribed_ok());
    let report = doctor(&production(), &Opens(backend), &graph).await;
    assert_eq!(graph.remaining(), 0);
    report
}

#[tokio::test]
async fn an_expired_token_fails() {
    // The example verbatim: it expired in 2022.
    let report = with_debug(debug_example()).await;
    assert_fails(&report, "token.waba.102289599326934", "it has expired");
}

#[tokio::test]
async fn a_token_expiring_soon_warns() {
    let mut debug = debug_ok();
    let soon = time::OffsetDateTime::now_utc().unix_timestamp() + 2 * 86_400;
    debug["data"]["expires_at"] = json!(soon);
    let report = with_debug(debug).await;
    let (status, detail) = check(&report, "token.waba.102289599326934");
    assert_eq!(status, Status::Warn, "{report}");
    assert!(detail.contains("expires within 7 days"), "{detail}");
}

#[tokio::test]
async fn a_token_meta_says_is_invalid_or_another_apps_fails() {
    let mut debug = debug_ok();
    debug["data"]["is_valid"] = json!(false);
    debug["data"]["app_id"] = json!("236484624622562");
    let report = with_debug(debug).await;
    assert_fails(&report, "token.waba.102289599326934", "not valid");
    assert_fails(
        &report,
        "token.waba.102289599326934",
        "issued for another app (236484624622562)",
    );
}

#[tokio::test]
async fn a_token_without_a_permission_fails_naming_it() {
    let mut debug = debug_ok();
    debug["data"]["scopes"] = json!(["whatsapp_business_management", "public_profile"]);
    let report = with_debug(debug).await;
    assert_fails(
        &report,
        "token.waba.102289599326934",
        "it lacks whatsapp_business_messaging",
    );
}

#[tokio::test]
async fn a_token_whose_grant_does_not_name_its_waba_fails() {
    let mut debug = debug_ok();
    for i in 0..2 {
        debug["data"]["granular_scopes"][i]["target_ids"] = json!(["101569239400667"]);
    }
    let report = with_debug(debug).await;
    assert_fails(
        &report,
        "token.waba.102289599326934",
        "its whatsapp_business_management grant does not name WABA 102289599326934",
    );
}

#[tokio::test]
async fn a_waba_its_app_is_not_subscribed_to_fails() {
    let graph = ScriptedTransport::new();
    let backend = deployment(Some((VAULT_KEY, WABA_TOKEN))).await;
    graph.push_json(200, subscriptions(&all_fields()));
    graph.push_json(200, debug_ok());
    // `solution-providers/manage-webhooks`' example verbatim: two other apps.
    graph.push_json(
        200,
        json!({"data": [
            {"whatsapp_business_api_data": {"id": "67084...", "link": "https://www.facebook.com/games/?app_id=67084...", "name": "Jaspers Market"}},
            {"whatsapp_business_api_data": {"id": "52565...", "link": "https://www.facebook.com/games/?app_id=52565...", "name": "Jaspers Fresh Finds"}}
        ]}),
    );
    let report = doctor(&production(), &Opens(backend), &graph).await;
    assert_fails(&report, "webhooks.waba.102289599326934", "not subscribed");
    assert_eq!(graph.remaining(), 0);
}

#[tokio::test]
async fn a_bound_waba_without_a_token_fails_and_is_not_inspected() {
    let graph = ScriptedTransport::new();
    let backend = deployment(None).await;
    graph.push_json(200, subscriptions(&all_fields()));
    let report = doctor(&production(), &Opens(backend), &graph).await;
    assert_fails(
        &report,
        "token.waba.102289599326934",
        "no token in the vault",
    );
    assert_eq!(graph.requests().len(), 1);
}

#[tokio::test]
async fn a_vault_key_that_cannot_open_a_token_fails_naming_the_variables() {
    let graph = ScriptedTransport::new();
    let backend = deployment(Some((OTHER_VAULT_KEY, WABA_TOKEN))).await;
    graph.push_json(200, subscriptions(&all_fields()));
    let report = doctor(&production(), &Opens(backend), &graph).await;
    assert_fails(
        &report,
        "token.waba.102289599326934",
        "check WA_VAULT_KEY and WA_VAULT_PREVIOUS_KEYS",
    );
}

#[tokio::test]
async fn a_solution_partners_system_token_is_inspected() {
    let graph = ScriptedTransport::new();
    let backend: Arc<dyn Backend> = Arc::new(Shared(MemoryBackend::new()));
    let env = production()
        .set("WA_ONBOARDING_MODE", "solution_partner")
        .set("WA_PARTNER_SYSTEM_TOKEN", PARTNER_TOKEN)
        .set("WA_PARTNER_SYSTEM_USER_ID", "104169029247128")
        .set("WA_CREDIT_LINE_ID", "1972385232742146")
        .set("WA_WABA_CURRENCY", "USD");
    graph.push_json(200, subscriptions(&all_fields()));
    let mut debug = debug_ok();
    debug["data"]["is_valid"] = json!(false);
    graph.push_json(200, debug);
    let report = doctor(&env, &Opens(backend), &graph).await;
    assert_fails(&report, "token.partner", "not valid");
    assert_eq!(check(&report, "token.wabas").1, "no WABA bound yet");
    let requests = graph.requests();
    assert_eq!(requests[1].path(), "/v25.0/debug_token");
    assert_eq!(
        requests[1].query("input_token").as_deref(),
        Some(PARTNER_TOKEN)
    );
    assert_eq!(graph.remaining(), 0);
}

// ─── The command ───────────────────────────────────────────────────────

/// The real binary: a refused configuration is reported on standard
/// output, by name, and the exit code is non-zero. No network: a refused
/// configuration makes no Graph call, and no database is configured.
#[test]
fn the_command_exits_non_zero_naming_the_misconfiguration() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_meta-whatsapp-server"))
        .arg("doctor")
        .env_clear()
        .env("WA_VERIFY_TOKEN", VERIFY_TOKEN)
        .env("WA_VAULT_KEY", VAULT_KEY)
        .env("WA_OTP_PEPPER", PEPPER)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!output.status.success(), "{stdout}{stderr}");
    assert!(
        stdout
            .lines()
            .any(|l| l == "FAIL config.WA_APP_SECRET: WA_APP_SECRET is required"),
        "{stdout}"
    );
    assert!(
        stdout
            .lines()
            .any(|l| l.starts_with("FAIL config.DATABASE_URL: ")),
        "{stdout}"
    );
    assert!(stderr.contains("doctor: 2 checks failed"), "{stderr}");
    assert!(!stdout.contains(VERIFY_TOKEN) && !stderr.contains(VERIFY_TOKEN));
}

// ─── Live: the migration histories, read-only ──────────────────────────

/// On a fresh schema the histories are missing: a failure with
/// `WA_SERVER_MIGRATE=skip`, and the doctor creates nothing; once
/// migrated, up to date.
#[tokio::test]
async fn live_postgres_doctor_reads_the_migration_histories_without_migrating() {
    use meta_whatsapp_rs::adapters::store::postgres::sqlx;
    let Some(db) = common::TestDb::new().await else {
        return;
    };
    let separator = if db.url.contains('?') { '&' } else { '?' };
    let url = format!(
        "{}{separator}options=-c%20search_path%3D{}",
        db.url, db.schema
    );
    let env = production()
        .unset("WA_APP_ID")
        .set("DATABASE_URL", &url)
        .set("WA_SERVER_MIGRATE", "skip");
    let graph = ScriptedTransport::new();
    let report = Doctor::new(&BuiltinBackends)
        .transport(Arc::new(graph.clone()))
        .run(&env)
        .await;
    assert_eq!(check(&report, "database").0, Status::Ok, "{report}");
    assert_fails(&report, "database.migrations", "WA_SERVER_MIGRATE=skip");
    let pool = db.pool(2).await;
    let tables: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables WHERE table_schema = $1",
    )
    .bind(&db.schema)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(tables, 0, "the doctor created tables");

    meta_whatsapp_server::store::migrate(&pool).await.unwrap();
    pool.close().await;
    let report = Doctor::new(&BuiltinBackends)
        .transport(Arc::new(graph.clone()))
        .run(&env)
        .await;
    let (status, detail) = check(&report, "database.migrations");
    assert_eq!(status, Status::Ok, "{report}");
    assert!(detail.contains("up to date"), "{detail}");
    assert!(!report.failed(), "{report}");
    assert!(graph.requests().is_empty());
}
