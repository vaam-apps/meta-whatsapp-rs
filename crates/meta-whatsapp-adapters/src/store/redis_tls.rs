//! `rediss://` for [`RedisKvStore`](super::RedisKvStore) (feature `redis`).
//!
//! redis-rs's own TLS support builds its rustls configuration with
//! `rustls::ClientConfig::builder()`, the **process-default** crypto provider,
//! and cannot be handed another one. So this crate enables none of redis-rs's
//! TLS features: [`RedisTlsConnection`] opens the TCP connection, does the TLS
//! handshake with a configuration built on an explicit aws-lc-rs provider, and
//! hands the encrypted stream to redis-rs
//! (`MultiplexedConnection::new_with_config`, which takes any stream).
//!
//! What an integrator needs to know is on [`RedisTls`]'s own docs: this
//! module is private, so its docs are not rendered.

use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use redis::aio::{ConnectionLike, MultiplexedConnection};
use redis::{
    AsyncConnectionConfig, Cmd, ConnectionAddr, ErrorKind, IntoConnectionInfo, Pipeline,
    RedisConnectionInfo, RedisError, RedisFuture, RedisResult, Value,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// redis-rs's defaults for a multiplexed connection, kept so a `rediss://`
/// store times out as a `redis://` one does.
const DEFAULT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(1);
const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_millis(500);

/// TLS settings for `rediss://`: a rustls configuration whose crypto
/// provider is chosen **explicitly** (aws-lc-rs), never the process default.
///
/// ```no_run
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use meta_whatsapp_adapters::store::{RedisKvStore, RedisTls};
///
/// // A private CA (self-hosted Redis); leave it out to trust the platform's roots.
/// let ca = std::fs::read("/etc/redis/tls/ca.crt")?;
/// let tls = RedisTls::builder().root_certificates_pem(ca).build()?;
/// let kv = RedisKvStore::new(tls.connect("rediss://:password@redis.internal:6380").await?);
/// # Ok(()) }
/// ```
///
/// # Why not redis-rs's own TLS
///
/// redis-rs builds its TLS configuration with `rustls::ClientConfig::builder()`,
/// which uses the process-wide default crypto provider. When rustls is built
/// with both of its providers (`aws-lc-rs`, which `reqwest` and `sqlx` use
/// here, and `ring`, which many other crates enable) and the application
/// installed none, that call **panics** on the first `rediss://` connection.
/// Which providers are linked depends on the final binary's whole dependency
/// graph, so a library cannot promise it will not happen. `RedisTls` names its
/// provider instead, so it works whether or not a process default is
/// installed, and whichever one is: an application that installed `ring` as
/// its default still gets aws-lc-rs here.
///
/// # Trust
///
/// - **Default: the platform's trust store**, through
///   `rustls-platform-verifier`, as the reqwest transport does (on Linux, the
///   system's CA bundle; [`build`](RedisTlsBuilder::build) fails when it holds
///   no certificate, as in a `scratch` image). Managed Redis offerings with
///   publicly trusted certificates need nothing more.
/// - **A private CA**: [`root_certificates_pem`](RedisTlsBuilder::root_certificates_pem)
///   trusts those certificates **instead of** the platform's.
/// - **Client certificates (mutual TLS)**:
///   [`client_auth_pem`](RedisTlsBuilder::client_auth_pem). A self-hosted
///   Redis asks for one by default (`tls-auth-clients yes`).
/// - **Anything else** (another verifier, ALPN, a provider of your choosing):
///   build the `rustls::ClientConfig` yourself and pass it to
///   [`from_client_config`](Self::from_client_config). Its provider is then
///   yours.
///
/// There is no mode that skips certificate verification: redis-rs's
/// `#insecure` URL fragment is refused.
///
/// # Connection
///
/// [`connect`](Self::connect) returns a [`RedisTlsConnection`], which
/// reconnects on its own, as `ConnectionManager` does for `redis://`.
/// Integrators who build their own connection (any async redis connection
/// that is `Clone`) can still hand it to `RedisKvStore::new`.
#[derive(Clone)]
pub struct RedisTls {
    config: Arc<ClientConfig>,
    connection_timeout: Option<Duration>,
    response_timeout: Option<Duration>,
}

impl fmt::Debug for RedisTls {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The configuration can hold a client key: nothing of it is shown.
        f.debug_struct("RedisTls")
            .field("connection_timeout", &self.connection_timeout)
            .field("response_timeout", &self.response_timeout)
            .finish_non_exhaustive()
    }
}

impl RedisTls {
    /// A builder: aws-lc-rs, the platform's trust store, no client
    /// certificate.
    pub fn builder() -> RedisTlsBuilder {
        RedisTlsBuilder {
            roots: None,
            client_auth: None,
        }
    }

    /// TLS with a configuration of your own. Its crypto provider is the one
    /// you built it with: `rustls::ClientConfig::builder()` means the process
    /// default, the case [`builder`](Self::builder) exists to avoid.
    pub fn from_client_config(config: Arc<ClientConfig>) -> Self {
        Self {
            config,
            connection_timeout: Some(DEFAULT_CONNECTION_TIMEOUT),
            response_timeout: Some(DEFAULT_RESPONSE_TIMEOUT),
        }
    }

    /// The rustls configuration every connection uses.
    pub fn client_config(&self) -> &Arc<ClientConfig> {
        &self.config
    }

    /// The limit on one connection attempt (TCP, the TLS handshake and
    /// redis-rs's connection setup together); `None` waits indefinitely.
    /// Default 1 s, redis-rs's.
    #[must_use]
    pub fn with_connection_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.connection_timeout = timeout;
        self
    }

    /// The limit on waiting for one reply; `None` waits indefinitely.
    /// Default 500 ms, redis-rs's.
    #[must_use]
    pub fn with_response_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.response_timeout = timeout;
        self
    }

    /// Connect to `url`, a `rediss://` (or `valkeys://`) URL in redis-rs's
    /// format: `rediss://[<username>][:<password>@]<host>[:<port>][/<db>]`,
    /// `?protocol=resp3` allowed. The port defaults to 6379. The host is
    /// the name the server's certificate must carry.
    ///
    /// Connects once before returning, so a wrong address, certificate or
    /// password fails here rather than on the first command.
    ///
    /// # Errors
    ///
    /// - `ErrorKind::InvalidClientConfig`: the URL does not parse, is not
    ///   `rediss://`/`valkeys://`, carries a fragment (`#insecure`), or its
    ///   host is not a valid TLS server name, or no tokio runtime is
    ///   running.
    /// - Any error of the first connection: TCP, the handshake (an untrusted
    ///   certificate, a refused client certificate), the connection setup
    ///   (`AUTH`, `SELECT`), or the connection timeout. None of them carries
    ///   the URL or its password.
    pub async fn connect(&self, url: &str) -> RedisResult<RedisTlsConnection> {
        let target = Target::parse(url, self)?;
        let db = target.redis.db();
        let first = target.open().await?;
        Ok(RedisTlsConnection {
            shared: Arc::new(Shared {
                target,
                attempts: AtomicU64::new(1),
                slot: tokio::sync::Mutex::new(Slot {
                    generation: 1,
                    conn: Some(first),
                    last_error: None,
                }),
            }),
            db,
        })
    }
}

/// Builds a [`RedisTls`] on the aws-lc-rs provider.
pub struct RedisTlsBuilder {
    roots: Option<Vec<u8>>,
    client_auth: Option<(Vec<u8>, Vec<u8>)>,
}

impl fmt::Debug for RedisTlsBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never the client key; the rest is not worth printing either.
        f.debug_struct("RedisTlsBuilder")
            .field("platform_roots", &self.roots.is_none())
            .field("client_auth", &self.client_auth.is_some())
            .finish()
    }
}

impl RedisTlsBuilder {
    /// Trust the CA certificates in `pem` (one or more `CERTIFICATE`
    /// blocks) **instead of** the platform's trust store.
    #[must_use]
    pub fn root_certificates_pem(mut self, pem: impl Into<Vec<u8>>) -> Self {
        self.roots = Some(pem.into());
        self
    }

    /// Present a client certificate (mutual TLS): `cert_chain_pem` holds the
    /// certificate and any intermediates, leaf first; `key_pem` its private
    /// key (PKCS #8, PKCS #1 or SEC1). The key is parsed by
    /// [`build`](Self::build) and never shown by `Debug` or an error.
    #[must_use]
    pub fn client_auth_pem(
        mut self,
        cert_chain_pem: impl Into<Vec<u8>>,
        key_pem: impl Into<Vec<u8>>,
    ) -> Self {
        self.client_auth = Some((cert_chain_pem.into(), key_pem.into()));
        self
    }

    /// The rustls configuration, on `rustls::crypto::aws_lc_rs::default_provider()`
    /// with rustls' safe default protocol versions (TLS 1.2 and 1.3).
    ///
    /// # Errors
    ///
    /// `ErrorKind::InvalidClientConfig` when a PEM holds no certificate or
    /// does not parse, the key does not parse or does not match the
    /// certificate, or the platform's trust store cannot be loaded (or is
    /// empty).
    pub fn build(self) -> RedisResult<RedisTls> {
        let builder = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| invalid_with("the TLS protocol versions were refused", &e))?;
        let builder = match self.roots {
            None => builder
                .with_platform_verifier()
                .map_err(|e| invalid_with("the platform's trust store could not be loaded", &e))?,
            Some(pem) => builder.with_root_certificates(root_store(&pem)?),
        };
        let config = match self.client_auth {
            None => builder.with_no_client_auth(),
            Some((chain_pem, key_pem)) => {
                let chain = certificates(&chain_pem, "the client certificate chain")?;
                // No detail: a PEM error can quote the input, and this one is
                // a private key.
                let key = PrivateKeyDer::from_pem_slice(&key_pem)
                    .map_err(|_| invalid("the client key PEM does not parse"))?;
                builder
                    .with_client_auth_cert(chain, key)
                    .map_err(|e| invalid_with("the client certificate and key were refused", &e))?
            }
        };
        Ok(RedisTls::from_client_config(Arc::new(config)))
    }
}

fn root_store(pem: &[u8]) -> RedisResult<RootCertStore> {
    let mut store = RootCertStore::empty();
    for cert in certificates(pem, "the root certificates")? {
        store
            .add(cert)
            .map_err(|e| invalid_with("a root certificate was refused", &e))?;
    }
    Ok(store)
}

fn certificates(pem: &[u8], what: &'static str) -> RedisResult<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| invalid_with(what, &e))?;
    if certs.is_empty() {
        return Err(RedisError::from((
            ErrorKind::InvalidClientConfig,
            "no CERTIFICATE block in the PEM",
            what.to_owned(),
        )));
    }
    Ok(certs)
}

fn invalid(what: &'static str) -> RedisError {
    RedisError::from((ErrorKind::InvalidClientConfig, what))
}

fn invalid_with(what: &'static str, detail: &dyn fmt::Display) -> RedisError {
    RedisError::from((ErrorKind::InvalidClientConfig, what, detail.to_string()))
}

/// A `rediss://` connection for [`RedisKvStore`](super::RedisKvStore): a
/// redis-rs `MultiplexedConnection` over a TLS stream [`RedisTls`]
/// configured. Cheap to clone; every clone shares one connection.
///
/// **Reconnects** as `ConnectionManager` does: a command that fails because
/// the connection is gone (`RedisError::is_unrecoverable_error`) returns its
/// error, and the next command opens a new connection. A command is never
/// replayed: a write whose reply was lost may or may not have happened, and
/// the store's caller decides. Concurrent commands that find the connection
/// gone share one attempt: when it fails, they all get its error.
#[derive(Clone)]
pub struct RedisTlsConnection {
    shared: Arc<Shared>,
    db: i64,
}

impl fmt::Debug for RedisTlsConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // No credentials: host, port and database only.
        f.debug_struct("RedisTlsConnection")
            .field("host", &self.shared.target.host)
            .field("port", &self.shared.target.port)
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

struct Shared {
    target: Target,
    /// Connection attempts finished, read without the lock: a caller that
    /// waited on the lock while another attempt ran sees it moved.
    attempts: AtomicU64,
    slot: tokio::sync::Mutex<Slot>,
}

struct Slot {
    /// Which connection `conn` is (or was): a failure on an older one must
    /// not discard its replacement.
    generation: u64,
    conn: Option<MultiplexedConnection>,
    /// The error of the last attempt, while `conn` is `None`.
    last_error: Option<RedisError>,
}

impl Shared {
    /// The live connection, or a new one.
    async fn connection(&self) -> RedisResult<(u64, MultiplexedConnection)> {
        let seen = self.attempts.load(Ordering::Acquire);
        let mut slot = self.slot.lock().await;
        if let Some(conn) = &slot.conn {
            return Ok((slot.generation, conn.clone()));
        }
        if self.attempts.load(Ordering::Acquire) != seen {
            // An attempt failed while this caller waited for the lock: share
            // its outcome rather than queue one attempt per caller.
            if let Some(error) = &slot.last_error {
                return Err(error.clone());
            }
        }
        let outcome = self.target.open().await;
        self.attempts.fetch_add(1, Ordering::AcqRel);
        match outcome {
            Ok(conn) => {
                slot.generation += 1;
                slot.conn = Some(conn.clone());
                slot.last_error = None;
                Ok((slot.generation, conn))
            }
            Err(error) => {
                slot.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Drop connection `generation` after `error`, when the error means it
    /// is gone.
    async fn after_error(&self, generation: u64, error: &RedisError) {
        if !error.is_unrecoverable_error() {
            return;
        }
        let mut slot = self.slot.lock().await;
        if slot.generation == generation {
            slot.conn = None;
        }
    }
}

impl ConnectionLike for RedisTlsConnection {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        Box::pin(async move {
            let (generation, mut conn) = self.shared.connection().await?;
            let result = conn.send_packed_command(cmd).await;
            if let Err(error) = &result {
                self.shared.after_error(generation, error).await;
            }
            result
        })
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        pipeline: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let (generation, mut conn) = self.shared.connection().await?;
            let result = conn.send_packed_commands(pipeline, offset, count).await;
            if let Err(error) = &result {
                self.shared.after_error(generation, error).await;
            }
            result
        })
    }

    fn get_db(&self) -> i64 {
        self.db
    }
}

/// Where and how to connect.
struct Target {
    host: String,
    port: u16,
    server_name: ServerName<'static>,
    redis: RedisConnectionInfo,
    connector: TlsConnector,
    connection_timeout: Option<Duration>,
    config: AsyncConnectionConfig,
}

impl Target {
    fn parse(url: &str, tls: &RedisTls) -> RedisResult<Self> {
        // No error below quotes `url`: it can carry a password.
        let mut parsed =
            url::Url::parse(url).map_err(|_| invalid("the Redis URL does not parse"))?;
        if !matches!(parsed.scheme(), "rediss" | "valkeys") {
            return Err(invalid(
                "RedisTls::connect takes a rediss:// (or valkeys://) URL; connect to redis:// with redis::Client",
            ));
        }
        if parsed.fragment().is_some() {
            return Err(invalid(
                "a rediss:// URL fragment is refused: redis-rs's `#insecure` turns certificate checks off, and RedisTls has no such mode",
            ));
        }
        // redis-rs parses `rediss://` only with its TLS features, which this
        // crate leaves off; the rest of the URL is the same for `redis://`.
        parsed
            .set_scheme("redis")
            .map_err(|()| invalid("the Redis URL does not parse"))?;
        let info = parsed.into_connection_info()?;
        let ConnectionAddr::Tcp(host, port) = info.addr() else {
            return Err(invalid("the Redis URL names no TCP host"));
        };
        let server_name = ServerName::try_from(host.clone())
            .map_err(|_| invalid("the Redis host is not a valid TLS server name"))?;
        Ok(Self {
            host: host.clone(),
            port: *port,
            server_name,
            redis: info.redis_settings().clone(),
            connector: TlsConnector::from(Arc::clone(&tls.config)),
            connection_timeout: tls.connection_timeout,
            config: AsyncConnectionConfig::new().set_response_timeout(tls.response_timeout),
        })
    }

    /// One connection attempt: TCP, TLS, redis-rs's setup, under the
    /// connection timeout.
    async fn open(&self) -> RedisResult<MultiplexedConnection> {
        // `tokio::spawn` panics outside a runtime: refuse instead.
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| invalid("RedisTls connects on a tokio runtime, and none is running"))?;
        let attempt = async {
            let tcp = TcpStream::connect((self.host.as_str(), self.port)).await?;
            tcp.set_nodelay(true)?;
            let stream = self
                .connector
                .connect(self.server_name.clone(), tcp)
                .await?;
            let (conn, driver) =
                MultiplexedConnection::new_with_config(&self.redis, stream, self.config.clone())
                    .await?;
            runtime.spawn(driver);
            Ok(conn)
        };
        match self.connection_timeout {
            None => attempt.await,
            Some(limit) => tokio::time::timeout(limit, attempt)
                .await
                .unwrap_or_else(|_| {
                    Err(RedisError::from(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "connecting to Redis over TLS timed out",
                    )))
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn tls() -> RedisTls {
        // Any configuration: these tests never connect.
        let config = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
        RedisTls::from_client_config(Arc::new(config))
    }

    fn parse(url: &str) -> RedisResult<Target> {
        Target::parse(url, &tls())
    }

    #[test]
    fn parses_a_rediss_url() {
        let t = parse("rediss://alice:s3cret@redis.example.com:6380/3?protocol=resp3").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("redis.example.com", 6380));
        assert_eq!(t.redis.db(), 3);
        assert_eq!(t.redis.username(), Some("alice"));
        assert_eq!(t.redis.password(), Some("s3cret"));
        assert_eq!(t.redis.protocol(), redis::ProtocolVersion::RESP3);
        assert_eq!(
            t.server_name,
            ServerName::try_from("redis.example.com").unwrap()
        );
        assert_eq!(t.connection_timeout, Some(DEFAULT_CONNECTION_TIMEOUT));
    }

    #[test]
    fn the_port_defaults_to_6379_and_ips_are_server_names() {
        let t = parse("valkeys://127.0.0.1").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("127.0.0.1", 6379));
        assert!(matches!(t.server_name, ServerName::IpAddress(_)));
        let t = parse("rediss://[::1]:7000").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("::1", 7000));
        assert!(matches!(t.server_name, ServerName::IpAddress(_)));
    }

    #[test]
    fn refuses_plain_and_insecure_urls_without_quoting_them() {
        for url in [
            "redis://:hunter2@redis.example.com",
            "rediss://:hunter2@redis.example.com/#insecure",
            "rediss://:hunter2@redis.example.com/#anything",
            "unix:///tmp/redis.sock?pass=hunter2",
            "not a url hunter2",
        ] {
            let error = parse(url).err().expect(url);
            assert_eq!(error.kind(), ErrorKind::InvalidClientConfig, "{url}");
            assert!(!error.to_string().contains("hunter2"), "{url}: {error}");
            assert!(
                !format!("{error:?}").contains("hunter2"),
                "{url}: {error:?}"
            );
        }
    }

    #[test]
    fn debug_shows_no_credentials() {
        let t = parse("rediss://alice:hunter2@redis.example.com:6380/2").unwrap();
        let conn = RedisTlsConnection {
            db: t.redis.db(),
            shared: Arc::new(Shared {
                target: t,
                attempts: AtomicU64::new(0),
                slot: tokio::sync::Mutex::new(Slot {
                    generation: 0,
                    conn: None,
                    last_error: None,
                }),
            }),
        };
        let shown = format!("{conn:?}");
        assert!(
            shown.contains("redis.example.com") && shown.contains("6380"),
            "{shown}"
        );
        assert!(
            !shown.contains("hunter2") && !shown.contains("alice"),
            "{shown}"
        );
    }

    #[test]
    fn builder_errors_name_the_input_but_never_quote_the_key() {
        let error = RedisTls::builder()
            .root_certificates_pem(b"no pem here".to_vec())
            .build()
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidClientConfig);
        assert!(error.to_string().contains("root certificates"), "{error}");

        let key = "-----BEGIN PRIVATE KEY-----\nhunter2hunter2\n-----END PRIVATE KEY-----\n";
        let builder = RedisTls::builder().client_auth_pem(b"x".to_vec(), key);
        assert!(!format!("{builder:?}").contains("hunter2"));
        // No certificate in the chain: refused before the key is read.
        let error = builder.build().unwrap_err();
        assert!(
            error.to_string().contains("client certificate chain"),
            "{error}"
        );
        assert!(!error.to_string().contains("hunter2"), "{error}");
    }

    #[tokio::test]
    async fn connect_refuses_a_plain_url_before_any_io() {
        let error = tls().connect("redis://127.0.0.1:1").await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidClientConfig);
    }

    #[test]
    fn open_refuses_to_run_outside_a_runtime() {
        let target = parse("rediss://127.0.0.1:1").unwrap();
        let error = futures::executor::block_on(target.open()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidClientConfig);
        assert!(error.to_string().contains("tokio runtime"), "{error}");
    }
}
