//! Acceptance test M1.6, first half: one test per start-up refusal of
//! docs/design/server.md, section 2.2, each against an otherwise valid
//! configuration (so the refusal is the one named).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::collections::HashMap;

use meta_whatsapp_server::config::{
    Config, ConfigError, Env, Environment, LogFormat, MigrateMode, NotUnicode, OnboardingMode,
    Storage,
};

/// Variables and files, instead of the process's.
#[derive(Default, Clone)]
struct Vars {
    vars: HashMap<String, String>,
    files: HashMap<String, String>,
}

impl Env for Vars {
    fn var(&self, name: &str) -> Result<Option<String>, NotUnicode> {
        Ok(self.vars.get(name).cloned())
    }
    fn read_file(&self, path: &str) -> std::io::Result<String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
    }
}

impl Vars {
    fn set(mut self, name: &str, value: &str) -> Self {
        self.vars.insert(name.to_owned(), value.to_owned());
        self
    }
    fn unset(mut self, name: &str) -> Self {
        self.vars.remove(name);
        self
    }
    fn file(mut self, path: &str, contents: &str) -> Self {
        self.files.insert(path.to_owned(), contents.to_owned());
        self
    }
}

const VAULT_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="; // 32 bytes
const PEPPER: &str = "a-pepper-of-at-least-thirty-two-bytes";

/// A valid production configuration on Postgres.
fn production() -> Vars {
    Vars::default()
        .set("DATABASE_URL", "postgres://wa:secret@127.0.0.1:5432/wa")
        .set("WA_APP_SECRET", "app-secret")
        .set("WA_VERIFY_TOKEN", "verify-token")
        .set("WA_VAULT_KEY", VAULT_KEY)
        .set("WA_OTP_PEPPER", PEPPER)
}

/// A valid development configuration on memory.
fn development() -> Vars {
    Vars::default()
        .set("WA_SERVER_ENV", "development")
        .set("WA_APP_SECRET", "app-secret")
        .set("WA_VERIFY_TOKEN", "verify-token")
}

fn refused(vars: &Vars) -> ConfigError {
    match Config::from_env(vars) {
        Ok(_) => panic!("started"),
        Err(error) => error,
    }
}

#[test]
fn a_valid_production_configuration_starts_with_safe_defaults() {
    let config = Config::from_env(&production()).unwrap();
    assert_eq!(config.environment, Environment::Production);
    assert!(matches!(config.storage, Storage::Postgres(_)));
    assert_eq!(config.public_bind.to_string(), "127.0.0.1:8080");
    assert_eq!(config.internal_bind.to_string(), "127.0.0.1:8081");
    assert_eq!(config.migrate, MigrateMode::Auto);
    assert_eq!(config.log_format, LogFormat::Json);
    assert_eq!(config.shutdown_grace.as_secs(), 25);
    // Retention is the owner's decision D10, still open: the design's
    // proposal until then.
    assert_eq!(config.outbox_retention.as_secs(), 7 * 24 * 3600);
    assert_eq!(config.app_secrets.len(), 1);
    assert_eq!(config.graph_endpoint.version().to_string(), "v25.0");
    assert!(matches!(config.onboarding, OnboardingMode::TechProvider));
    assert!(!config.vault_key_is_throwaway);
    // Debug prints no secret.
    let debug = format!("{config:?}");
    for secret in ["secret@", "app-secret", "verify-token", VAULT_KEY, PEPPER] {
        assert!(!debug.contains(secret), "{debug}");
    }
}

#[test]
fn development_may_run_on_memory_with_a_throwaway_vault_key() {
    let config = Config::from_env(&development()).unwrap();
    assert_eq!(config.storage, Storage::Memory);
    assert!(config.vault_key_is_throwaway);
    assert!(config.otp_pepper.is_none());
}

// ─── The refusals of section 2.2, one test each ──────────────────────────

#[test]
fn refuses_a_missing_app_secret() {
    assert_eq!(
        refused(&production().unset("WA_APP_SECRET")),
        ConfigError::Missing("WA_APP_SECRET")
    );
}

#[test]
fn refuses_a_blank_app_secret() {
    for blank in ["", "  ", "\n"] {
        assert_eq!(
            refused(&production().set("WA_APP_SECRET", blank)),
            ConfigError::Blank("WA_APP_SECRET"),
            "{blank:?}"
        );
    }
    // A blank previous secret too: it would accept signatures made with "".
    assert_eq!(
        refused(&production().set("WA_APP_SECRET_PREVIOUS", " ")),
        ConfigError::Blank("WA_APP_SECRET_PREVIOUS")
    );
}

#[test]
fn refuses_a_missing_verify_token() {
    assert_eq!(
        refused(&production().unset("WA_VERIFY_TOKEN")),
        ConfigError::Missing("WA_VERIFY_TOKEN")
    );
}

#[test]
fn refuses_a_blank_verify_token() {
    assert_eq!(
        refused(&production().set("WA_VERIFY_TOKEN", " \t")),
        ConfigError::Blank("WA_VERIFY_TOKEN")
    );
    // In development too: the check is not about the environment.
    assert_eq!(
        refused(&development().set("WA_VERIFY_TOKEN", "")),
        ConfigError::Blank("WA_VERIFY_TOKEN")
    );
}

#[test]
fn refuses_a_missing_vault_key_with_postgres() {
    assert_eq!(
        refused(&production().unset("WA_VAULT_KEY")),
        ConfigError::VaultKeyRequired
    );
    // Development on Postgres is still Postgres.
    let dev_on_postgres = development()
        .set("DATABASE_URL", "postgres://127.0.0.1/wa")
        .set("WA_OTP_PEPPER", PEPPER);
    assert_eq!(refused(&dev_on_postgres), ConfigError::VaultKeyRequired);
}

#[test]
fn refuses_a_pepper_under_32_bytes_with_postgres() {
    assert_eq!(
        refused(&production().unset("WA_OTP_PEPPER")),
        ConfigError::PepperRequired
    );
    assert_eq!(
        refused(&production().set("WA_OTP_PEPPER", &"p".repeat(31))),
        ConfigError::PepperTooShort
    );
    assert!(Config::from_env(&production().set("WA_OTP_PEPPER", &"p".repeat(32))).is_ok());
}

#[test]
fn refuses_memory_storage_outside_development() {
    assert_eq!(
        refused(&production().unset("DATABASE_URL")),
        ConfigError::MemoryOutsideDevelopment
    );
    assert_eq!(
        refused(&development().set("WA_SERVER_ENV", "production")),
        ConfigError::MemoryOutsideDevelopment
    );
}

#[test]
fn refuses_identical_binds() {
    assert_eq!(
        refused(
            &production()
                .set("WA_SERVER_PUBLIC_BIND", "0.0.0.0:9000")
                .set("WA_SERVER_INTERNAL_BIND", "0.0.0.0:9000")
        ),
        ConfigError::SameBinds
    );
    // The defaults collide too when only one is moved onto the other.
    assert_eq!(
        refused(&production().set("WA_SERVER_PUBLIC_BIND", "127.0.0.1:8081")),
        ConfigError::SameBinds
    );
}

#[test]
fn refuses_solution_partner_mode_without_its_credentials() {
    let partner = production()
        .set("WA_ONBOARDING_MODE", "solution_partner")
        .set("WA_PARTNER_SYSTEM_TOKEN", "system-token")
        .set("WA_PARTNER_SYSTEM_USER_ID", "123456")
        .set("WA_CREDIT_LINE_ID", "789")
        .set("WA_WABA_CURRENCY", "EUR");
    assert!(matches!(
        Config::from_env(&partner).unwrap().onboarding,
        OnboardingMode::SolutionPartner(_)
    ));
    for missing in [
        "WA_PARTNER_SYSTEM_TOKEN",
        "WA_PARTNER_SYSTEM_USER_ID",
        "WA_CREDIT_LINE_ID",
        "WA_WABA_CURRENCY",
    ] {
        assert_eq!(
            refused(&partner.clone().unset(missing)),
            ConfigError::PartnerSettingMissing(missing),
            "{missing} unset"
        );
        assert_eq!(
            refused(&partner.clone().set(missing, " ")),
            ConfigError::PartnerSettingMissing(missing),
            "{missing} blank"
        );
    }
    assert!(matches!(
        refused(&partner.set("WA_WABA_CURRENCY", "XYZ")),
        ConfigError::Invalid {
            name: "WA_WABA_CURRENCY",
            ..
        }
    ));
}

// ─── Secrets from files, and what cannot be parsed ───────────────────────

#[test]
fn secrets_come_from_a_variable_or_a_file_never_both() {
    let from_file = production()
        .unset("WA_APP_SECRET")
        .set("WA_APP_SECRET_FILE", "/run/secrets/app")
        .file("/run/secrets/app", "from-a-file\n");
    let config = Config::from_env(&from_file).unwrap();
    assert_eq!(config.app_secrets[0].expose_secret(), "from-a-file");
    assert_eq!(
        refused(&from_file.clone().set("WA_APP_SECRET", "too")),
        ConfigError::Ambiguous("WA_APP_SECRET")
    );
    assert_eq!(
        refused(&from_file.clone().set("WA_APP_SECRET_FILE", "/nowhere")),
        ConfigError::Unreadable("WA_APP_SECRET")
    );
    // A file holding only a newline is blank.
    assert_eq!(
        refused(&from_file.file("/run/secrets/app", "\n")),
        ConfigError::Blank("WA_APP_SECRET")
    );
}

#[test]
fn refuses_what_it_cannot_parse_without_quoting_it() {
    for (name, value) in [
        ("WA_SERVER_ENV", "staging"),
        ("WA_SERVER_PUBLIC_BIND", "localhost"),
        ("WA_VAULT_KEY", "c2hvcnQ="),
        ("WA_VAULT_KEY_ID", "bad id"),
        ("WA_VAULT_PREVIOUS_KEYS", "no-colon"),
        ("WA_GRAPH_API_VERSION", "latest"),
        ("WA_GRAPH_ENDPOINT", "ftp://graph.example"),
        ("WA_SERVER_MIGRATE", "sometimes"),
        ("WA_SERVER_SHUTDOWN_GRACE", "soon"),
        ("WA_SERVER_OUTBOX_RETENTION", "a week"),
        // Zero would purge every event at the next housekeeping round.
        ("WA_SERVER_OUTBOX_RETENTION", "0"),
        ("WA_SERVER_OUTBOX_RETENTION", "0d"),
        ("WA_SERVER_LOG_FORMAT", "xml"),
        ("WA_ONBOARDING_MODE", "reseller"),
        ("WA_APP_ID", "not-digits"),
    ] {
        let error = refused(&production().set(name, value));
        assert!(
            matches!(error, ConfigError::Invalid { name: n, .. } if n == name),
            "{name}={value}: {error:?}"
        );
        assert!(
            !error.to_string().contains(value),
            "{error} quotes the value"
        );
    }
    assert_eq!(
        refused(&production().set("WA_SERVER_CONFIG", "/etc/wa.toml")),
        ConfigError::ConfigFileNotSupported
    );
}

#[test]
fn previous_vault_keys_and_other_settings_parse() {
    let config = Config::from_env(
        &production()
            .set("WA_VAULT_KEY_ID", "k2")
            .set(
                "WA_VAULT_PREVIOUS_KEYS",
                &format!("k1:{VAULT_KEY}, k0:{VAULT_KEY}"),
            )
            .set("WA_GRAPH_API_VERSION", "v24.0")
            .set(
                "WA_GRAPH_ENDPOINT",
                "https://graph-proxy.internal:9999/graph",
            )
            .set("WA_SERVER_SHUTDOWN_GRACE", "2m")
            .set("WA_SERVER_OUTBOX_RETENTION", "30d")
            .set("WA_APP_SECRET_PREVIOUS", "the-previous-app-secret")
            .set("WA_SERVER_MIGRATE", "skip")
            .set("WA_SERVER_LOG_FORMAT", "text")
            .set("WA_APP_ID", "1234567890"),
    )
    .unwrap();
    assert_eq!(config.vault_keys.active_id(), "k2");
    assert_eq!(config.graph_endpoint.version().to_string(), "v24.0");
    assert_eq!(
        config.graph_endpoint.base().as_str(),
        "https://graph-proxy.internal:9999/graph/"
    );
    assert_eq!(
        config.graph_endpoint_override().as_deref(),
        Some("graph-proxy.internal")
    );
    assert_eq!(config.shutdown_grace.as_secs(), 120);
    assert_eq!(config.outbox_retention.as_secs(), 30 * 24 * 3600);
    // Deliveries signed with either app secret verify while rotating.
    assert_eq!(config.app_secrets.len(), 2);
    assert_eq!(config.migrate, MigrateMode::Skip);
    assert_eq!(config.log_format, LogFormat::Text);
    assert_eq!(config.app_id.unwrap().as_str(), "1234567890");
}

/// Every merchant's and system user's token travels to the Graph endpoint:
/// plain `http` only for a local stub in development (security review L1).
#[test]
fn refuses_a_plain_http_graph_endpoint_outside_development() {
    let stub = "http://127.0.0.1:9999/graph";
    let error = refused(&production().set("WA_GRAPH_ENDPOINT", stub));
    assert!(
        matches!(
            error,
            ConfigError::Invalid {
                name: "WA_GRAPH_ENDPOINT",
                ..
            }
        ),
        "{error:?}"
    );
    assert!(!error.to_string().contains("127.0.0.1"), "{error}");
    let dev = Config::from_env(&development().set("WA_GRAPH_ENDPOINT", stub)).unwrap();
    assert_eq!(dev.graph_endpoint_override().as_deref(), Some("127.0.0.1"));
    // The default endpoint is Meta's, and no override is reported.
    let default = Config::from_env(&production()).unwrap();
    assert_eq!(default.graph_endpoint_override(), None);
}

/// Each vault key needs its own id: records name the key that sealed them
/// (security review L2).
#[test]
fn refuses_a_vault_key_id_used_twice() {
    for (active, previous) in [
        (None, format!("k1:{VAULT_KEY}")),
        (Some("k2"), format!("k1:{VAULT_KEY},k2:{VAULT_KEY}")),
        (Some("k3"), format!("k1:{VAULT_KEY}, k1:{VAULT_KEY}")),
    ] {
        let mut vars = production().set("WA_VAULT_PREVIOUS_KEYS", &previous);
        if let Some(id) = active {
            vars = vars.set("WA_VAULT_KEY_ID", id);
        }
        let error = refused(&vars);
        assert!(
            matches!(
                error,
                ConfigError::Invalid {
                    name: "WA_VAULT_PREVIOUS_KEYS",
                    ..
                }
            ),
            "{active:?} + {previous}: {error:?}"
        );
        assert!(!error.to_string().contains(VAULT_KEY));
    }
}
