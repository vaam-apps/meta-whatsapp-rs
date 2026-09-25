//! Configuration from the environment, refused at start when unsafe
//! (docs/design/server.md, sections 2.2 and 7.2).
//!
//! Every secret may come from `<NAME>` or from the file named by
//! `<NAME>_FILE` (not both; one trailing newline is dropped). Error
//! messages name variables, never their values.
//!
//! **The service refuses to start** on:
//!
//! | Refusal | [`ConfigError`] |
//! | --- | --- |
//! | a missing or blank app secret or verify token | `Missing`, `Blank` |
//! | a missing vault key with Postgres | `VaultKeyRequired` |
//! | a missing OTP pepper, or one under 32 bytes, with Postgres | `PepperRequired`, `PepperTooShort` |
//! | memory storage (no `DATABASE_URL`) outside `WA_SERVER_ENV=development` | `MemoryOutsideDevelopment` |
//! | identical public and internal binds | `SameBinds` |
//! | `WA_ONBOARDING_MODE=solution_partner` without its four settings | `PartnerSettingMissing` |
//! | a plain-`http` `WA_GRAPH_ENDPOINT` outside development (every token travels to it) | `Invalid` |
//! | a vault key id used twice (`WA_VAULT_KEY_ID`'s and `WA_VAULT_PREVIOUS_KEYS`') | `Invalid` |
//!
//! and on anything it cannot parse (`Invalid`), a variable set twice
//! (`Ambiguous`) or a secret file it cannot read (`Unreadable`).
//!
//! The service's own limits ([`Settings`], all optional):
//!
//! | Variable | Default | What |
//! | --- | --- | --- |
//! | `WA_SERVER_IDEMPOTENCY_TTL` | `24h` | how long an idempotency key's record is kept |
//! | `WA_SERVER_MEDIA_MAX_BYTES` | 104857600 (100 MiB) | largest upload, and largest streamed download |
//! | `WA_SERVER_MEDIA_CONCURRENCY` | 4 | uploads and whole-file downloads held in memory at once, per replica (half of them at most for one tenant) |
//! | `WA_SERVER_MEDIA_STREAMS` | 16 | streamed downloads at once, per replica (half of them at most for one tenant) |
//! | `WA_SERVER_RATE_SEND`, `WA_SERVER_RATE_SEND_BURST` | 20, 40 | per tenant and replica, a second ([`crate::ratelimit`]) |
//! | `WA_SERVER_RATE_READ`, `WA_SERVER_RATE_READ_BURST` | 50, 50 | the same, for reads |
//! | `WA_SERVER_RATE_TEMPLATES`, `WA_SERVER_RATE_TEMPLATES_BURST` | 2, 2 | the same, for template management |
//!
//! A rate set without its burst keeps the default's proportion (twice the
//! rate for sends, the rate itself for the others).
//! `WA_SERVER_CONFIG` (the TOML file of non-secrets) is not read yet: set,
//! it is refused (`ConfigFileNotSupported`) rather than ignored.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use meta_whatsapp_rs::client::authentication::OtpPepper;
use meta_whatsapp_rs::client::credit_lines::WabaCurrency;
use meta_whatsapp_rs::client::embedded_signup::{VaultKey, VaultKeys};
use meta_whatsapp_rs::core::config::{ApiVersion, GraphEndpoint};
use meta_whatsapp_rs::core::ids::AppId;
use meta_whatsapp_rs::core::secret::{AccessToken, AppSecret, VerifyToken};

use crate::ratelimit::{Rate, RateLimits};
use crate::state::Settings;

/// Why the service refuses to start. Never holds a value it read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// A required variable is unset.
    #[error("{0} is required")]
    Missing(&'static str),
    /// A required secret is empty or whitespace.
    #[error("{0} is blank")]
    Blank(&'static str),
    /// Both `NAME` and `NAME_FILE` are set.
    #[error("{0} and {0}_FILE are both set: set one")]
    Ambiguous(&'static str),
    /// `NAME_FILE` names a file that cannot be read.
    #[error("{0}_FILE: the file cannot be read")]
    Unreadable(&'static str),
    /// A value does not parse.
    #[error("{name}: {reason}")]
    Invalid {
        /// The variable.
        name: &'static str,
        /// What is expected (never the value read).
        reason: &'static str,
    },
    /// No `DATABASE_URL` outside development.
    #[error("memory storage is for WA_SERVER_ENV=development only: set DATABASE_URL (Postgres)")]
    MemoryOutsideDevelopment,
    /// Postgres without a vault key: tokens would be unreadable after a
    /// restart.
    #[error("WA_VAULT_KEY is required with Postgres (base64 of 32 random bytes)")]
    VaultKeyRequired,
    /// Postgres without an OTP pepper.
    #[error("WA_OTP_PEPPER is required with Postgres (at least 32 bytes)")]
    PepperRequired,
    /// An OTP pepper under 32 bytes.
    #[error("WA_OTP_PEPPER must be at least 32 bytes")]
    PepperTooShort,
    /// The public and internal listeners would share an address.
    #[error("WA_SERVER_PUBLIC_BIND and WA_SERVER_INTERNAL_BIND must differ")]
    SameBinds,
    /// Solution Partner mode without one of its settings.
    #[error("WA_ONBOARDING_MODE=solution_partner needs {0}")]
    PartnerSettingMissing(&'static str),
    /// `WA_SERVER_CONFIG` is set; the TOML file is not supported yet.
    #[error("WA_SERVER_CONFIG is not supported yet: set every setting in the environment")]
    ConfigFileNotSupported,
}

/// A variable set to something that is not UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotUnicode;

/// Where the variables come from: the process ([`ProcessEnv`]) or, in
/// tests, a map.
pub trait Env {
    /// The variable's value, `None` when unset.
    fn var(&self, name: &str) -> Result<Option<String>, NotUnicode>;
    /// The contents of a secret file.
    fn read_file(&self, path: &str) -> std::io::Result<String>;
}

/// The process environment and file system.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, name: &str) -> Result<Option<String>, NotUnicode> {
        match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(NotUnicode),
        }
    }

    fn read_file(&self, path: &str) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }
}

/// `production` (the default) or `development`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    /// Postgres required, secrets required.
    Production,
    /// Memory storage and a throwaway vault key allowed.
    Development,
}

/// Where the service keeps its records and the library's.
#[derive(Clone, PartialEq, Eq)]
pub enum Storage {
    /// Postgres at this URL (it may hold a password: never printed).
    Postgres(DatabaseUrl),
    /// Memory, emptied on restart (development only).
    Memory,
}

impl fmt::Debug for Storage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Postgres(_) => f.write_str("Postgres(..)"),
            Self::Memory => f.write_str("Memory"),
        }
    }
}

/// A database URL. `Debug` hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseUrl(String);

impl DatabaseUrl {
    /// The URL, for connecting.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DatabaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseUrl(..)")
    }
}

/// The onboarding mode of the deployment (per Meta app).
#[derive(Debug, Clone)]
pub enum OnboardingMode {
    /// Merchants pay Meta.
    TechProvider,
    /// The partner's credit line pays.
    SolutionPartner(PartnerSettings),
}

/// Solution Partner settings.
#[derive(Debug, Clone)]
pub struct PartnerSettings {
    /// `WA_PARTNER_SYSTEM_TOKEN` (never printed).
    pub system_token: AccessToken,
    /// `WA_PARTNER_SYSTEM_USER_ID`.
    pub system_user_id: String,
    /// `WA_CREDIT_LINE_ID`.
    pub credit_line_id: String,
    /// `WA_WABA_CURRENCY`.
    pub currency: WabaCurrency,
}

/// Whether `serve` migrates at start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateMode {
    /// Run the migrations at start (the default).
    Auto,
    /// A job runs `meta-whatsapp-server migrate`.
    Skip,
}

/// Log output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line (the default).
    Json,
    /// Human-readable.
    Text,
}

/// The service's settings. `Debug` shows no secret.
pub struct Config {
    /// `WA_SERVER_ENV`.
    pub environment: Environment,
    /// `WA_SERVER_PUBLIC_BIND` (Meta's webhook, `/livez`).
    pub public_bind: SocketAddr,
    /// `WA_SERVER_INTERNAL_BIND` (the API, operations).
    pub internal_bind: SocketAddr,
    /// `DATABASE_URL`, or memory.
    pub storage: Storage,
    /// `WA_APP_ID`, when set.
    pub app_id: Option<AppId>,
    /// `WA_APP_SECRET`, then `WA_APP_SECRET_PREVIOUS` when set.
    pub app_secrets: Vec<AppSecret>,
    /// `WA_VERIFY_TOKEN`.
    pub verify_token: VerifyToken,
    /// `WA_ES_CONFIG_ID`, when set.
    pub es_config_id: Option<String>,
    /// `WA_VAULT_KEY`, `WA_VAULT_KEY_ID`, `WA_VAULT_PREVIOUS_KEYS`; a
    /// throwaway key in development with memory storage.
    pub vault_keys: VaultKeys,
    /// Whether the vault key was generated for this process.
    pub vault_key_is_throwaway: bool,
    /// `WA_OTP_PEPPER`.
    pub otp_pepper: Option<OtpPepper>,
    /// `WA_ONBOARDING_MODE` and its settings.
    pub onboarding: OnboardingMode,
    /// `WA_GRAPH_API_VERSION` and `WA_GRAPH_ENDPOINT`.
    pub graph_endpoint: GraphEndpoint,
    /// `WA_SERVER_MIGRATE`.
    pub migrate: MigrateMode,
    /// `WA_SERVER_SHUTDOWN_GRACE`.
    pub shutdown_grace: Duration,
    /// `WA_SERVER_LOG_FORMAT`.
    pub log_format: LogFormat,
    /// `RUST_LOG`.
    pub log_filter: String,
    /// `WA_SERVER_IDEMPOTENCY_TTL`, `WA_SERVER_MEDIA_*`, `WA_SERVER_RATE_*`.
    pub settings: Settings,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("environment", &self.environment)
            .field("public_bind", &self.public_bind)
            .field("internal_bind", &self.internal_bind)
            .field("storage", &self.storage)
            .field("vault_keys", &self.vault_keys)
            .field(
                "onboarding",
                &matches!(self.onboarding, OnboardingMode::SolutionPartner(_)),
            )
            .field("graph_endpoint", &self.graph_endpoint)
            .field("migrate", &self.migrate)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// Default public bind: loopback (the container image sets `0.0.0.0`).
pub const DEFAULT_PUBLIC_BIND: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
/// Default internal bind: loopback.
pub const DEFAULT_INTERNAL_BIND: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 8081);
/// Default `WA_SERVER_SHUTDOWN_GRACE`.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(25);

/// Reads variables for [`Config::from_env`].
struct Reader<'a> {
    env: &'a dyn Env,
}

impl Reader<'_> {
    /// A plain (non-secret) variable.
    fn plain(&self, name: &'static str) -> Result<Option<String>, ConfigError> {
        self.env
            .var(name)
            .map_err(|NotUnicode| ConfigError::Invalid {
                name,
                reason: "not valid UTF-8",
            })
    }

    /// A secret: `NAME` or the file `NAME_FILE` names.
    fn secret(&self, name: &'static str) -> Result<Option<String>, ConfigError> {
        let file_var = file_var(name);
        let direct = self.plain(name)?;
        let file = self
            .env
            .var(&file_var)
            .map_err(|NotUnicode| ConfigError::Invalid {
                name,
                reason: "the _FILE path is not valid UTF-8",
            })?;
        match (direct, file) {
            (Some(_), Some(_)) => Err(ConfigError::Ambiguous(name)),
            (Some(value), None) => Ok(Some(value)),
            (None, Some(path)) => {
                let contents = self
                    .env
                    .read_file(&path)
                    .map_err(|_| ConfigError::Unreadable(name))?;
                let trimmed = contents
                    .strip_suffix("\r\n")
                    .or_else(|| contents.strip_suffix('\n'))
                    .unwrap_or(&contents);
                Ok(Some(trimmed.to_owned()))
            }
            (None, None) => Ok(None),
        }
    }

    /// A required secret that may not be blank.
    fn required_secret(&self, name: &'static str) -> Result<String, ConfigError> {
        let value = self.secret(name)?.ok_or(ConfigError::Missing(name))?;
        if value.trim().is_empty() {
            return Err(ConfigError::Blank(name));
        }
        Ok(value)
    }

    /// An optional secret that, when set, may not be blank.
    fn optional_secret(&self, name: &'static str) -> Result<Option<String>, ConfigError> {
        match self.secret(name)? {
            Some(value) if value.trim().is_empty() => Err(ConfigError::Blank(name)),
            other => Ok(other),
        }
    }

    fn bind(&self, name: &'static str, default: SocketAddr) -> Result<SocketAddr, ConfigError> {
        match self.plain(name)? {
            None => Ok(default),
            Some(value) => value.parse().map_err(|_| ConfigError::Invalid {
                name,
                reason: "expected an address and port, e.g. 127.0.0.1:8081",
            }),
        }
    }
}

/// The `_FILE` variable of a secret.
fn file_var(name: &'static str) -> String {
    format!("{name}_FILE")
}

/// `<n>`, `<n>s`, `<n>m`, `<n>h` or `<n>d`.
fn duration(name: &'static str, value: &str) -> Result<Duration, ConfigError> {
    let invalid = ConfigError::Invalid {
        name,
        reason: "expected a duration such as 25s, 10m, 24h or 7d",
    };
    let (digits, unit) = match value.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => (&value[..i], c),
        _ => (value, 's'),
    };
    let n: u64 = digits.parse().map_err(|_| invalid.clone())?;
    let seconds = match unit {
        's' => Some(n),
        'm' => n.checked_mul(60),
        'h' => n.checked_mul(3600),
        'd' => n.checked_mul(86_400),
        _ => None,
    };
    seconds.map(Duration::from_secs).ok_or(invalid)
}

impl Config {
    /// Read the process environment.
    pub fn from_process_env() -> Result<Self, ConfigError> {
        Self::from_env(&ProcessEnv)
    }

    /// Read `env`, refusing every unsafe setting (see the module docs).
    pub fn from_env(env: &dyn Env) -> Result<Self, ConfigError> {
        let r = Reader { env };
        if r.plain("WA_SERVER_CONFIG")?.is_some() {
            return Err(ConfigError::ConfigFileNotSupported);
        }
        let environment = match r.plain("WA_SERVER_ENV")?.as_deref() {
            None | Some("production") => Environment::Production,
            Some("development") => Environment::Development,
            Some(_) => {
                return Err(ConfigError::Invalid {
                    name: "WA_SERVER_ENV",
                    reason: "expected production or development",
                });
            }
        };
        let public_bind = r.bind("WA_SERVER_PUBLIC_BIND", DEFAULT_PUBLIC_BIND)?;
        let internal_bind = r.bind("WA_SERVER_INTERNAL_BIND", DEFAULT_INTERNAL_BIND)?;
        if public_bind == internal_bind {
            return Err(ConfigError::SameBinds);
        }
        let storage = match r.secret("DATABASE_URL")? {
            Some(url) if url.trim().is_empty() => return Err(ConfigError::Blank("DATABASE_URL")),
            Some(url) => Storage::Postgres(DatabaseUrl(url)),
            None if environment == Environment::Development => Storage::Memory,
            None => return Err(ConfigError::MemoryOutsideDevelopment),
        };
        let postgres = matches!(storage, Storage::Postgres(_));
        let meta = meta_app(&r)?;
        let (vault_keys, vault_key_is_throwaway) = vault_keys(&r, postgres)?;
        let otp_pepper = match r.optional_secret("WA_OTP_PEPPER")? {
            Some(pepper) => {
                Some(OtpPepper::new(pepper.into_bytes()).map_err(|_| ConfigError::PepperTooShort)?)
            }
            None if postgres => return Err(ConfigError::PepperRequired),
            None => None,
        };
        let onboarding = onboarding(&r)?;
        let graph_endpoint = graph_endpoint(&r, environment)?;
        let runtime = runtime(&r)?;
        let settings = settings(&r)?;
        Ok(Self {
            environment,
            public_bind,
            internal_bind,
            storage,
            app_id: meta.app_id,
            app_secrets: meta.app_secrets,
            verify_token: meta.verify_token,
            es_config_id: meta.es_config_id,
            vault_keys,
            vault_key_is_throwaway,
            otp_pepper,
            onboarding,
            graph_endpoint,
            migrate: runtime.migrate,
            shutdown_grace: runtime.shutdown_grace,
            log_format: runtime.log_format,
            log_filter: runtime.log_filter,
            settings,
        })
    }
}

/// A positive whole number.
fn positive(r: &Reader<'_>, name: &'static str) -> Result<Option<u64>, ConfigError> {
    match r.plain(name)? {
        None => Ok(None),
        Some(value) => value
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or(ConfigError::Invalid {
                name,
                reason: "expected a positive whole number",
            }),
    }
}

/// A class's rate and burst; an unset burst keeps `default`'s proportion.
fn rate(
    r: &Reader<'_>,
    name: &'static str,
    burst_name: &'static str,
    default: Rate,
) -> Result<Rate, ConfigError> {
    let too_large = ConfigError::Invalid {
        name,
        reason: "expected at most 4294967295",
    };
    let per_second = match positive(r, name)? {
        None => default.per_second,
        Some(n) => u32::try_from(n).map_err(|_| too_large.clone())?,
    };
    let burst = match positive(r, burst_name)? {
        Some(n) => u32::try_from(n).map_err(|_| ConfigError::Invalid {
            name: burst_name,
            reason: "expected at most 4294967295",
        })?,
        // The default's proportion.
        None => per_second.saturating_mul(default.burst / default.per_second.max(1)),
    };
    Ok(Rate { per_second, burst })
}

/// `WA_SERVER_IDEMPOTENCY_TTL`, `WA_SERVER_MEDIA_MAX_BYTES`,
/// `WA_SERVER_MEDIA_CONCURRENCY` and the rate limits.
fn settings(r: &Reader<'_>) -> Result<Settings, ConfigError> {
    let defaults = Settings::default();
    let idempotency_ttl = match r.plain("WA_SERVER_IDEMPOTENCY_TTL")? {
        None => defaults.idempotency_ttl,
        Some(value) => {
            let ttl = duration("WA_SERVER_IDEMPOTENCY_TTL", &value)?;
            // A record must outlive its lease, or a running request's key
            // could be claimed again.
            if ttl <= defaults.idempotency_lease {
                return Err(ConfigError::Invalid {
                    name: "WA_SERVER_IDEMPOTENCY_TTL",
                    reason: "expected more than the idempotency lease (1m)",
                });
            }
            ttl
        }
    };
    let media_concurrency = match positive(r, "WA_SERVER_MEDIA_CONCURRENCY")? {
        None => defaults.media_concurrency,
        Some(n) => usize::try_from(n).map_err(|_| ConfigError::Invalid {
            name: "WA_SERVER_MEDIA_CONCURRENCY",
            reason: "expected a smaller number",
        })?,
    };
    let media_streams = match positive(r, "WA_SERVER_MEDIA_STREAMS")? {
        None => defaults.media_streams,
        Some(n) => usize::try_from(n).map_err(|_| ConfigError::Invalid {
            name: "WA_SERVER_MEDIA_STREAMS",
            reason: "expected a smaller number",
        })?,
    };
    let limits = RateLimits::default();
    Ok(Settings {
        rate_limits: RateLimits {
            send: rate(
                r,
                "WA_SERVER_RATE_SEND",
                "WA_SERVER_RATE_SEND_BURST",
                limits.send,
            )?,
            read: rate(
                r,
                "WA_SERVER_RATE_READ",
                "WA_SERVER_RATE_READ_BURST",
                limits.read,
            )?,
            templates: rate(
                r,
                "WA_SERVER_RATE_TEMPLATES",
                "WA_SERVER_RATE_TEMPLATES_BURST",
                limits.templates,
            )?,
        },
        idempotency_ttl,
        media_max_bytes: positive(r, "WA_SERVER_MEDIA_MAX_BYTES")?
            .unwrap_or(defaults.media_max_bytes),
        media_concurrency,
        media_streams,
        ..defaults
    })
}

/// The Meta app's settings.
struct MetaApp {
    app_id: Option<AppId>,
    app_secrets: Vec<AppSecret>,
    verify_token: VerifyToken,
    es_config_id: Option<String>,
}

/// `WA_APP_SECRET` (+ previous) and `WA_VERIFY_TOKEN`, required and not
/// blank; `WA_APP_ID` and `WA_ES_CONFIG_ID` when set.
fn meta_app(r: &Reader<'_>) -> Result<MetaApp, ConfigError> {
    let mut app_secrets = vec![AppSecret::new(r.required_secret("WA_APP_SECRET")?)];
    if let Some(previous) = r.optional_secret("WA_APP_SECRET_PREVIOUS")? {
        app_secrets.push(AppSecret::new(previous));
    }
    let verify_token = VerifyToken::new(r.required_secret("WA_VERIFY_TOKEN")?);
    let app_id = match r.plain("WA_APP_ID")? {
        None => None,
        Some(id) if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
            Some(AppId::new(id))
        }
        Some(_) => {
            return Err(ConfigError::Invalid {
                name: "WA_APP_ID",
                reason: "expected the numeric app id",
            });
        }
    };
    Ok(MetaApp {
        app_id,
        app_secrets,
        verify_token,
        es_config_id: r.plain("WA_ES_CONFIG_ID")?,
    })
}

/// `WA_GRAPH_API_VERSION` and `WA_GRAPH_ENDPOINT`. Every merchant's and
/// system user's token goes to that endpoint: plain `http` only in
/// development (a local stub).
fn graph_endpoint(r: &Reader<'_>, environment: Environment) -> Result<GraphEndpoint, ConfigError> {
    let version = match r.plain("WA_GRAPH_API_VERSION")? {
        None => ApiVersion::DEFAULT,
        Some(v) => v.parse().map_err(|_| ConfigError::Invalid {
            name: "WA_GRAPH_API_VERSION",
            reason: "expected vNN.N, e.g. v25.0",
        })?,
    };
    let Some(url) = r.plain("WA_GRAPH_ENDPOINT")? else {
        return Ok(GraphEndpoint::production(version));
    };
    let endpoint = GraphEndpoint::custom(&url, version).map_err(|_| ConfigError::Invalid {
        name: "WA_GRAPH_ENDPOINT",
        reason: "expected an absolute http(s) URL",
    })?;
    if endpoint.base().scheme() != "https" && environment != Environment::Development {
        return Err(ConfigError::Invalid {
            name: "WA_GRAPH_ENDPOINT",
            reason: "https is required outside WA_SERVER_ENV=development (the tokens travel to it)",
        });
    }
    Ok(endpoint)
}

impl Config {
    /// The host Graph calls go to when it is not Meta's (`WA_GRAPH_ENDPOINT`
    /// set), for the warning `serve` logs at start.
    pub fn graph_endpoint_override(&self) -> Option<String> {
        let base = self.graph_endpoint.base();
        (base.as_str() != GraphEndpoint::PRODUCTION)
            .then(|| base.host_str().unwrap_or_default().to_owned())
    }
}

/// How the process runs.
struct Runtime {
    migrate: MigrateMode,
    shutdown_grace: Duration,
    log_format: LogFormat,
    log_filter: String,
}

fn migrate_mode(r: &Reader<'_>) -> Result<MigrateMode, ConfigError> {
    match r.plain("WA_SERVER_MIGRATE")?.as_deref() {
        None | Some("auto") => Ok(MigrateMode::Auto),
        Some("skip") => Ok(MigrateMode::Skip),
        Some(_) => Err(ConfigError::Invalid {
            name: "WA_SERVER_MIGRATE",
            reason: "expected auto or skip",
        }),
    }
}

/// `WA_SERVER_MIGRATE`, `WA_SERVER_SHUTDOWN_GRACE`, `WA_SERVER_LOG_FORMAT`,
/// `RUST_LOG`.
fn runtime(r: &Reader<'_>) -> Result<Runtime, ConfigError> {
    let shutdown_grace = match r.plain("WA_SERVER_SHUTDOWN_GRACE")? {
        None => DEFAULT_SHUTDOWN_GRACE,
        Some(value) => duration("WA_SERVER_SHUTDOWN_GRACE", &value)?,
    };
    let log_format = match r.plain("WA_SERVER_LOG_FORMAT")?.as_deref() {
        None | Some("json") => LogFormat::Json,
        Some("text") => LogFormat::Text,
        Some(_) => {
            return Err(ConfigError::Invalid {
                name: "WA_SERVER_LOG_FORMAT",
                reason: "expected json or text",
            });
        }
    };
    Ok(Runtime {
        migrate: migrate_mode(r)?,
        shutdown_grace,
        log_format,
        log_filter: r.plain("RUST_LOG")?.unwrap_or_else(|| "info".to_owned()),
    })
}

/// `DATABASE_URL` (or `DATABASE_URL_FILE`) and `WA_SERVER_MIGRATE`, for the
/// commands that only need the database (`migrate`, `admin …`).
pub fn database(env: &dyn Env) -> Result<(DatabaseUrl, MigrateMode), ConfigError> {
    let r = Reader { env };
    let url = r.required_secret("DATABASE_URL")?;
    Ok((DatabaseUrl(url), migrate_mode(&r)?))
}

/// `WA_SERVER_INTERNAL_BIND`, for `healthcheck`.
pub fn internal_bind(env: &dyn Env) -> Result<SocketAddr, ConfigError> {
    Reader { env }.bind("WA_SERVER_INTERNAL_BIND", DEFAULT_INTERNAL_BIND)
}

/// The vault keys: `WA_VAULT_KEY` (+ id, + previous keys), or a throwaway
/// key in development without Postgres.
fn vault_keys(r: &Reader<'_>, postgres: bool) -> Result<(VaultKeys, bool), ConfigError> {
    let id = r
        .plain("WA_VAULT_KEY_ID")?
        .unwrap_or_else(|| "k1".to_owned());
    let invalid_id = ConfigError::Invalid {
        name: "WA_VAULT_KEY_ID",
        reason: "1-64 characters of A-Z a-z 0-9 - _ . :",
    };
    // The library checks the id too; checking it here first tells a bad id
    // from a bad key without quoting either.
    let id_valid = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
    if !id_valid {
        return Err(invalid_id);
    }
    let (active, throwaway) = match r.optional_secret("WA_VAULT_KEY")? {
        Some(encoded) => {
            let key =
                VaultKey::from_base64(id, encoded.trim()).map_err(|_| ConfigError::Invalid {
                    name: "WA_VAULT_KEY",
                    reason: "expected base64 of exactly 32 bytes",
                })?;
            (key, false)
        }
        None if postgres => return Err(ConfigError::VaultKeyRequired),
        None => (VaultKey::generate(id).map_err(|_| invalid_id)?, true),
    };
    // Ids name the key each record was sealed with: one id for two keys
    // would leave the second unreachable, and its records undecryptable.
    let mut ids = vec![active.id().to_owned()];
    let mut keys = VaultKeys::new(active);
    if let Some(previous) = r.optional_secret("WA_VAULT_PREVIOUS_KEYS")? {
        let invalid = ConfigError::Invalid {
            name: "WA_VAULT_PREVIOUS_KEYS",
            reason: "expected <id>:<base64 of 32 bytes>, comma-separated",
        };
        for entry in previous.split(',') {
            let (id, encoded) = entry
                .trim()
                .rsplit_once(':')
                .ok_or_else(|| invalid.clone())?;
            let key = VaultKey::from_base64(id, encoded).map_err(|_| invalid.clone())?;
            if ids.iter().any(|seen| seen == id) {
                return Err(ConfigError::Invalid {
                    name: "WA_VAULT_PREVIOUS_KEYS",
                    reason: "a key id appears twice (WA_VAULT_KEY_ID's included): each key needs its own",
                });
            }
            ids.push(id.to_owned());
            keys = keys.with_previous(key);
        }
    }
    Ok((keys, throwaway))
}

/// `WA_ONBOARDING_MODE` and, in Solution Partner mode, its four settings.
fn onboarding(r: &Reader<'_>) -> Result<OnboardingMode, ConfigError> {
    match r.plain("WA_ONBOARDING_MODE")?.as_deref() {
        None | Some("tech_provider") => Ok(OnboardingMode::TechProvider),
        Some("solution_partner") => {
            let required = |value: Option<String>, name: &'static str| match value {
                Some(v) if !v.trim().is_empty() => Ok(v),
                _ => Err(ConfigError::PartnerSettingMissing(name)),
            };
            let system_token = required(
                r.secret("WA_PARTNER_SYSTEM_TOKEN")?,
                "WA_PARTNER_SYSTEM_TOKEN",
            )?;
            let system_user_id = required(
                r.plain("WA_PARTNER_SYSTEM_USER_ID")?,
                "WA_PARTNER_SYSTEM_USER_ID",
            )?;
            let credit_line_id = required(r.plain("WA_CREDIT_LINE_ID")?, "WA_CREDIT_LINE_ID")?;
            let currency = required(r.plain("WA_WABA_CURRENCY")?, "WA_WABA_CURRENCY")?;
            if !WabaCurrency::SUPPORTED.contains(&currency.as_str()) {
                return Err(ConfigError::Invalid {
                    name: "WA_WABA_CURRENCY",
                    reason: "expected one of AUD, EUR, GBP, IDR, INR, USD",
                });
            }
            let currency = currency.parse().map_err(|_| ConfigError::Invalid {
                name: "WA_WABA_CURRENCY",
                reason: "expected one of AUD, EUR, GBP, IDR, INR, USD",
            })?;
            Ok(OnboardingMode::SolutionPartner(PartnerSettings {
                system_token: AccessToken::new(system_token),
                system_user_id,
                credit_line_id,
                currency,
            }))
        }
        Some(_) => Err(ConfigError::Invalid {
            name: "WA_ONBOARDING_MODE",
            reason: "expected tech_provider or solution_partner",
        }),
    }
}
