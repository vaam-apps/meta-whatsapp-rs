//! The command line (docs/design/server.md, section 7.3).
//!
//! ```text
//! meta-whatsapp-server serve                  run both listeners
//! meta-whatsapp-server migrate                create or upgrade the tables, then exit
//! meta-whatsapp-server openapi                print the OpenAPI document
//! meta-whatsapp-server healthcheck            GET /livez on the internal listener (container HEALTHCHECK)
//! meta-whatsapp-server admin create-admin-key [--name N] [--expires-at RFC3339]
//! meta-whatsapp-server admin create-platform-key --tenants '*'|a,b --scopes numbers,… [--name N] [--expires-at RFC3339]
//! meta-whatsapp-server admin list-keys [--tenant ID]
//! meta-whatsapp-server admin revoke-key <key_id>
//! ```
//!
//! The admin commands need only `DATABASE_URL` (they migrate first unless
//! `WA_SERVER_MIGRATE=skip`) and print the new key once, alone on
//! standard output.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::api::admin::{TenantsSpec, allowed_tenants, mint};
use crate::api::common::rfc3339;
use crate::config::{self, Config, Env, MigrateMode, ProcessEnv};
use crate::model::{
    AllowedTenants, ApiKeyRecord, KeyOwner, KeyScope, MAX_NAME_CHARS, MAX_PAGE_SIZE, PageRequest,
    Scope, TenantId,
};
use crate::store::{PgStore, Store, migrate};
use crate::{api, serve, telemetry};

/// The meta-whatsapp-rs HTTP service.
#[derive(Debug, Parser)]
#[command(name = "meta-whatsapp-server", version, about)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// A command.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Validate the configuration, migrate (unless `WA_SERVER_MIGRATE=skip`)
    /// and serve both listeners until `SIGTERM`.
    Serve,
    /// Create or upgrade the library's and the service's tables, then exit.
    Migrate,
    /// Print the OpenAPI document of the internal listener.
    Openapi,
    /// Exit 0 when `GET /livez` on the internal listener answers 200.
    Healthcheck,
    /// Keys, from the database directly (the first admin key).
    #[command(subcommand)]
    Admin(AdminCommand),
}

/// An admin command.
#[derive(Debug, Subcommand)]
pub enum AdminCommand {
    /// Mint an admin key (for `/v1/admin`) and print it once.
    CreateAdminKey {
        /// A label for operators.
        #[arg(long, default_value = "")]
        name: String,
        /// When it stops working (RFC 3339, in the future); never, if unset.
        #[arg(long)]
        expires_at: Option<String>,
    },
    /// Mint a platform key and print it once.
    CreatePlatformKey {
        /// `*` for any tenant, or comma-separated tenant ids.
        #[arg(long)]
        tenants: String,
        /// Comma-separated scopes (`numbers`, `send`, …).
        #[arg(long)]
        scopes: String,
        /// A label for operators.
        #[arg(long, default_value = "")]
        name: String,
        /// When it stops working (RFC 3339, in the future); never, if unset.
        #[arg(long)]
        expires_at: Option<String>,
    },
    /// List the admin and platform keys (or one tenant's), revoked ones
    /// included, one per line: never a secret.
    ListKeys {
        /// A tenant's keys instead.
        #[arg(long)]
        tenant: Option<String>,
    },
    /// Revoke a key of any kind.
    RevokeKey {
        /// The key id (the part between `wak_` and the secret).
        key_id: String,
    },
}

/// Run `cli`.
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Serve => {
            let config = Config::from_process_env().context("refusing to start")?;
            telemetry::init(config.log_format, &config.log_filter);
            serve::serve(config).await
        }
        Command::Migrate => {
            init_cli_logs();
            let (url, _) = config::database(&ProcessEnv).context("refusing to migrate")?;
            let pool = serve::connect(&url).await?;
            migrate(&pool).await.context("migrations failed")?;
            eprintln!("migrations applied");
            Ok(())
        }
        Command::Openapi => {
            print!("{}", api::openapi_document());
            Ok(())
        }
        Command::Healthcheck => healthcheck(&ProcessEnv).await,
        Command::Admin(command) => {
            init_cli_logs();
            admin(command, &ProcessEnv).await
        }
    }
}

fn init_cli_logs() {
    telemetry::init(config::LogFormat::Text, "warn");
}

async fn admin_store(env: &dyn Env) -> anyhow::Result<PgStore> {
    let (url, mode) = config::database(env).context("the admin commands need DATABASE_URL")?;
    let pool = serve::connect(&url).await?;
    if mode == MigrateMode::Auto {
        migrate(&pool).await.context("migrations failed")?;
    }
    Ok(PgStore::new(pool))
}

async fn admin(command: AdminCommand, env: &dyn Env) -> anyhow::Result<()> {
    let store = admin_store(env).await?;
    match command {
        AdminCommand::CreateAdminKey { name, expires_at } => {
            check_name(&name)?;
            let expires_at = expiry(expires_at.as_deref())?;
            let (minted, record) =
                mint(&store, KeyOwner::Admin, Vec::new(), name, expires_at).await?;
            eprintln!("admin key {} created; it is shown once:", record.key_id);
            println!("{}", minted.expose_key());
        }
        AdminCommand::CreatePlatformKey {
            tenants,
            scopes,
            name,
            expires_at,
        } => {
            check_name(&name)?;
            let expires_at = expiry(expires_at.as_deref())?;
            let spec = if tenants.trim() == "*" {
                TenantsSpec::All("*".to_owned())
            } else {
                TenantsSpec::Only(tenants.split(',').map(|t| t.trim().to_owned()).collect())
            };
            let allowed = allowed_tenants(spec)
                .map_err(|_| anyhow::anyhow!("--tenants: `*` or comma-separated tenant ids"))?;
            let scopes = parse_scopes(&scopes)?;
            let (minted, record) = mint(
                &store,
                KeyOwner::Platform(allowed),
                scopes,
                name,
                expires_at,
            )
            .await?;
            eprintln!("platform key {} created; it is shown once:", record.key_id);
            println!("{}", minted.expose_key());
        }
        AdminCommand::ListKeys { tenant } => {
            let scopes = match tenant {
                Some(tenant) => vec![KeyScope::Tenant(
                    TenantId::parse(&tenant).context("--tenant: not a tenant id")?,
                )],
                None => vec![KeyScope::Admin, KeyScope::Platform],
            };
            println!(
                "key_id\tkind\ttenants\tscopes\tname\tcreated_at\texpires_at\trevoked_at\tlast_used_at"
            );
            for scope in &scopes {
                let mut page = PageRequest {
                    after: None,
                    limit: MAX_PAGE_SIZE,
                };
                loop {
                    let listing = store.keys(scope, &page).await?;
                    for key in &listing.items {
                        println!("{}", key_line(key));
                    }
                    let Some(after) = listing.next_after else {
                        break;
                    };
                    page.after = Some(after);
                }
            }
        }
        AdminCommand::RevokeKey { key_id } => {
            let record = store
                .key(&key_id)
                .await?
                .with_context(|| format!("no key {key_id}"))?;
            let scope = match record.owner {
                KeyOwner::Tenant(tenant) => KeyScope::Tenant(tenant),
                KeyOwner::Platform(_) => KeyScope::Platform,
                KeyOwner::Admin => KeyScope::Admin,
            };
            store.revoke_key(&scope, &key_id).await?;
            eprintln!("key {key_id} revoked");
        }
    }
    Ok(())
}

/// `--expires-at`: RFC 3339, in the future.
fn expiry(value: Option<&str>) -> anyhow::Result<Option<OffsetDateTime>> {
    let Some(value) = value else { return Ok(None) };
    let at = OffsetDateTime::parse(value, &Rfc3339)
        .context("--expires-at: an RFC 3339 time, e.g. 2027-01-01T00:00:00Z")?;
    anyhow::ensure!(at > OffsetDateTime::now_utc(), "--expires-at: in the past");
    Ok(Some(at))
}

/// A key as one tab-separated line: its public id and record, never a
/// secret.
fn key_line(key: &ApiKeyRecord) -> String {
    let time = |at: Option<OffsetDateTime>| at.map_or_else(|| "-".to_owned(), rfc3339);
    let tenants = match &key.owner {
        KeyOwner::Tenant(tenant) => tenant.as_str().to_owned(),
        KeyOwner::Platform(AllowedTenants::All) => "*".to_owned(),
        KeyOwner::Platform(AllowedTenants::Only(list)) => list
            .iter()
            .map(TenantId::as_str)
            .collect::<Vec<_>>()
            .join(","),
        KeyOwner::Admin => "-".to_owned(),
    };
    let scopes = key
        .scopes
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(",");
    // Names are operators' labels: tabs and newlines would break the line.
    let name: String = key.name.chars().filter(|c| !c.is_control()).collect();
    [
        key.key_id.clone(),
        key.owner.kind().as_str().to_owned(),
        tenants,
        if scopes.is_empty() {
            "-".to_owned()
        } else {
            scopes
        },
        if name.is_empty() {
            "-".to_owned()
        } else {
            name
        },
        rfc3339(key.created_at),
        time(key.expires_at),
        time(key.revoked_at),
        time(key.last_used_at),
    ]
    .join("\t")
}

fn check_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        name.chars().count() <= MAX_NAME_CHARS && !name.chars().any(char::is_control),
        "--name: at most {MAX_NAME_CHARS} characters, no control characters"
    );
    Ok(())
}

fn parse_scopes(list: &str) -> anyhow::Result<Vec<Scope>> {
    let mut scopes = list
        .split(',')
        .map(|s| {
            Scope::parse(s.trim()).with_context(|| {
                format!(
                    "--scopes: unknown scope `{}` (one of {})",
                    s.trim(),
                    Scope::ALL.map(Scope::as_str).join(", ")
                )
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    scopes.sort();
    scopes.dedup();
    anyhow::ensure!(!scopes.is_empty(), "--scopes: at least one scope");
    Ok(scopes)
}

/// `GET /livez` on the internal listener, over plain TCP (the image has no
/// curl). An unspecified bind address is reached on loopback.
async fn healthcheck(env: &dyn Env) -> anyhow::Result<()> {
    let bind = config::internal_bind(env)?;
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    let target = SocketAddr::new(ip, bind.port());
    let check = async {
        let mut stream = tokio::net::TcpStream::connect(target).await?;
        stream
            .write_all(b"GET /livez HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut head = [0u8; 12];
        stream.read_exact(&mut head).await?;
        Ok::<_, std::io::Error>(head)
    };
    let head = tokio::time::timeout(Duration::from_secs(5), check)
        .await
        .context("healthcheck timed out")?
        .context("healthcheck could not reach the internal listener")?;
    anyhow::ensure!(
        head.starts_with(b"HTTP/1.1 200") || head.starts_with(b"HTTP/1.0 200"),
        "healthcheck: /livez did not answer 200"
    );
    Ok(())
}
