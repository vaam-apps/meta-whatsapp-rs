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
//! | an erasure key (`WA_SERVER_ERASURE_KEY`) under 32 bytes | `Invalid` |
//! | a Flow private key (`WA_SERVER_FLOWS_PRIVATE_KEY`) that is not an unencrypted 2048-bit RSA key in PEM | `Invalid` |
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
//! | `WA_SERVER_MEDIA_CONVERTER` | `none` | the converter an upload may ask for (`convert`; roadmap M5c1): `none`, or `ffmpeg` (the library's `FfmpegConverter`, in a build with the `ffmpeg` feature, over an `ffmpeg` the deployment installs); pending the owner's confirmation before the first release |
//! | `WA_SERVER_MEDIA_CONVERTER_PROGRAM` | `ffmpeg` from `PATH` | with `ffmpeg` only: the program to run (an absolute path is safest); pending the owner's confirmation before the first release |
//! | `WA_SERVER_SEND_MESSAGING_ACCOUNT_ID` | `on` | `on`: a send from a number (a message, an inbox reply) names its binding's WABA id as `messaging_account_id`, so Meta bills the binding's Messaging Account whichever ones the token reaches (roadmap M5c5, design D37); `off`: a send names none and Meta resolves the account from the token; pending the owner's confirmation before the first release |
//! | `WA_SERVER_LEGAL_ACTS` | `off` | `on` lets tenants take the legal act their keys allow: the India compliance information declaration (`PUT /v1/numbers/{pn}/compliance-info`; roadmap M5c3, design D192), and the operator the max price agreement's signing (`POST /v1/admin/businesses/{business_id}/max-price-agreement`; roadmap M5h, design D231); off, each is `403 feature_not_available` with nothing asked of Meta (the Official Business Account application and the contact book erasure are not gated by it); pending the owner's confirmation before the first release |
//! | `WA_SERVER_CALL_SIP_CREDENTIALS` | `off` | `on` lets a key holding the scope `call_credentials` read a number's SIP server password (`GET /v1/numbers/{pn}/call-settings/sip-credentials`; roadmap M5e2, design D410); off, it is `403 feature_not_available` with nothing asked of Meta, whichever key asks; pending the owner's confirmation before the first release |
//! | `WA_SERVER_STREAMS_PER_TENANT` | 20 | live event streams (`GET /v1/events/stream`, `GET /v1/numbers/{pn}/events/stream`) one tenant holds open at once, per replica (the next is `429 too_many_streams`) |
//! | `WA_SERVER_STREAMS_PER_REPLICA` | 1000 | live event streams open at once on a replica, every tenant's |
//! | `WA_SERVER_RATE_SEND`, `WA_SERVER_RATE_SEND_BURST` | 20, 40 | per tenant and replica, a second ([`crate::ratelimit`]) |
//! | `WA_SERVER_RATE_READ`, `WA_SERVER_RATE_READ_BURST` | 50, 50 | the same, for reads |
//! | `WA_SERVER_RATE_TEMPLATES`, `WA_SERVER_RATE_TEMPLATES_BURST` | 2, 2 | the same, for template management |
//! | `WA_SERVER_RATE_OTP_ISSUE`, `WA_SERVER_RATE_OTP_ISSUE_BURST` | 5, 5 | the same, for sending OTP codes (roadmap M3c; per recipient, the library's cooldown and issue limit apply too) |
//! | `WA_SERVER_RATE_OTP_VERIFY`, `WA_SERVER_RATE_OTP_VERIFY_BURST` | 20, 20 | the same, for checking OTP codes (the service's default: the design names no limit) |
//! | `WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION` | `graph` | whether an `account_update` webhook that offboards a merchant (`PARTNER_APP_UNINSTALLED` of this app, `ACCOUNT_DELETED`) or revokes their credit line (`PARTNER_REMOVED`) is confirmed with Graph first (`graph`: `debug_token` on the WABA's stored token, which must no longer be valid or no longer granted the WABA) or acted on by its signature alone (`signature`) (roadmap M3a) |
//! | `WA_SIGNUP_EVERY_GRANTED_WABA` | `off` | `on`: a signup that grants several WABAs binds every one to its tenant (each under the ownership rules, a WABA another tenant holds refused); `off`: only the primary WABA (the one the event claims, else the first it names) is bound, and the answer lists the others as `not_bound_waba_ids` (roadmap M3f, decision D200); pending the owner's confirmation before the first release |
//! | `WA_HOSTED_SIGNUP` | `off` | `on`: `PARTNER_ADDED` webhooks are remembered and the operator claims one for a tenant with `POST /v1/admin/signup/hosted`; needs `WA_SYSTEM_TOKEN` (or Solution Partner mode, whose system token it falls back to) (roadmap M3f, decision D201); pending the owner's confirmation before the first release |
//! | `WA_SYSTEM_TOKEN` (secret) | the Solution Partner's `WA_PARTNER_SYSTEM_TOKEN` | the deployment's system user token: fetches a hosted signup's business token and manages the pre-verified pool (`business_management`); pending the owner's confirmation before the first release |
//! | `WA_PARTNER_BUSINESS_ID` | unset: the pool's routes off | the deployment's own business portfolio, whose pool of pre-verified numbers `/v1/admin/pre-verified-numbers` manages (roadmap M3f, decision D204); needs `WA_SYSTEM_TOKEN` or Solution Partner mode; pending the owner's confirmation before the first release |
//! | `WA_SERVER_OTP_DEFAULT_TENANT` | unset | the tenant whose OTP settings a tenant without its own borrows: the platform's number, decision D8's default (roadmap M3c); unset, such a tenant's OTP requests are `409 otp_not_configured` |
//! | `WA_SERVER_REGISTRATION_ATTEMPTS`, `WA_SERVER_REGISTRATION_WINDOW` | 5, `72h` | a number's registrations and deregistrations, per number and fixed window, shared by every replica (roadmap M3d; Meta allows 10 of each in a moving 72 hours and locks the number on the 11th) |
//! | `WA_SERVER_VERIFICATION_ATTEMPTS`, `WA_SERVER_VERIFICATION_WINDOW` | 10, `1h` | a number's verification codes requested and checked, the same way |
//! | `WA_SERVER_FLOWS_PRIVATE_KEY` (secret) | unset: no deployment key | the deployment's Flow private key, an unencrypted 2048-bit RSA key in PEM (roadmap M5d; design D133). Since roadmap M5d2 a tenant sets its own per WABA (`PUT /v1/wabas/{waba_id}/flow-key`), read first; unset, a tenant with none is answered `404`. Its public half is what each number's business public key is set to |
//! | `WA_SERVER_FLOWS_DEPLOYMENT_KEY_FALLBACK` | `allow` | `allow`: a tenant with no Flow key of its own is decrypted with the deployment's (`WA_SERVER_FLOWS_PRIVATE_KEY`), as before tenants could set keys; `deny`: such a tenant's Flow requests are `404`, so that no tenant's traffic is decrypted with a key shared with others (roadmap M5d2, design D371); pending the owner's confirmation before the first release |
//! | `WA_SERVER_INBOX_REPLY_CHECKS` | none listed: every check on every number | the local checks an inbox reply makes, per number: `<phone_number_id>:<checks>` entries, comma-separated, `<checks>` being `all`, `none` or the checks left on joined by `+` (`window`, `trust_handover`, `thread_owner`; `trust_handover` only with `window`): `106540352242922:window+trust_handover` turns the ownership check off on that number (the core's `inbox::ReplyChecksByNumber`; roadmap M2a) |
//! | `WA_SERVER_TOKEN_EXPIRY_LEAD` | `7d`, or `WA_SERVER_OUTBOX_RETENTION` when that is shorter | how long before a stored token's expiry the tenant is told (`token_expiring`, once per token and expiry; roadmap M3e): a positive duration no longer than `WA_SERVER_OUTBOX_RETENTION` (set past it, the start is refused), or `off` |
//!
//! And the inbox's retention, the store's own setting, which housekeeping
//! applies: `WA_SERVER_INBOX_RETENTION`, `keep` (the default: the inbox
//! keeps everything, decision D10) or a positive duration such as `90d`,
//! past which a customer's messages, window events and thread ownership
//! records are purged ([`Config::inbox_retention`]). Which one a deployment
//! sets follows its privacy obligations: the deployer's call.
//!
//! And webhooks out ([`WebhookConfig`], roadmap M2c):
//!
//! | Variable | Default | What |
//! | --- | --- | --- |
//! | `WA_SERVER_DATA_KEY` (secret), `WA_SERVER_DATA_KEY_ID`, `WA_SERVER_DATA_PREVIOUS_KEYS` (secret) | unset, `k1`, none | the key webhook endpoints' signing secrets are sealed under (base64 of 32 bytes), its id (1 to 32 of `[A-Za-z0-9_-]`) and the previous ones (`<id>:<base64>`, comma-separated, each id once), which still open what they sealed. Unset: with memory storage a throwaway key, with Postgres no webhooks out (`409 webhooks_not_configured`, no dispatcher) |
//! | `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS` | none: every destination refused | comma-separated host names (`hooks.example.com`), `*.`-prefixed ones (`*.example.com`) and CIDRs (`10.0.0.0/8`, not `/0`); a private, loopback or link-local address passes only inside a listed CIDR, an unspecified, multicast or metadata address never (`meta_whatsapp_server_core::webhooks_out::destination`); plain `http` destinations only with `WA_SERVER_ENV=development` |
//! | `WA_SERVER_WEBHOOK_RETRY_WINDOW` | `72h` | how long a delivery is retried before it is `failed`; an endpoint failing for the whole window is disabled |
//! | `WA_SERVER_WEBHOOK_IN_FLIGHT`, `…_IN_FLIGHT_PER_TENANT`, `…_IN_FLIGHT_PER_ENDPOINT` | `32`, `8`, `4` | attempts in flight at once on a replica: in all, of one tenant, to one endpoint (positive whole numbers) |
//! | `WA_SERVER_WEBHOOK_LEASE` | `60s` | how long a replica's claim holds a delivery before another may claim it again; at least `30s` (an attempt takes up to 16 s), and replicas' clocks must agree within it (design D107) |
//!
//! And the routing of a moved WABA's events ([`MoveRules`], roadmap S2b,
//! design D36; none applies to a WABA no other tenant held):
//!
//! | Variable | Default | What |
//! | --- | --- | --- |
//! | `WA_SERVER_UNDATED_AFTER_MOVE` | `operator_only` | who gets an undated event (an error, a history chunk, an undated group update) after the WABA moved from another tenant: nobody (`operator_only`, within the window) or the holder (`current_holder`, the behaviour before S2b) |
//! | `WA_SERVER_UNDATED_AFTER_MOVE_WINDOW` | `169h` (7 days and an hour) | how long after the move, with `operator_only` (a positive duration) |
//! | `WA_SERVER_MOVE_SKEW_MARGIN` | `0s` | how much later than the second its binding began an event of a moved WABA must be dated to reach the holder |
//!
//! A rate set without its burst keeps the default's proportion (twice the
//! rate for sends, the rate itself for the others).
//! `WA_SERVER_CONFIG` (the TOML file of non-secrets) is not read yet: set,
//! it is refused (`ConfigFileNotSupported`) rather than ignored.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_rs::client::credit_lines::WabaCurrency;
use meta_whatsapp_rs::client::embedded_signup::{VaultKey, VaultKeys};
use meta_whatsapp_rs::client::flows::endpoint::FlowEndpointKey;
use meta_whatsapp_rs::core::config::{ApiVersion, GraphEndpoint};
use meta_whatsapp_rs::core::ids::{AppId, BusinessId};
use meta_whatsapp_rs::core::media::MediaConverter;
use meta_whatsapp_rs::core::secret::{AccessToken, AppSecret, SecretBytes, VerifyToken};
use meta_whatsapp_rs::core::store::Retention;
use meta_whatsapp_server_core::erasure::ErasureKey;
use meta_whatsapp_server_core::inbox::{InvalidReplyChecks, ReplyChecksByNumber};
use meta_whatsapp_server_core::live::LiveSettings;
use meta_whatsapp_server_core::model::TenantId;
use meta_whatsapp_server_core::otp::OtpKeys;
use meta_whatsapp_server_core::registration::{AttemptLimit, AttemptLimits};
use meta_whatsapp_server_core::signup::AccountUpdateConfirmation;
use meta_whatsapp_server_core::webhooks_out::DEFAULT_RETRY_WINDOW;
use meta_whatsapp_server_core::webhooks_out::destination::DestinationPolicy;
use meta_whatsapp_server_core::webhooks_out::dispatch::DispatchSettings;
use meta_whatsapp_server_core::webhooks_out::seal::{DataKey, DataKeys};

use crate::events::{
    DEFAULT_MOVE_SKEW_MARGIN, DEFAULT_UNDATED_AFTER_MOVE_WINDOW, MoveRules, UndatedAfterMove,
};
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

/// What a signup does beyond the code flow (roadmap M3f). `Debug` shows
/// no token.
#[derive(Debug, Clone, Default)]
pub struct SignupOptions {
    /// `WA_SIGNUP_EVERY_GRANTED_WABA`.
    pub every_granted_waba: bool,
    /// `WA_HOSTED_SIGNUP=on`: the system token that fetches the business
    /// token.
    pub hosted_system_token: Option<AccessToken>,
    /// `WA_PARTNER_BUSINESS_ID` and the system token that manages its pool
    /// of pre-verified numbers.
    pub pre_verified_pool: Option<(BusinessId, AccessToken)>,
}

/// Whether `serve` migrates at start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateMode {
    /// Run the migrations at start (the default).
    Auto,
    /// A job runs `meta-whatsapp-server migrate`.
    Skip,
}

/// Log output: the HTTP crate's, which installs the subscriber
/// (`meta_whatsapp_server_http::telemetry::init`).
pub use meta_whatsapp_server_http::telemetry::LogFormat;

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
    /// `WA_OTP_PEPPER`: the library's OTP pepper and the key of the OTP
    /// issue's idempotency fingerprint, derived from it (roadmap M3c). Unset
    /// only on memory storage, where `serve` uses a throwaway one.
    pub otp_pepper: Option<OtpKeys>,
    /// `WA_SERVER_OTP_DEFAULT_TENANT` (roadmap M3c, decision D8): the
    /// tenant whose OTP settings a tenant without its own borrows.
    pub otp_default_tenant: Option<TenantId>,
    /// `WA_SERVER_ERASURE_KEY` (roadmap M2f): the key of the erasure
    /// journal's HMACs, at least 32 bytes. Unset, erasure is off (`409
    /// erasure_not_configured`), except on memory storage, where `serve`
    /// uses a throwaway one.
    pub erasure_key: Option<ErasureKey>,
    /// `WA_SERVER_FLOWS_PRIVATE_KEY` (roadmap M5d): the deployment's Flow
    /// private key, a 2048-bit RSA key in PEM, unencrypted. Unset, the
    /// Flows data endpoint (`POST /webhooks/flows/{waba_id}`) is `404`.
    /// `Debug` shows its size only.
    pub flows_private_key: Option<FlowEndpointKey>,
    /// `WA_ONBOARDING_MODE` and its settings.
    pub onboarding: OnboardingMode,
    /// `WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION` (roadmap M3a, proposed
    /// decision D72): whether an `account_update` that offboards a merchant
    /// or revokes their credit line is confirmed with Graph first (`graph`,
    /// the default) or acted on by its signature alone (`signature`).
    pub account_update_confirmation: AccountUpdateConfirmation,
    /// `WA_SIGNUP_EVERY_GRANTED_WABA`, `WA_HOSTED_SIGNUP`,
    /// `WA_SYSTEM_TOKEN` and `WA_PARTNER_BUSINESS_ID` (roadmap M3f).
    pub signup_options: SignupOptions,
    /// `WA_GRAPH_API_VERSION` and `WA_GRAPH_ENDPOINT`.
    pub graph_endpoint: GraphEndpoint,
    /// `WA_SERVER_MIGRATE`.
    pub migrate: MigrateMode,
    /// `WA_SERVER_SHUTDOWN_GRACE`.
    pub shutdown_grace: Duration,
    /// `WA_SERVER_OUTBOX_RETENTION`: how long the event outbox keeps an
    /// event (default [`crate::events::DEFAULT_OUTBOX_RETENTION`], 7 days,
    /// until the owner decides retention, the design's decision D10).
    pub outbox_retention: Duration,
    /// `WA_SERVER_TOKEN_EXPIRY_LEAD`: how long before a stored token's
    /// expiry its tenant is told (`token_expiring`; roadmap M3e; default
    /// [`meta_whatsapp_server_core::token_expiry::DEFAULT_LEAD`], 7 days, or
    /// [`Self::outbox_retention`] when that is shorter); `None` (`off`): never.
    /// At most [`Self::outbox_retention`].
    pub token_expiry_lead: Option<Duration>,
    /// `WA_SERVER_INBOX_RETENTION`: how long the inbox keeps a customer's
    /// messages, the store's own setting (`with_retention`), which
    /// housekeeping applies ([`Retention::Keep`] unset, the default of
    /// decision D10; roadmap M2a). The backend must be built with it: the
    /// start refuses one whose inbox applies another.
    pub inbox_retention: Retention,
    /// `WA_SERVER_LOG_FORMAT`.
    pub log_format: LogFormat,
    /// `RUST_LOG`.
    pub log_filter: String,
    /// `WA_SERVER_IDEMPOTENCY_TTL`, `WA_SERVER_MEDIA_*`, `WA_SERVER_RATE_*`.
    pub settings: Settings,
    /// `WA_SERVER_UNDATED_AFTER_MOVE`, `WA_SERVER_UNDATED_AFTER_MOVE_WINDOW`
    /// and `WA_SERVER_MOVE_SKEW_MARGIN`: the routing of a moved WABA's
    /// events (roadmap S2b, design D36; [`MoveRules::default`] unset).
    pub move_rules: MoveRules,
    /// `WA_SERVER_DATA_KEY*`, `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS` and
    /// `WA_SERVER_WEBHOOK_RETRY_WINDOW` (roadmap M2c).
    pub webhooks: WebhookConfig,
}

/// Webhooks out's settings (roadmap M2c). `Debug` shows the data keys'
/// ids, never a key.
#[derive(Debug, Clone)]
pub struct WebhookConfig {
    /// `WA_SERVER_DATA_KEY` (+ id, + previous keys); a throwaway key with
    /// memory storage; `None` with Postgres and no key: no webhooks out.
    pub data_keys: Option<DataKeys>,
    /// Whether the data key was generated for this process.
    pub data_key_is_throwaway: bool,
    /// `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS`, plain `http` allowed in
    /// development only.
    pub destinations: DestinationPolicy,
    /// `WA_SERVER_WEBHOOK_RETRY_WINDOW`.
    pub retry_window: Duration,
    /// `WA_SERVER_WEBHOOK_IN_FLIGHT`, `…_IN_FLIGHT_PER_TENANT`,
    /// `…_IN_FLIGHT_PER_ENDPOINT` and `WA_SERVER_WEBHOOK_LEASE`: the
    /// dispatcher's bounds and lease (design D108).
    pub dispatch: DispatchSettings,
}

/// The shortest `WA_SERVER_WEBHOOK_LEASE`: an attempt takes up to 16 s (a
/// 5 s lookup, the sender's 10 s and the dispatcher's second), and its
/// event read and settlement come on top.
pub const MIN_WEBHOOK_LEASE: Duration = Duration::from_secs(30);

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
            .field("outbox_retention", &self.outbox_retention)
            .field("token_expiry_lead", &self.token_expiry_lead)
            .field("inbox_retention", &self.inbox_retention)
            .field("settings", &self.settings)
            .field("move_rules", &self.move_rules)
            .field("webhooks", &self.webhooks)
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
            Some(pepper) => Some(
                OtpKeys::new(SecretBytes::new(pepper.into_bytes()))
                    .map_err(|_| ConfigError::PepperTooShort)?,
            ),
            None if postgres => return Err(ConfigError::PepperRequired),
            None => None,
        };
        let otp_default_tenant = match r.plain("WA_SERVER_OTP_DEFAULT_TENANT")? {
            None => None,
            Some(id) => Some(TenantId::parse(id.trim()).ok_or(ConfigError::Invalid {
                name: "WA_SERVER_OTP_DEFAULT_TENANT",
                reason: "expected a tenant id: 1 to 64 of [A-Za-z0-9._:-]",
            })?),
        };
        let erasure_key = match r.optional_secret("WA_SERVER_ERASURE_KEY")? {
            None => None,
            Some(key) => Some(ErasureKey::new(SecretBytes::new(key.into_bytes())).map_err(
                |_| ConfigError::Invalid {
                    name: "WA_SERVER_ERASURE_KEY",
                    reason: "expected at least 32 bytes, from your secret manager",
                },
            )?),
        };
        let flows_private_key = flows_private_key(&r)?;
        let onboarding = onboarding(&r)?;
        let account_update_confirmation =
            match r.plain("WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION")?.as_deref() {
                None | Some("graph") => AccountUpdateConfirmation::Graph,
                Some("signature") => AccountUpdateConfirmation::Signature,
                Some(_) => {
                    return Err(ConfigError::Invalid {
                        name: "WA_SERVER_ACCOUNT_UPDATE_CONFIRMATION",
                        reason: "expected graph or signature",
                    });
                }
            };
        let graph_endpoint = graph_endpoint(&r, environment)?;
        let runtime = runtime(&r)?;
        let settings = settings(&r)?;
        let move_rules = move_rules(&r)?;
        let webhooks = webhook_config(&r, postgres, environment)?;
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
            otp_default_tenant,
            erasure_key,
            flows_private_key,
            signup_options: signup_options(&r, &onboarding)?,
            onboarding,
            account_update_confirmation,
            graph_endpoint,
            migrate: runtime.migrate,
            shutdown_grace: runtime.shutdown_grace,
            outbox_retention: runtime.outbox_retention,
            token_expiry_lead: runtime.token_expiry_lead,
            inbox_retention: runtime.inbox_retention,
            log_format: runtime.log_format,
            log_filter: runtime.log_filter,
            settings,
            move_rules,
            webhooks,
        })
    }
}

/// `WA_SERVER_FLOWS_PRIVATE_KEY` (roadmap M5d, design D133): the
/// deployment's Flow private key, parsed at start. The key is never in the
/// error: its reason names the format.
fn flows_private_key(r: &Reader<'_>) -> Result<Option<FlowEndpointKey>, ConfigError> {
    const NAME: &str = "WA_SERVER_FLOWS_PRIVATE_KEY";
    r.optional_secret(NAME)?
        .map(|pem| {
            FlowEndpointKey::from_pem(&pem).map_err(|_| ConfigError::Invalid {
                name: NAME,
                reason: "expected an unencrypted 2048-bit RSA private key in PEM",
            })
        })
        .transpose()
}

/// Webhooks out: the data keys (a throwaway one with memory storage, none
/// with Postgres and no `WA_SERVER_DATA_KEY`), the allow-list (plain
/// `http` in development only), the retry window (positive).
fn webhook_config(
    r: &Reader<'_>,
    postgres: bool,
    environment: Environment,
) -> Result<WebhookConfig, ConfigError> {
    let id = r
        .plain("WA_SERVER_DATA_KEY_ID")?
        .unwrap_or_else(|| "k1".to_owned());
    let invalid_id = ConfigError::Invalid {
        name: "WA_SERVER_DATA_KEY_ID",
        reason: "1-32 characters of A-Z a-z 0-9 - _",
    };
    let (active, throwaway) = match r.optional_secret("WA_SERVER_DATA_KEY")? {
        Some(encoded) => {
            let key = DataKey::from_base64(id, &encoded).map_err(|e| match e {
                meta_whatsapp_server_core::webhooks_out::seal::InvalidDataKey::Id => {
                    invalid_id.clone()
                }
                _ => ConfigError::Invalid {
                    name: "WA_SERVER_DATA_KEY",
                    reason: "expected base64 of exactly 32 bytes",
                },
            })?;
            (Some(key), false)
        }
        None if postgres => (None, false),
        None => (
            Some(DataKey::generate(id).map_err(|_| invalid_id.clone())?),
            true,
        ),
    };
    let data_keys = match active {
        None => None,
        Some(active) => {
            let mut keys = DataKeys::new(active);
            if let Some(previous) = r.optional_secret("WA_SERVER_DATA_PREVIOUS_KEYS")? {
                let invalid = ConfigError::Invalid {
                    name: "WA_SERVER_DATA_PREVIOUS_KEYS",
                    reason: "expected <id>:<base64 of 32 bytes>, comma-separated, each id once (WA_SERVER_DATA_KEY_ID's included)",
                };
                for entry in previous.split(',') {
                    let (id, encoded) = entry
                        .trim()
                        .split_once(':')
                        .ok_or_else(|| invalid.clone())?;
                    let key = DataKey::from_base64(id, encoded).map_err(|_| invalid.clone())?;
                    keys = keys.with_previous(key).map_err(|_| invalid.clone())?;
                }
            }
            Some(keys)
        }
    };
    let destinations = DestinationPolicy::parse(
        &r.plain("WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS")?
            .unwrap_or_default(),
        environment == Environment::Development,
    )
    .map_err(|_| ConfigError::Invalid {
        name: "WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS",
        reason: "expected host names, *.-prefixed host names and CIDRs (not /0), comma-separated",
    })?;
    let retry_window = match r.plain("WA_SERVER_WEBHOOK_RETRY_WINDOW")? {
        None => DEFAULT_RETRY_WINDOW,
        Some(value) => match duration("WA_SERVER_WEBHOOK_RETRY_WINDOW", &value)? {
            zero if zero.is_zero() => {
                return Err(ConfigError::Invalid {
                    name: "WA_SERVER_WEBHOOK_RETRY_WINDOW",
                    reason: "expected a positive duration such as 72h",
                });
            }
            window => window,
        },
    };
    let dispatch = dispatch_settings(r)?;
    Ok(WebhookConfig {
        data_keys,
        data_key_is_throwaway: throwaway,
        destinations,
        retry_window,
        dispatch,
    })
}

/// `WA_SERVER_WEBHOOK_IN_FLIGHT`, `…_IN_FLIGHT_PER_TENANT`,
/// `…_IN_FLIGHT_PER_ENDPOINT` (positive) and `WA_SERVER_WEBHOOK_LEASE`
/// (at least [`MIN_WEBHOOK_LEASE`]): the dispatcher's bounds and lease
/// (design D108), the defaults unset.
fn dispatch_settings(r: &Reader<'_>) -> Result<DispatchSettings, ConfigError> {
    let defaults = DispatchSettings::default();
    let bound = |name: &'static str, default: usize| -> Result<usize, ConfigError> {
        match positive(r, name)? {
            None => Ok(default),
            Some(n) => usize::try_from(n).map_err(|_| ConfigError::Invalid {
                name,
                reason: "expected a smaller number",
            }),
        }
    };
    let lease = match r.plain("WA_SERVER_WEBHOOK_LEASE")? {
        None => defaults.lease,
        Some(value) => match duration("WA_SERVER_WEBHOOK_LEASE", &value)? {
            short if short < MIN_WEBHOOK_LEASE => {
                return Err(ConfigError::Invalid {
                    name: "WA_SERVER_WEBHOOK_LEASE",
                    reason: "expected at least 30s (an attempt takes up to 16 s)",
                });
            }
            lease => lease,
        },
    };
    Ok(DispatchSettings {
        workers: bound("WA_SERVER_WEBHOOK_IN_FLIGHT", defaults.workers)?,
        per_tenant: bound(
            "WA_SERVER_WEBHOOK_IN_FLIGHT_PER_TENANT",
            defaults.per_tenant,
        )?,
        per_endpoint: bound(
            "WA_SERVER_WEBHOOK_IN_FLIGHT_PER_ENDPOINT",
            defaults.per_endpoint,
        )?,
        lease,
        ..defaults
    })
}

/// `WA_SERVER_UNDATED_AFTER_MOVE` (`operator_only`, the default, or
/// `current_holder`), `WA_SERVER_UNDATED_AFTER_MOVE_WINDOW` (a positive
/// duration, `169h` by default: 7 days and an hour; only with
/// `operator_only`, refused with `current_holder` rather than ignored) and
/// `WA_SERVER_MOVE_SKEW_MARGIN` (a duration, `0s` by default).
fn move_rules(r: &Reader<'_>) -> Result<MoveRules, ConfigError> {
    let window = match r.plain("WA_SERVER_UNDATED_AFTER_MOVE_WINDOW")? {
        None => None,
        Some(value) => match duration("WA_SERVER_UNDATED_AFTER_MOVE_WINDOW", &value)? {
            // Zero would be `current_holder` under another name.
            zero if zero.is_zero() => {
                return Err(ConfigError::Invalid {
                    name: "WA_SERVER_UNDATED_AFTER_MOVE_WINDOW",
                    reason: "expected a positive duration such as 169h (or set \
                             WA_SERVER_UNDATED_AFTER_MOVE=current_holder)",
                });
            }
            window => Some(window),
        },
    };
    let undated_after_move = match r.plain("WA_SERVER_UNDATED_AFTER_MOVE")?.as_deref() {
        None | Some("operator_only") => UndatedAfterMove::OperatorOnly {
            window: window.unwrap_or(DEFAULT_UNDATED_AFTER_MOVE_WINDOW),
        },
        Some("current_holder") if window.is_some() => {
            return Err(ConfigError::Invalid {
                name: "WA_SERVER_UNDATED_AFTER_MOVE_WINDOW",
                reason: "a window applies to WA_SERVER_UNDATED_AFTER_MOVE=operator_only only",
            });
        }
        Some("current_holder") => UndatedAfterMove::CurrentHolder,
        Some(_) => {
            return Err(ConfigError::Invalid {
                name: "WA_SERVER_UNDATED_AFTER_MOVE",
                reason: "expected operator_only or current_holder",
            });
        }
    };
    let skew_margin = match r.plain("WA_SERVER_MOVE_SKEW_MARGIN")? {
        None => DEFAULT_MOVE_SKEW_MARGIN,
        Some(value) => duration("WA_SERVER_MOVE_SKEW_MARGIN", &value)?,
    };
    Ok(MoveRules {
        undated_after_move,
        skew_margin,
    })
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

/// A number's attempt budget: `attempts` (a positive whole number, at most
/// 4294967295) and `window` (a positive duration), each `default`'s when
/// unset.
fn attempt_limit(
    r: &Reader<'_>,
    attempts: &'static str,
    window: &'static str,
    default: AttemptLimit,
) -> Result<AttemptLimit, ConfigError> {
    let count = match positive(r, attempts)? {
        None => default.attempts,
        Some(n) => u32::try_from(n).map_err(|_| ConfigError::Invalid {
            name: attempts,
            reason: "expected at most 4294967295",
        })?,
    };
    let window = match r.plain(window)? {
        None => default.window,
        Some(value) => match duration(window, &value)? {
            positive if !positive.is_zero() => positive,
            _ => {
                return Err(ConfigError::Invalid {
                    name: window,
                    reason: "expected a positive duration",
                });
            }
        },
    };
    Ok(AttemptLimit {
        attempts: count,
        window,
    })
}

/// `WA_SERVER_REGISTRATION_*` and `WA_SERVER_VERIFICATION_*` (roadmap
/// M3d).
fn attempt_limits(r: &Reader<'_>) -> Result<AttemptLimits, ConfigError> {
    let defaults = AttemptLimits::default();
    Ok(AttemptLimits {
        registration: attempt_limit(
            r,
            "WA_SERVER_REGISTRATION_ATTEMPTS",
            "WA_SERVER_REGISTRATION_WINDOW",
            defaults.registration,
        )?,
        verification: attempt_limit(
            r,
            "WA_SERVER_VERIFICATION_ATTEMPTS",
            "WA_SERVER_VERIFICATION_WINDOW",
            defaults.verification,
        )?,
    })
}

/// `WA_SERVER_IDEMPOTENCY_TTL`, `WA_SERVER_MEDIA_MAX_BYTES`,
/// `WA_SERVER_MEDIA_CONCURRENCY`, `WA_SERVER_MEDIA_STREAMS`, the live
/// event streams' bounds (`WA_SERVER_STREAMS_PER_TENANT`,
/// `WA_SERVER_STREAMS_PER_REPLICA`), the rate limits, the attempt budgets
/// ([`attempt_limits`]) and
/// `WA_SERVER_INBOX_REPLY_CHECKS` (the core's
/// `inbox::ReplyChecksByNumber::parse`).
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
    let streams = |name: &'static str, default: usize| -> Result<usize, ConfigError> {
        match positive(r, name)? {
            None => Ok(default),
            Some(n) => usize::try_from(n).map_err(|_| ConfigError::Invalid {
                name,
                reason: "expected a smaller number",
            }),
        }
    };
    let live = LiveSettings {
        per_tenant: streams("WA_SERVER_STREAMS_PER_TENANT", defaults.live.per_tenant)?,
        per_replica: streams("WA_SERVER_STREAMS_PER_REPLICA", defaults.live.per_replica)?,
        ..defaults.live
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
            otp_issue: rate(
                r,
                "WA_SERVER_RATE_OTP_ISSUE",
                "WA_SERVER_RATE_OTP_ISSUE_BURST",
                limits.otp_issue,
            )?,
            otp_verify: rate(
                r,
                "WA_SERVER_RATE_OTP_VERIFY",
                "WA_SERVER_RATE_OTP_VERIFY_BURST",
                limits.otp_verify,
            )?,
        },
        idempotency_ttl,
        media_max_bytes: positive(r, "WA_SERVER_MEDIA_MAX_BYTES")?
            .unwrap_or(defaults.media_max_bytes),
        media_concurrency,
        media_streams,
        media_converter: media_converter(r)?,
        legal_acts: legal_acts(r)?,
        flows_deployment_key_fallback: flows_deployment_key_fallback(r)?,
        send_messaging_account_id: send_messaging_account_id(r)?,
        call_sip_credentials: call_sip_credentials(r)?,
        live,
        registration_attempts: attempt_limits(r)?,
        inbox_reply_checks: match r.plain("WA_SERVER_INBOX_REPLY_CHECKS")? {
            None => ReplyChecksByNumber::new(),
            Some(value) => {
                ReplyChecksByNumber::parse(&value).map_err(|InvalidReplyChecks(reason)| {
                    ConfigError::Invalid {
                        name: "WA_SERVER_INBOX_REPLY_CHECKS",
                        reason,
                    }
                })?
            }
        },
        ..defaults
    })
}

/// `WA_SERVER_LEGAL_ACTS` (roadmap M5c3): `off` (the default) or `on`.
/// The legal acts a tenant may ask for (the compliance information
/// declaration, the confirmation of template insights;
/// `meta_whatsapp_server_core::number_settings::is_legal_act`)
/// is taken only when the deployment turns it on.
fn legal_acts(r: &Reader<'_>) -> Result<bool, ConfigError> {
    match r.plain("WA_SERVER_LEGAL_ACTS")?.as_deref().map(str::trim) {
        None | Some("" | "off") => Ok(false),
        Some("on") => Ok(true),
        Some(_) => Err(ConfigError::Invalid {
            name: "WA_SERVER_LEGAL_ACTS",
            reason: "expected `off` or `on`",
        }),
    }
}

/// `WA_SERVER_CALL_SIP_CREDENTIALS` (roadmap M5e2, design D410): `off`
/// (the default) or `on`. On, a key holding the scope `call_credentials`
/// may read a SIP server's password.
fn call_sip_credentials(r: &Reader<'_>) -> Result<bool, ConfigError> {
    const NAME: &str = "WA_SERVER_CALL_SIP_CREDENTIALS";
    match r.plain(NAME)?.as_deref().map(str::trim) {
        None | Some("" | "off") => Ok(false),
        Some("on") => Ok(true),
        Some(_) => Err(ConfigError::Invalid {
            name: NAME,
            reason: "expected `off` or `on`",
        }),
    }
}

/// `WA_SERVER_FLOWS_DEPLOYMENT_KEY_FALLBACK` (roadmap M5d2, design
/// D371): `allow` (the default) or `deny`.
fn flows_deployment_key_fallback(r: &Reader<'_>) -> Result<bool, ConfigError> {
    const NAME: &str = "WA_SERVER_FLOWS_DEPLOYMENT_KEY_FALLBACK";
    match r.plain(NAME)?.as_deref().map(str::trim) {
        None | Some("" | "allow") => Ok(true),
        Some("deny") => Ok(false),
        Some(_) => Err(ConfigError::Invalid {
            name: NAME,
            reason: "expected `allow` or `deny`",
        }),
    }
}

/// `WA_SERVER_SEND_MESSAGING_ACCOUNT_ID` (roadmap M5c5): `on` (the
/// default) or `off`. On, a send names its binding's WABA id as
/// `messaging_account_id`.
fn send_messaging_account_id(r: &Reader<'_>) -> Result<bool, ConfigError> {
    match r
        .plain("WA_SERVER_SEND_MESSAGING_ACCOUNT_ID")?
        .as_deref()
        .map(str::trim)
    {
        None | Some("" | "on") => Ok(true),
        Some("off") => Ok(false),
        Some(_) => Err(ConfigError::Invalid {
            name: "WA_SERVER_SEND_MESSAGING_ACCOUNT_ID",
            reason: "expected `on` or `off`",
        }),
    }
}

/// How long the `ffmpeg` converter may run: under the request deadline
/// (`meta_whatsapp_server_http::api::REQUEST_DEADLINE`, 55 s) with room for
/// the upload that follows, so that a conversion is stopped by its own
/// limit (`502 media_conversion_failed`, retryable) and not by the
/// deadline, which would keep the request's key as a timeout that may have
/// been sent. Pending the owner's confirmation before the first release.
#[cfg(feature = "ffmpeg")]
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(20);

/// `WA_SERVER_MEDIA_CONVERTER` and `WA_SERVER_MEDIA_CONVERTER_PROGRAM`
/// (roadmap M5c1): the converter an upload may ask for, or none (the
/// default: an upload that asks for conversion is refused).
fn media_converter(r: &Reader<'_>) -> Result<Option<Arc<dyn MediaConverter>>, ConfigError> {
    let program = r.plain("WA_SERVER_MEDIA_CONVERTER_PROGRAM")?;
    match r
        .plain("WA_SERVER_MEDIA_CONVERTER")?
        .as_deref()
        .map(str::trim)
    {
        None | Some("" | "none") => {
            if program.is_some() {
                return Err(ConfigError::Invalid {
                    name: "WA_SERVER_MEDIA_CONVERTER_PROGRAM",
                    reason: "needs WA_SERVER_MEDIA_CONVERTER=ffmpeg",
                });
            }
            Ok(None)
        }
        Some("ffmpeg") => ffmpeg(program),
        Some(_) => Err(ConfigError::Invalid {
            name: "WA_SERVER_MEDIA_CONVERTER",
            reason: "expected `none` or `ffmpeg`",
        }),
    }
}

/// The library's `FfmpegConverter`, bounded by [`FFMPEG_TIMEOUT`], over
/// `program` or `ffmpeg` from `PATH`.
#[cfg(feature = "ffmpeg")]
fn ffmpeg(program: Option<String>) -> Result<Option<Arc<dyn MediaConverter>>, ConfigError> {
    use meta_whatsapp_rs::adapters::media::FfmpegConverter;
    let mut converter = FfmpegConverter::new().with_timeout(FFMPEG_TIMEOUT);
    if let Some(program) = program {
        if program.trim().is_empty() {
            return Err(ConfigError::Blank("WA_SERVER_MEDIA_CONVERTER_PROGRAM"));
        }
        converter = converter.with_program(program.trim());
    }
    Ok(Some(Arc::new(converter)))
}

/// A build without the `ffmpeg` feature has no converter to run.
#[cfg(not(feature = "ffmpeg"))]
fn ffmpeg(_program: Option<String>) -> Result<Option<Arc<dyn MediaConverter>>, ConfigError> {
    Err(ConfigError::Invalid {
        name: "WA_SERVER_MEDIA_CONVERTER",
        reason: "this build has no ffmpeg support (the `ffmpeg` feature)",
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
    let app_id = r
        .plain("WA_APP_ID")?
        .map(AppId::new)
        .transpose()
        .map_err(|_| ConfigError::Invalid {
            name: "WA_APP_ID",
            reason: "expected the numeric app id",
        })?;
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
    outbox_retention: Duration,
    token_expiry_lead: Option<Duration>,
    inbox_retention: Retention,
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

/// `WA_SERVER_MIGRATE`, `WA_SERVER_SHUTDOWN_GRACE`,
/// `WA_SERVER_OUTBOX_RETENTION`, `WA_SERVER_INBOX_RETENTION`,
/// `WA_SERVER_LOG_FORMAT`, `RUST_LOG`.
fn runtime(r: &Reader<'_>) -> Result<Runtime, ConfigError> {
    let shutdown_grace = match r.plain("WA_SERVER_SHUTDOWN_GRACE")? {
        None => DEFAULT_SHUTDOWN_GRACE,
        Some(value) => duration("WA_SERVER_SHUTDOWN_GRACE", &value)?,
    };
    let outbox_retention = match r.plain("WA_SERVER_OUTBOX_RETENTION")? {
        None => crate::events::DEFAULT_OUTBOX_RETENTION,
        Some(value) => match duration("WA_SERVER_OUTBOX_RETENTION", &value)? {
            // Zero would purge every event at the next housekeeping.
            zero if zero.is_zero() => {
                return Err(ConfigError::Invalid {
                    name: "WA_SERVER_OUTBOX_RETENTION",
                    reason: "expected a positive duration such as 7d",
                });
            }
            retention => retention,
        },
    };
    // `off`, or a positive duration no longer than the outbox's retention:
    // the event's outbox key must outlive the token's lapse, or a purge
    // would tell a token still due again (roadmap M3e). Unset, the default
    // lead yields to a shorter retention rather than refuse the start: a
    // deployment that set `WA_SERVER_OUTBOX_RETENTION` below a week before
    // this setting existed must keep starting; one that sets the lead
    // itself is held to the bound.
    let token_expiry_lead = match r.plain("WA_SERVER_TOKEN_EXPIRY_LEAD")?.as_deref() {
        None => Some(meta_whatsapp_server_core::token_expiry::DEFAULT_LEAD.min(outbox_retention)),
        Some("off") => None,
        Some(value) => match duration("WA_SERVER_TOKEN_EXPIRY_LEAD", value) {
            Ok(lead) if !lead.is_zero() => Some(lead),
            _ => {
                return Err(ConfigError::Invalid {
                    name: "WA_SERVER_TOKEN_EXPIRY_LEAD",
                    reason: "expected off, or a positive duration such as 7d",
                });
            }
        },
    };
    if let Some(lead) = token_expiry_lead
        && !meta_whatsapp_server_core::token_expiry::lead_fits_retention(lead, outbox_retention)
    {
        return Err(ConfigError::Invalid {
            name: "WA_SERVER_TOKEN_EXPIRY_LEAD",
            reason: "expected at most WA_SERVER_OUTBOX_RETENTION (or off)",
        });
    }
    // `keep` (the default), or a positive duration: zero would purge the
    // whole inbox at the next housekeeping.
    let inbox_retention = match r.plain("WA_SERVER_INBOX_RETENTION")?.as_deref() {
        None | Some("keep") => Retention::Keep,
        Some(value) => match duration("WA_SERVER_INBOX_RETENTION", value) {
            Ok(retention) if !retention.is_zero() => Retention::For(retention),
            _ => {
                return Err(ConfigError::Invalid {
                    name: "WA_SERVER_INBOX_RETENTION",
                    reason: "expected keep, or a positive duration such as 7d",
                });
            }
        },
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
        outbox_retention,
        token_expiry_lead,
        inbox_retention,
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

/// `on` or `off` (unset: `off`).
fn switch(r: &Reader<'_>, name: &'static str) -> Result<bool, ConfigError> {
    match r.plain(name)?.as_deref() {
        None | Some("off") => Ok(false),
        Some("on") => Ok(true),
        Some(_) => Err(ConfigError::Invalid {
            name,
            reason: "expected on or off",
        }),
    }
}

/// `WA_SIGNUP_EVERY_GRANTED_WABA`, `WA_HOSTED_SIGNUP`, `WA_SYSTEM_TOKEN`
/// and `WA_PARTNER_BUSINESS_ID` (roadmap M3f): the system token is
/// `WA_SYSTEM_TOKEN`, else a Solution Partner's own.
fn signup_options(
    r: &Reader<'_>,
    onboarding: &OnboardingMode,
) -> Result<SignupOptions, ConfigError> {
    let every_granted_waba = switch(r, "WA_SIGNUP_EVERY_GRANTED_WABA")?;
    let hosted = switch(r, "WA_HOSTED_SIGNUP")?;
    let system_token = match r.secret("WA_SYSTEM_TOKEN")? {
        Some(token) if token.trim().is_empty() => {
            return Err(ConfigError::Blank("WA_SYSTEM_TOKEN"));
        }
        Some(token) => Some(AccessToken::new(token)),
        None => match onboarding {
            OnboardingMode::SolutionPartner(partner) => Some(partner.system_token.clone()),
            OnboardingMode::TechProvider => None,
        },
    };
    let hosted_system_token = if hosted {
        Some(system_token.clone().ok_or(ConfigError::Invalid {
            name: "WA_HOSTED_SIGNUP",
            reason: "needs WA_SYSTEM_TOKEN (or Solution Partner mode)",
        })?)
    } else {
        None
    };
    let pre_verified_pool = match r.plain("WA_PARTNER_BUSINESS_ID")? {
        None => None,
        Some(id) => {
            let business = BusinessId::new(id.trim()).map_err(|_| ConfigError::Invalid {
                name: "WA_PARTNER_BUSINESS_ID",
                reason: "expected a business portfolio id (digits)",
            })?;
            let token = system_token.ok_or(ConfigError::Invalid {
                name: "WA_PARTNER_BUSINESS_ID",
                reason: "needs WA_SYSTEM_TOKEN (or Solution Partner mode)",
            })?;
            Some((business, token))
        }
    };
    Ok(SignupOptions {
        every_granted_waba,
        hosted_system_token,
        pre_verified_pool,
    })
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
