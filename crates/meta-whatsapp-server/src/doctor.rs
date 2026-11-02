//! `meta-whatsapp-server doctor`: what would stop the service, or stop
//! Meta's events and the tokens from reaching it, checked read-only and
//! reported together (roadmap L22a).
//!
//! One line per check, `OK`, `WARN`, `FAIL` or `SKIP` (a check that could
//! not run, and why), then a count; the command exits non-zero when any
//! check failed. The checks, in order:
//!
//! | Check | Reads | Fails when |
//! | --- | --- | --- |
//! | `config.<VARIABLE>` | the environment, as `serve` reads it ([`Config::from_env`]) | a variable `serve` refuses: **every** one, not only the first |
//! | `config`, `config.WA_APP_SECRET`, `config.WA_VERIFY_TOKEN`, `config.binds`, `config.WA_GRAPH_ENDPOINT`, `config.WA_VAULT_KEY`, `config.WA_APP_ID`, `config.WA_ONBOARDING_MODE` | the configuration read | never: what was read (no value of a secret), and warnings (a throwaway vault key, a Graph endpoint that is not Meta's, no `WA_APP_ID`) |
//! | `database` | the backend `serve` would open, connected through the [`BackendFactory`], not migrated | the build lacks the backend's feature, or the database does not answer |
//! | `otp.default_tenant` | with `WA_SERVER_OTP_DEFAULT_TENANT` set, on a shared backend: the tenant it names, its OTP settings and their sending number's binding | never: it warns when the tenant is missing or suspended, has no OTP settings, or its settings name a number it no longer holds (tenants without settings of their own then get `409 otp_not_configured`; roadmap M3c) |
//! | `database.migrations` | on Postgres, the migration histories (`wa_server_sqlx_migrations`, `wa_sqlx_migrations`), by `SELECT` only | a service migration is pending with `WA_SERVER_MIGRATE=skip` (a warning with `auto`: `serve` applies it), a recorded one differs from this binary's, or one was left half-applied |
//! | `webhooks.subscription`, `webhooks.fields` | `GET /{app-id}/subscriptions` with the app token | the app has no active `whatsapp_business_account` subscription, or lacks a field the service needs ([`REQUIRED_FIELDS`]; the others of [`RECOMMENDED_FIELDS`] warn) |
//! | `token.partner` | `GET /debug_token` on `WA_PARTNER_SYSTEM_TOKEN` (Solution Partner mode) | Meta says it is invalid, expired, or another app's |
//! | `token.wabas`, `token.waba.<id>`, `webhooks.waba.<id>` | every bound WABA (at most [`MAX_WABAS`]): its token from the vault (read without re-encrypting), `GET /debug_token` on it, `GET /{waba-id}/subscribed_apps` with it | no token, a vault key that cannot open it, a token Meta says is invalid, expired, another app's, without `whatsapp_business_management` or `whatsapp_business_messaging`, or whose grant does not name the WABA; the app not subscribed to the WABA |
//!
//! **Every problem is reported.** After a refusal, the reading is repeated
//! with the refused variable replaced by the most permissive value `serve`
//! accepts (unset when it has a default), so one mistake does not hide the
//! next; a replacement can hide a problem that only the intended value
//! would show. A refused configuration skips the Graph checks (they need
//! its app secret, endpoint and vault keys); the database is still checked
//! when `DATABASE_URL` alone reads.
//!
//! **Read-only.** No migration runs, the vault re-encrypts nothing, and
//! every Graph call is a `GET` through the library's client (its
//! `GraphRequest`: credentials only to the configured Graph host).
//!
//! **No secret is printed**: variables are named, never quoted; a URL is
//! shown through [`redacted_url`]; a Graph failure by its error kind and
//! code, never Meta's text.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use meta_whatsapp_rs::client::embedded_signup::{
    EmbeddedSignup, TokenDebug, TokenVault, WHATSAPP_BUSINESS_MANAGEMENT,
    WHATSAPP_BUSINESS_MESSAGING,
};
use meta_whatsapp_rs::core::config::GraphEndpoint;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{AppId, WabaId};
use meta_whatsapp_rs::core::secret::{AccessToken, redacted_url};
use meta_whatsapp_rs::core::transport::HttpTransport;
use meta_whatsapp_rs::{AppCredentials, Client, Error};
use meta_whatsapp_server_http::api::common::rfc3339;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::config::{
    self, Config, ConfigError, DatabaseUrl, Env, Environment, MigrateMode, NotUnicode,
    OnboardingMode, Storage,
};
use crate::model::{MAX_PAGE_SIZE, PageRequest, TenantStatus};
use crate::serve::{self, BackendFactory};
use crate::store::{Backend, BackendKind};

/// The webhook fields without which the service misses what it is for:
/// Meta's `messages` (messages in, statuses, errors) and `account_update`
/// (a WABA offboarded, banned, deleted). Missing, `webhooks.fields` fails.
/// Names from Meta's `webhooks/overview`, § Fields.
pub const REQUIRED_FIELDS: [&str; 2] = ["messages", "account_update"];

/// The other fields behind the service's tenant event types
/// (`meta_whatsapp_server_core::events::TENANT_EVENT_TYPES`) that every
/// deployment gets from Meta: missing, `webhooks.fields` warns. Fields of
/// products a deployment may not use (calls, coexistence's `history`,
/// `smb_*`, payments, automatic events, partner solutions) are not asked
/// for. Names from Meta's `webhooks/overview`, § Fields.
pub const RECOMMENDED_FIELDS: [&str; 11] = [
    "account_alerts",
    "account_review_update",
    "business_capability_update",
    "message_template_components_update",
    "message_template_quality_update",
    "message_template_status_update",
    "phone_number_name_update",
    "phone_number_quality_update",
    "security",
    "template_category_update",
    "user_preferences",
];

/// The subscription object of WhatsApp's webhooks
/// (`webhooks/create-webhook-endpoint`).
pub const WHATSAPP_OBJECT: &str = "whatsapp_business_account";

/// The most bound WABAs whose token and subscription one run checks (two
/// Graph calls each); past it, `token.wabas` warns.
pub const MAX_WABAS: usize = 100;

/// A token expiring sooner than this warns.
pub const EXPIRY_WARNING: Duration = Duration::from_hours(7 * 24);

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// Fine.
    Ok,
    /// Works, but look at it.
    Warn,
    /// Broken: the command exits non-zero.
    Fail,
    /// Not checked: the detail says why.
    Skip,
}

impl Status {
    /// `OK`, `WARN`, `FAIL` or `SKIP`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

/// One check's line. Its detail names variables and ids, never a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// What was checked (`config.WA_APP_SECRET`, `webhooks.fields`, …).
    pub name: String,
    /// How it came out.
    pub status: Status,
    /// What was found.
    pub detail: String,
}

impl Check {
    fn new(status: Status, name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status,
            detail: detail.into(),
        }
    }

    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(Status::Ok, name, detail)
    }

    fn warn(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(Status::Warn, name, detail)
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(Status::Fail, name, detail)
    }

    fn skip(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(Status::Skip, name, detail)
    }
}

/// Every check of a run, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The checks.
    pub checks: Vec<Check>,
}

impl Report {
    /// Whether any check failed (the command then exits non-zero).
    pub fn failed(&self) -> bool {
        self.checks.iter().any(|c| c.status == Status::Fail)
    }

    /// The checks with `status`.
    pub fn count(&self, status: Status) -> usize {
        self.checks.iter().filter(|c| c.status == status).count()
    }

    /// The check named `name`.
    pub fn get(&self, name: &str) -> Option<&Check> {
        self.checks.iter().find(|c| c.name == name)
    }

    fn push(&mut self, check: Check) {
        self.checks.push(check);
    }
}

/// One line per check (`FAIL config.WA_APP_SECRET: WA_APP_SECRET is
/// required`), then the counts.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for check in &self.checks {
            writeln!(f, "{:<4} {}: {}", check.status, check.name, check.detail)?;
        }
        writeln!(
            f,
            "{} failed, {} warnings, {} ok, {} skipped",
            self.count(Status::Fail),
            self.count(Status::Warn),
            self.count(Status::Ok),
            self.count(Status::Skip)
        )
    }
}

/// The doctor: the backends `serve` would open, and the transport its
/// Graph calls take (the production one unless [`Self::transport`]).
pub struct Doctor<'a> {
    backends: &'a dyn BackendFactory,
    transport: Option<Arc<dyn HttpTransport>>,
}

impl fmt::Debug for Doctor<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Doctor").finish_non_exhaustive()
    }
}

impl<'a> Doctor<'a> {
    /// A doctor opening storage through `backends`.
    pub fn new(backends: &'a dyn BackendFactory) -> Self {
        Self {
            backends,
            transport: None,
        }
    }

    /// Send the Graph calls through `transport` (tests script one) instead
    /// of the production one. They still go through the library's client,
    /// to the configured endpoint.
    #[must_use]
    pub fn transport(mut self, transport: Arc<dyn HttpTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Run every check on `env`.
    pub async fn run(&self, env: &dyn Env) -> Report {
        let mut report = Report::default();
        let config = configuration(env, &mut report);
        let Some(config) = config else {
            // Only DATABASE_URL: what `migrate` reads.
            match config::database(env) {
                Ok((url, mode)) => {
                    let storage = Storage::Postgres(url);
                    if let Some(backend) = self.database(&storage, mode, &mut report).await {
                        backend.close().await;
                    }
                }
                Err(ConfigError::Missing(_)) => {
                    report.push(Check::skip("database", "DATABASE_URL is unset"));
                }
                Err(_) => report.push(Check::skip(
                    "database",
                    "DATABASE_URL or WA_SERVER_MIGRATE is refused (above)",
                )),
            }
            for name in ["webhooks.subscription", "token.wabas"] {
                report.push(Check::skip(name, "the configuration is refused"));
            }
            return report;
        };
        let backend = self
            .database(&config.storage, config.migrate, &mut report)
            .await;
        otp_default_tenant(&config, backend.as_ref(), &mut report).await;
        self.graph(config, backend.as_ref(), &mut report).await;
        if let Some(backend) = backend {
            backend.close().await;
        }
        report
    }

    /// The webhook subscription and the tokens' reach, on `config`.
    async fn graph(&self, config: Config, backend: Option<&Arc<dyn Backend>>, report: &mut Report) {
        let app = config
            .app_id
            .clone()
            .zip(config.app_secrets.first().cloned())
            .map(|(id, secret)| AppCredentials::new(id, secret));
        let partner = matches!(config.onboarding, OnboardingMode::SolutionPartner(_));
        let skipped = |report: &mut Report, why: &str| {
            for name in ["webhooks.subscription", "token.partner", "token.wabas"] {
                if name != "token.partner" || partner {
                    report.push(Check::skip(name, why));
                }
            }
        };
        let Some(app) = app else {
            skipped(report, "WA_APP_ID is unset: the app token needs it");
            return;
        };
        let Ok(client) = self.client(&config.graph_endpoint) else {
            skipped(report, "the Graph client cannot be built (its TLS backend)");
            return;
        };
        webhooks(&client, &app, report).await;
        if let OnboardingMode::SolutionPartner(partner) = &config.onboarding {
            let es = client.embedded_signup(app.clone());
            report.push(token(&es, "token.partner", &partner.system_token, None).await);
        }
        let Some(backend) = backend else {
            report.push(Check::skip("token.wabas", "the database is not reachable"));
            return;
        };
        if backend.kind().is_process_local() {
            report.push(Check::skip(
                "token.wabas",
                "memory storage is per process: this command cannot see the running service's WABAs",
            ));
            return;
        }
        match serve::vault(backend.kv(), config.vault_keys) {
            Ok(vault) => {
                // Read-only: a record under a previous key stays as it is.
                let vault = vault.rotate_on_read(false);
                wabas(backend.as_ref(), &vault, &client, &app, report).await;
            }
            Err(_) => report.push(Check::fail(
                "token.wabas",
                "the vault cannot be opened with WA_VAULT_KEY and WA_VAULT_PREVIOUS_KEYS",
            )),
        }
    }

    fn client(&self, endpoint: &GraphEndpoint) -> meta_whatsapp_rs::Result<Client> {
        let builder = match &self.transport {
            Some(transport) => Client::builder().shared_transport(Arc::clone(transport)),
            None => meta_whatsapp_rs::client_builder()?,
        };
        builder.endpoint(endpoint.clone()).build()
    }

    /// `database` and `database.migrations`; the backend, when it opened.
    async fn database(
        &self,
        storage: &Storage,
        mode: MigrateMode,
        report: &mut Report,
    ) -> Option<Arc<dyn Backend>> {
        let backend = match self.backends.open(storage).await {
            Ok(backend) => backend,
            Err(error) => {
                // The factory's errors name settings and features, never
                // their values (`BackendFactory::open`).
                report.push(Check::fail("database", format!("{error:#}")));
                report.push(Check::skip(
                    "database.migrations",
                    "the database is not reachable",
                ));
                return None;
            }
        };
        let kind = backend.kind();
        match (kind, storage) {
            (BackendKind::Memory, _) => {
                report.push(Check::warn(
                    "database",
                    "memory storage: per process, emptied on restart (WA_SERVER_ENV=development only)",
                ));
                report.push(Check::skip(
                    "database.migrations",
                    "memory storage has none",
                ));
            }
            (BackendKind::Postgres, Storage::Postgres(url)) => {
                report.push(Check::ok("database", "Postgres reachable"));
                report.push(migrations(url, mode).await);
            }
            (other, _) => {
                report.push(Check::ok(
                    "database",
                    format!("{} backend opened", other.name()),
                ));
                report.push(Check::skip(
                    "database.migrations",
                    format!("not checked on the {} backend", other.name()),
                ));
            }
        }
        Some(backend)
    }
}

// ─── OTP ───────────────────────────────────────────────────────────────

/// `otp.default_tenant`, when `WA_SERVER_OTP_DEFAULT_TENANT` is set: the
/// tenant whose OTP settings the tenants without their own borrow
/// (decision D8's default, D45) exists, is active, has settings, and
/// still holds their sending number; else a warning, since `serve` starts
/// anyway and only those tenants' OTP requests fail (`409
/// otp_not_configured`). Names the tenant and the number's id, nothing
/// else.
async fn otp_default_tenant(
    config: &Config,
    backend: Option<&Arc<dyn Backend>>,
    report: &mut Report,
) {
    const NAME: &str = "otp.default_tenant";
    let Some(id) = &config.otp_default_tenant else {
        return;
    };
    let Some(backend) = backend else {
        report.push(Check::skip(NAME, "the database is not reachable"));
        return;
    };
    if backend.kind().is_process_local() {
        report.push(Check::skip(
            NAME,
            "memory storage is per process: this command cannot see the running service's tenants",
        ));
        return;
    }
    let records = backend.records();
    let unreadable = |report: &mut Report, what: &str| {
        report.push(Check::warn(
            NAME,
            format!("WA_SERVER_OTP_DEFAULT_TENANT: {what} cannot be read"),
        ));
    };
    let borrowers = "tenants without OTP settings of their own get 409 otp_not_configured";
    let Ok(tenant) = records.tenant(id).await else {
        return unreadable(report, "the tenant");
    };
    match tenant {
        None => {
            return report.push(Check::warn(
                NAME,
                format!(
                    "WA_SERVER_OTP_DEFAULT_TENANT names {id}, which does not exist: {borrowers}"
                ),
            ));
        }
        Some(tenant) if tenant.status != TenantStatus::Active => {
            return report.push(Check::warn(
                NAME,
                format!("the default OTP tenant {id} is suspended: {borrowers}"),
            ));
        }
        Some(_) => {}
    }
    let settings = match records.otp_settings(id).await {
        Ok(Some(settings)) => settings,
        Ok(None) => {
            return report.push(Check::warn(
                NAME,
                format!("the default OTP tenant {id} has no OTP settings: {borrowers}"),
            ));
        }
        Err(_) => return unreadable(report, "the default OTP tenant's settings"),
    };
    let pn = &settings.phone_number_id;
    match records.number(pn).await {
        Ok(Some(binding)) if &binding.tenant_id == id => report.push(Check::ok(
            NAME,
            format!("{id}, sending from {pn}"),
        )),
        Ok(_) => report.push(Check::warn(
            NAME,
            format!(
                "the default OTP tenant {id}'s settings name {pn}, which it no longer holds: {borrowers}"
            ),
        )),
        Err(_) => unreadable(report, "the sending number's binding"),
    }
}

// ─── Configuration ─────────────────────────────────────────────────────

/// The variable a refusal is about.
fn subject(error: &ConfigError) -> &'static str {
    match error {
        ConfigError::Missing(name)
        | ConfigError::Blank(name)
        | ConfigError::Ambiguous(name)
        | ConfigError::Unreadable(name)
        | ConfigError::Invalid { name, .. }
        | ConfigError::PartnerSettingMissing(name) => name,
        ConfigError::MemoryOutsideDevelopment => "DATABASE_URL",
        ConfigError::VaultKeyRequired => "WA_VAULT_KEY",
        ConfigError::PepperRequired | ConfigError::PepperTooShort => "WA_OTP_PEPPER",
        ConfigError::SameBinds => "WA_SERVER_PUBLIC_BIND",
        ConfigError::ConfigFileNotSupported => "WA_SERVER_CONFIG",
    }
}

/// The most permissive value `serve` accepts for `name`, or `None` to
/// unset it (its default). Never used for anything but reading on.
fn stand_in(name: &str) -> Option<&'static str> {
    match name {
        "WA_APP_SECRET"
        | "WA_VERIFY_TOKEN"
        | "WA_PARTNER_SYSTEM_TOKEN"
        | "WA_PARTNER_SYSTEM_USER_ID"
        | "WA_CREDIT_LINE_ID" => Some("doctor-stand-in"),
        "WA_WABA_CURRENCY" => Some("USD"),
        "DATABASE_URL" => Some("postgres://doctor.invalid/stand-in"),
        // Base64 of 32 zero bytes.
        "WA_VAULT_KEY" => Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
        "WA_OTP_PEPPER" => Some("doctor-stand-in-pepper-of-32-bytes-or-more"),
        "WA_SERVER_ENV" => Some("development"),
        _ => None,
    }
}

/// `base` with some variables replaced (and their `_FILE` unset).
struct Overlay<'a> {
    base: &'a dyn Env,
    replaced: HashMap<String, Option<&'static str>>,
}

impl Overlay<'_> {
    fn replace(&mut self, name: &str, value: Option<&'static str>) {
        self.replaced.insert(name.to_owned(), value);
        self.replaced.insert(format!("{name}_FILE"), None);
    }
}

impl Env for Overlay<'_> {
    fn var(&self, name: &str) -> Result<Option<String>, NotUnicode> {
        match self.replaced.get(name) {
            Some(value) => Ok(value.map(str::to_owned)),
            None => self.base.var(name),
        }
    }

    fn read_file(&self, path: &str) -> std::io::Result<String> {
        self.base.read_file(path)
    }
}

/// Every refusal of `env`, each as `config.<VARIABLE>`; the configuration
/// when there is none.
fn configuration(env: &dyn Env, report: &mut Report) -> Option<Config> {
    let mut overlay = Overlay {
        base: env,
        replaced: HashMap::new(),
    };
    let mut refused = BTreeSet::new();
    // Each round replaces at least one new variable (or, for binds that
    // collide, unsets both, after which their defaults differ): bounded by
    // how many the configuration has.
    for _ in 0..64 {
        let error = match Config::from_env(&overlay) {
            Ok(config) if refused.is_empty() => {
                describe(&config, report);
                return Some(config);
            }
            Ok(_) => return None,
            Err(error) => error,
        };
        if matches!(error, ConfigError::SameBinds) {
            const BINDS: [&str; 2] = ["WA_SERVER_PUBLIC_BIND", "WA_SERVER_INTERNAL_BIND"];
            // A bind doctor unset (to its default) can meet the other's
            // value: that collision is the replacement's, not the setting's.
            if !BINDS.iter().any(|b| refused.contains(b)) {
                report.push(Check::fail(
                    format!("config.{}", subject(&error)),
                    error.to_string(),
                ));
            }
            refused.extend(BINDS);
            for bind in BINDS {
                overlay.replace(bind, None);
            }
            continue;
        }
        let name = subject(&error);
        if !refused.insert(name) {
            // Refused again: about the stand-in, not the value set. Stop
            // rather than loop, or report doctor's value as the operator's.
            return None;
        }
        report.push(Check::fail(format!("config.{name}"), error.to_string()));
        overlay.replace(name, stand_in(name));
    }
    None
}

/// What a configuration `serve` accepts holds, without a secret's value.
fn describe(config: &Config, report: &mut Report) {
    let environment = match config.environment {
        Environment::Production => "production",
        Environment::Development => "development",
    };
    let storage = match config.storage {
        Storage::Postgres(_) => "Postgres (DATABASE_URL)",
        Storage::Memory => "memory",
    };
    report.push(Check::ok(
        "config",
        format!("accepted: {environment}, {storage}"),
    ));
    report.push(Check::ok(
        "config.WA_APP_SECRET",
        if config.app_secrets.len() > 1 {
            "set, with WA_APP_SECRET_PREVIOUS (rotating)"
        } else {
            "set"
        },
    ));
    report.push(Check::ok(
        "config.WA_VERIFY_TOKEN",
        "set (the App Dashboard's verify token must be the same)",
    ));
    report.push(Check::ok(
        "config.binds",
        format!(
            "public {} (Meta's webhook), internal {} (the API: keep it private)",
            config.public_bind, config.internal_bind
        ),
    ));
    let version = config.graph_endpoint.version();
    let base = config.graph_endpoint.base().as_str();
    report.push(if base == GraphEndpoint::PRODUCTION {
        Check::ok(
            "config.WA_GRAPH_ENDPOINT",
            format!("Meta's Graph API, {version}"),
        )
    } else {
        Check::warn(
            "config.WA_GRAPH_ENDPOINT",
            format!(
                "{} ({version}) is not Meta's: every token travels to it",
                redacted_url(base).unwrap_or_else(|| "<redacted>".to_owned())
            ),
        )
    });
    report.push(if config.vault_key_is_throwaway {
        Check::warn(
            "config.WA_VAULT_KEY",
            "unset: a throwaway key, so every token is lost on restart (development)",
        )
    } else {
        Check::ok(
            "config.WA_VAULT_KEY",
            format!("set, active key id {}", config.vault_keys.active_id()),
        )
    });
    report.push(match &config.app_id {
        Some(id) => Check::ok("config.WA_APP_ID", id.as_str().to_owned()),
        None => Check::warn(
            "config.WA_APP_ID",
            "unset: the webhook subscription and token checks need it",
        ),
    });
    report.push(Check::ok(
        "config.WA_ONBOARDING_MODE",
        match config.onboarding {
            OnboardingMode::TechProvider => "tech_provider",
            OnboardingMode::SolutionPartner(_) => "solution_partner",
        },
    ));
}

// ─── Migrations ────────────────────────────────────────────────────────

#[cfg_attr(not(feature = "store-postgres"), allow(dead_code))]
/// What a migration history holds against what this binary carries.
#[derive(Debug, Default, PartialEq, Eq)]
struct History {
    /// Versions this binary has and the database has not.
    pending: Vec<i64>,
    /// Versions recorded with another checksum.
    changed: Vec<i64>,
    /// Versions the database has and this binary does not (a newer one ran).
    newer: Vec<i64>,
    /// A version left half-applied.
    dirty: Option<i64>,
}

#[cfg_attr(not(feature = "store-postgres"), allow(dead_code))]
/// `expected` (version, checksum) against `applied`.
fn compare(expected: &[(i64, Vec<u8>)], applied: &[(i64, Vec<u8>)], dirty: Option<i64>) -> History {
    let applied_map: HashMap<i64, &[u8]> =
        applied.iter().map(|(v, c)| (*v, c.as_slice())).collect();
    let expected_map: HashMap<i64, &[u8]> =
        expected.iter().map(|(v, c)| (*v, c.as_slice())).collect();
    let mut history = History {
        dirty,
        ..History::default()
    };
    for (version, checksum) in expected {
        match applied_map.get(version) {
            None => history.pending.push(*version),
            Some(recorded) if *recorded != checksum.as_slice() => history.changed.push(*version),
            Some(_) => {}
        }
    }
    history.newer = applied
        .iter()
        .map(|(v, _)| *v)
        .filter(|v| !expected_map.contains_key(v))
        .collect();
    history
}

#[cfg_attr(not(feature = "store-postgres"), allow(dead_code))]
fn versions(list: &[i64]) -> String {
    list.iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg_attr(not(feature = "store-postgres"), allow(dead_code))]
/// `database.migrations` from the service's history and the library's.
fn migration_check(
    service: Option<&History>,
    library: Option<(usize, Option<i64>)>,
    mode: MigrateMode,
) -> Check {
    const NAME: &str = "database.migrations";
    let pending = |what: String| match mode {
        MigrateMode::Auto => Check::warn(
            NAME,
            format!("{what}: `serve` applies them at start (WA_SERVER_MIGRATE=auto)"),
        ),
        MigrateMode::Skip => Check::fail(
            NAME,
            format!("{what}, and WA_SERVER_MIGRATE=skip: run `meta-whatsapp-server migrate`"),
        ),
    };
    let Some(service) = service else {
        return pending("no service tables yet (wa_server_sqlx_migrations is missing)".to_owned());
    };
    if let Some(version) = service.dirty {
        return Check::fail(
            NAME,
            format!("service migration {version} was left half-applied: repair it by hand"),
        );
    }
    if let Some((_, Some(version))) = library {
        return Check::fail(
            NAME,
            format!("library migration {version} was left half-applied: repair it by hand"),
        );
    }
    if !service.changed.is_empty() {
        return Check::fail(
            NAME,
            format!(
                "service migrations {} differ from this binary's: `migrate` would refuse them",
                versions(&service.changed)
            ),
        );
    }
    let Some((library_applied, None)) = library else {
        return pending("no library tables yet (wa_sqlx_migrations is missing)".to_owned());
    };
    if !service.pending.is_empty() {
        return pending(format!(
            "service migrations {} are pending",
            versions(&service.pending)
        ));
    }
    if !service.newer.is_empty() {
        return Check::warn(
            NAME,
            format!(
                "service migrations {} are newer than this binary (a later release migrated)",
                versions(&service.newer)
            ),
        );
    }
    Check::ok(
        NAME,
        format!(
            "the service's are up to date; the library's history has {library_applied} (its versions are not compared)"
        ),
    )
}

#[cfg(feature = "store-postgres")]
async fn migrations(url: &DatabaseUrl, mode: MigrateMode) -> Check {
    use meta_whatsapp_rs::adapters::store::postgres::sqlx;
    use sqlx::migrate::Migrate as _;

    /// `(applied (version, checksum), dirty)` of `table`, `None` when it
    /// does not exist. `SELECT`s only.
    async fn history(
        pool: &sqlx::PgPool,
        table: &str,
    ) -> Result<Option<(Vec<(i64, Vec<u8>)>, Option<i64>)>, &'static str> {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(pool)
            .await
            .map_err(|_| "the migration history cannot be read")?;
        if !exists {
            return Ok(None);
        }
        let mut conn = pool
            .acquire()
            .await
            .map_err(|_| "no connection for the migration history")?;
        let dirty = conn
            .dirty_version(table)
            .await
            .map_err(|_| "the migration history cannot be read")?;
        let applied = conn
            .list_applied_migrations(table)
            .await
            .map_err(|_| "the migration history cannot be read")?;
        Ok(Some((
            applied
                .into_iter()
                .map(|m| (m.version, m.checksum.into_owned()))
                .collect(),
            dirty,
        )))
    }

    let pool = match serve::connect(url).await {
        Ok(pool) => pool,
        Err(error) => return Check::fail("database.migrations", format!("{error:#}")),
    };
    let service = history(&pool, crate::store::MIGRATIONS_TABLE).await;
    let library = history(&pool, "wa_sqlx_migrations").await;
    pool.close().await;
    let (service, library) = match (service, library) {
        (Ok(service), Ok(library)) => (service, library),
        (Err(why), _) | (_, Err(why)) => return Check::fail("database.migrations", why),
    };
    let expected: Vec<(i64, Vec<u8>)> = crate::store::migrations()
        .into_iter()
        .map(|m| (m.version, m.checksum.into_owned()))
        .collect();
    let service = service.map(|(applied, dirty)| compare(&expected, &applied, dirty));
    let library = library.map(|(applied, dirty)| (applied.len(), dirty));
    migration_check(service.as_ref(), library, mode)
}

#[cfg(not(feature = "store-postgres"))]
#[allow(clippy::unused_async)] // the signature of the Postgres one
async fn migrations(_url: &DatabaseUrl, _mode: MigrateMode) -> Check {
    Check::skip(
        "database.migrations",
        "this build has no Postgres backend (the `store-postgres` feature)",
    )
}

// ─── Graph ─────────────────────────────────────────────────────────────

/// A Graph failure by kind and code: never Meta's text, which is not ours
/// to vouch for.
fn graph_failure(error: &Error) -> String {
    match error.graph() {
        Some(graph) => match graph.error_subcode {
            Some(subcode) => format!(
                "Graph answered {} (code {}, subcode {subcode})",
                error.kind().as_str(),
                graph.code
            ),
            None => format!(
                "Graph answered {} (code {})",
                error.kind().as_str(),
                graph.code
            ),
        },
        None => format!("the Graph call failed: {}", error.kind().as_str()),
    }
}

/// `GET /{app-id}/subscriptions`: `data` (`webhooks/subscriptions-edge`).
#[derive(Deserialize)]
struct Subscriptions {
    #[serde(default)]
    data: Vec<Subscription>,
}

/// One object's subscription: `object`, `callback_url`, `active`,
/// `fields` (the Graph reference's `application/subscriptions`).
#[derive(Deserialize)]
struct Subscription {
    #[serde(default)]
    object: String,
    #[serde(default)]
    callback_url: Option<String>,
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    fields: Vec<SubscribedField>,
}

/// A field: a string in the reference's table, `{name, version}` in the
/// subscriptions-edge guide's example; anything else is ignored.
#[derive(Deserialize)]
#[serde(untagged)]
enum SubscribedField {
    Named { name: String },
    Name(String),
    Other(serde::de::IgnoredAny),
}

impl SubscribedField {
    fn name(&self) -> Option<&str> {
        match self {
            Self::Named { name } | Self::Name(name) => Some(name),
            Self::Other(_) => None,
        }
    }
}

/// `webhooks.subscription` and `webhooks.fields`.
async fn webhooks(client: &Client, app: &AppCredentials, report: &mut Report) {
    let answer = client
        .get_at(&[app.app_id.as_str(), "subscriptions"])
        .bearer(&app.app_token())
        .context("app subscriptions response")
        .send::<Subscriptions>()
        .await;
    let subscriptions = match answer {
        Ok(subscriptions) => subscriptions,
        Err(error) => {
            report.push(Check::fail("webhooks.subscription", graph_failure(&error)));
            report.push(Check::skip(
                "webhooks.fields",
                "the subscription could not be read",
            ));
            return;
        }
    };
    let Some(whatsapp) = subscriptions
        .data
        .iter()
        .find(|s| s.object == WHATSAPP_OBJECT)
    else {
        report.push(Check::fail(
            "webhooks.subscription",
            format!(
                "the app has no `{WHATSAPP_OBJECT}` subscription: set the callback URL in the App Dashboard (WhatsApp, Configuration)"
            ),
        ));
        report.push(Check::fail(
            "webhooks.fields",
            format!("not subscribed: {}", REQUIRED_FIELDS.join(", ")),
        ));
        return;
    };
    let callback = whatsapp
        .callback_url
        .as_deref()
        .and_then(redacted_url)
        .unwrap_or_else(|| "no callback URL".to_owned());
    report.push(if whatsapp.active == Some(false) {
        Check::fail(
            "webhooks.subscription",
            format!("`{WHATSAPP_OBJECT}` subscription to {callback} is inactive"),
        )
    } else {
        Check::ok(
            "webhooks.subscription",
            format!("`{WHATSAPP_OBJECT}` to {callback}"),
        )
    });
    let subscribed: BTreeSet<&str> = whatsapp
        .fields
        .iter()
        .filter_map(SubscribedField::name)
        .collect();
    let missing = |fields: &[&'static str]| -> Vec<&'static str> {
        fields
            .iter()
            .copied()
            .filter(|f| !subscribed.contains(f))
            .collect()
    };
    let required = missing(&REQUIRED_FIELDS);
    let recommended = missing(&RECOMMENDED_FIELDS);
    report.push(if !required.is_empty() {
        let detail = if recommended.is_empty() {
            format!("not subscribed: {}", required.join(", "))
        } else {
            format!(
                "not subscribed: {}; nor {}",
                required.join(", "),
                recommended.join(", ")
            )
        };
        Check::fail("webhooks.fields", detail)
    } else if !recommended.is_empty() {
        Check::warn(
            "webhooks.fields",
            format!("not subscribed: {}", recommended.join(", ")),
        )
    } else {
        Check::ok(
            "webhooks.fields",
            format!(
                "{} fields, every one the service asks for",
                subscribed.len()
            ),
        )
    });
}

/// What `debug_token` says of a token, for `waba` when given.
fn reach(
    name: &str,
    debug: &TokenDebug,
    app_id: &AppId,
    waba: Option<&WabaId>,
    now: OffsetDateTime,
) -> Check {
    let mut fails = Vec::new();
    let mut warns = Vec::new();
    if !debug.is_valid {
        fails.push("Meta says it is not valid".to_owned());
    }
    if let Some(issued_for) = debug.app_id.as_ref().filter(|issued| *issued != app_id) {
        fails.push(format!("it was issued for another app ({issued_for})"));
    }
    let expiry = match debug.expires_at_time() {
        Some(at) if at <= now => {
            fails.push("it has expired".to_owned());
            String::new()
        }
        Some(at) => {
            if at - now < EXPIRY_WARNING {
                warns.push(format!(
                    "it expires within {} days",
                    EXPIRY_WARNING.as_secs() / 86_400
                ));
            }
            format!("expires at {}", rfc3339(at))
        }
        None => "never expires".to_owned(),
    };
    if let Some(waba) = waba {
        for scope in [WHATSAPP_BUSINESS_MANAGEMENT, WHATSAPP_BUSINESS_MESSAGING] {
            if !debug.has_scope(scope) {
                fails.push(format!("it lacks {scope}"));
            } else if let Some(targets) = debug.target_ids(scope)
                && !targets.iter().any(|t| t == waba.as_str())
            {
                fails.push(format!("its {scope} grant does not name WABA {waba}"));
            }
        }
    }
    if !fails.is_empty() {
        Check::fail(name, fails.join("; "))
    } else if !warns.is_empty() {
        Check::warn(name, warns.join("; "))
    } else {
        Check::ok(name, format!("valid, {expiry}"))
    }
}

/// `debug_token` on `token`, checked by [`reach`].
async fn token(
    es: &EmbeddedSignup,
    name: &str,
    token: &AccessToken,
    waba: Option<&WabaId>,
) -> Check {
    match es.debug_token(token).await {
        Ok(debug) => reach(
            name,
            &debug,
            &es.app().app_id,
            waba,
            OffsetDateTime::now_utc(),
        ),
        Err(error) => Check::fail(name, graph_failure(&error)),
    }
}

/// `token.wabas`, then `token.waba.<id>` and `webhooks.waba.<id>` for each
/// bound WABA, at most [`MAX_WABAS`].
async fn wabas(
    backend: &dyn Backend,
    vault: &TokenVault,
    client: &Client,
    app: &AppCredentials,
    report: &mut Report,
) {
    let records = backend.records();
    let mut bound = Vec::new();
    let mut page = PageRequest {
        after: None,
        limit: MAX_PAGE_SIZE,
    };
    let more = loop {
        let listing = match records.all_wabas(&page).await {
            Ok(listing) => listing,
            Err(error) => {
                // The kind only: a backend error's text (a sqlx error's,
                // say) is not ours to vouch for.
                let why = match error {
                    StorageError::Busy => "the database is busy",
                    StorageError::Corrupt { .. } => "a stored binding is unreadable",
                    _ => "the storage backend failed",
                };
                report.push(Check::fail(
                    "token.wabas",
                    format!("the bound WABAs cannot be listed: {why}"),
                ));
                return;
            }
        };
        bound.extend(listing.items.into_iter().map(|b| b.waba_id));
        if bound.len() >= MAX_WABAS {
            let more = bound.len() > MAX_WABAS || listing.next_after.is_some();
            bound.truncate(MAX_WABAS);
            break more;
        }
        let Some(after) = listing.next_after else {
            break false;
        };
        page.after = Some(after);
    };
    report.push(match (bound.len(), more) {
        (0, _) => Check::ok("token.wabas", "no WABA bound yet"),
        (n, false) => Check::ok("token.wabas", format!("{n} bound, each checked below")),
        (n, true) => Check::warn(
            "token.wabas",
            format!("more than {n} bound: the first {n} checked below"),
        ),
    });
    let es = client.embedded_signup(app.clone());
    for waba in &bound {
        let name = format!("token.waba.{waba}");
        let stored = match vault.get(waba).await {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                report.push(Check::fail(
                    name,
                    "no token in the vault: attach the WABA again",
                ));
                continue;
            }
            Err(error) => {
                report.push(Check::fail(
                    name,
                    format!(
                        "the vault cannot open its token ({}): check WA_VAULT_KEY and WA_VAULT_PREVIOUS_KEYS",
                        error.kind().as_str()
                    ),
                ));
                continue;
            }
        };
        report.push(token(&es, &name, &stored.token, Some(waba)).await);
        report.push(subscribed(client, &stored.token, waba, &app.app_id).await);
    }
}

/// `webhooks.waba.<id>`: whether `app_id` is among the WABA's subscribed
/// apps (`GET /{waba-id}/subscribed_apps`, with the WABA's token).
async fn subscribed(client: &Client, token: &AccessToken, waba: &WabaId, app_id: &AppId) -> Check {
    let name = format!("webhooks.waba.{waba}");
    let mut apps = std::pin::pin!(
        client
            .with_token(token.clone())
            .waba(waba.clone())
            .subscribed_apps_stream()
    );
    while let Some(app) = apps.next().await {
        match app {
            Ok(app) if app.whatsapp_business_api_data.id == app_id.as_str() => {
                return Check::ok(name, "the app is subscribed");
            }
            Ok(_) => {}
            Err(error) => return Check::fail(name, graph_failure(&error)),
        }
    }
    Check::fail(
        name,
        "the app is not subscribed to this WABA's webhooks: Meta sends nothing for its numbers (attach it again)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(versions: &[(i64, u8)]) -> Vec<(i64, Vec<u8>)> {
        versions.iter().map(|(v, c)| (*v, vec![*c])).collect()
    }

    #[test]
    fn a_history_names_what_is_pending_changed_and_newer() {
        let expected = v(&[(1, 1), (2, 2), (3, 3)]);
        assert_eq!(compare(&expected, &expected, None), History::default());
        let history = compare(&expected, &v(&[(1, 1), (2, 9), (4, 4)]), Some(4));
        assert_eq!(
            history,
            History {
                pending: vec![3],
                changed: vec![2],
                newer: vec![4],
                dirty: Some(4),
            }
        );
    }

    #[test]
    fn pending_migrations_fail_only_when_serve_would_not_apply_them() {
        let pending = History {
            pending: vec![3],
            ..History::default()
        };
        let auto = migration_check(Some(&pending), Some((5, None)), MigrateMode::Auto);
        assert_eq!(auto.status, Status::Warn, "{auto:?}");
        let skip = migration_check(Some(&pending), Some((5, None)), MigrateMode::Skip);
        assert_eq!(skip.status, Status::Fail, "{skip:?}");
        assert!(
            skip.detail.contains("service migrations 3 are pending"),
            "{skip:?}"
        );
        let none = migration_check(None, None, MigrateMode::Skip);
        assert_eq!(none.status, Status::Fail);
        let library = migration_check(Some(&History::default()), None, MigrateMode::Skip);
        assert_eq!(library.status, Status::Fail, "{library:?}");
        assert!(library.detail.contains("wa_sqlx_migrations"), "{library:?}");
    }

    #[test]
    fn a_changed_or_dirty_history_fails_whatever_the_mode() {
        let changed = History {
            changed: vec![2],
            ..History::default()
        };
        let dirty = History {
            dirty: Some(3),
            ..History::default()
        };
        for mode in [MigrateMode::Auto, MigrateMode::Skip] {
            assert_eq!(
                migration_check(Some(&changed), Some((5, None)), mode).status,
                Status::Fail
            );
            assert_eq!(
                migration_check(Some(&dirty), Some((5, None)), mode).status,
                Status::Fail
            );
            assert_eq!(
                migration_check(Some(&History::default()), Some((5, Some(7))), mode).status,
                Status::Fail
            );
        }
        let up_to_date = migration_check(
            Some(&History::default()),
            Some((5, None)),
            MigrateMode::Skip,
        );
        assert_eq!(up_to_date.status, Status::Ok, "{up_to_date:?}");
    }

    /// Every refusal `serve` has names a variable doctor can replace.
    #[test]
    fn every_refusal_has_a_subject() {
        for (error, name) in [
            (ConfigError::SameBinds, "WA_SERVER_PUBLIC_BIND"),
            (ConfigError::MemoryOutsideDevelopment, "DATABASE_URL"),
            (ConfigError::VaultKeyRequired, "WA_VAULT_KEY"),
            (ConfigError::PepperTooShort, "WA_OTP_PEPPER"),
            (ConfigError::ConfigFileNotSupported, "WA_SERVER_CONFIG"),
            (ConfigError::Missing("WA_APP_SECRET"), "WA_APP_SECRET"),
        ] {
            assert_eq!(subject(&error), name);
        }
    }
}
