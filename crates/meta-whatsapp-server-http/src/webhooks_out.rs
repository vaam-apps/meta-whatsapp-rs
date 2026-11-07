//! Webhooks out over the network (roadmap M2c; design §4.4 and §6): the
//! core's [`Sender`] and [`Resolver`] ports, as the service runs them.
//!
//! - [`HyperSender`] connects to the address the core's destination check
//!   pinned ([`SendRequest::address`]) and nowhere else: it resolves
//!   nothing itself, so the host's name is used only for TLS (SNI and the
//!   certificate's check, on the platform's trust store and an explicit
//!   aws-lc-rs provider) and the `Host` header. HTTP/1.1, the whole attempt
//!   within its time limit, a redirect never followed (its status is the
//!   answer), the answer's head within 16 KiB, at most
//!   [`SendRequest::max_response_bytes`] of the body read.
//! - **A pool** (roadmap M2j, [`PoolSettings`]): a connection whose answer
//!   was read to its end is kept idle, bounded in time, per key and in
//!   keys, and handed out only for an exact (checked address, port, host
//!   name, scheme) match, after the core's fresh lookup and allow-list
//!   check of the attempt that asks. Not tied to any endpoint's secret
//!   (the signature is per request). A connection the attempt's time limit
//!   cuts, or whose answer was not read whole, is dropped, never pooled.
//! - [`SystemResolver`] asks the system's resolver (`getaddrinfo`, through
//!   tokio): every call a fresh lookup, which the core checks.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper_util::rt::TokioIo;
use meta_whatsapp_rs::webhooks::axum::http::{Request, header};
use meta_whatsapp_server_core::webhooks_out::destination::{Host, ResolveError, Resolver};
use meta_whatsapp_server_core::webhooks_out::dispatch::{SendError, SendRequest, Sender};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use rustls_platform_verifier::BuilderVerifierExt as _;
use tokio::net::TcpStream;
use tokio::time::Instant;

/// Why a [`HyperSender`] could not be built.
#[derive(Debug, thiserror::Error)]
#[error("the TLS configuration of webhooks out could not be built: {0}")]
pub struct SenderError(&'static str);

/// The bounds of the sender's connection pool (roadmap M2j, design D400
/// to D403): connections kept idle between attempts. A connection is
/// handed out only to an attempt whose own fresh lookup, checked by the
/// core, named the same address, port and host name, over the same
/// scheme; nothing else shares one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSettings {
    /// How long a connection may sit idle before it is closed. Zero turns
    /// the pool off. Keep it below what receivers keep a connection for
    /// (many close after 5 to 60 s).
    pub idle_timeout: Duration,
    /// Idle connections kept per (address, port, host name, scheme). Zero
    /// turns the pool off.
    pub max_idle_per_key: usize,
    /// Distinct keys holding idle connections at once: a connection that
    /// would open another is closed instead. Zero turns the pool off.
    pub max_keys: usize,
}

impl Default for PoolSettings {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(30),
            max_idle_per_key: 2,
            max_keys: 64,
        }
    }
}

impl PoolSettings {
    /// No pool: one connection per attempt, as before roadmap M2j.
    #[must_use]
    pub const fn off() -> Self {
        Self {
            idle_timeout: Duration::ZERO,
            max_idle_per_key: 0,
            max_keys: 0,
        }
    }

    /// Whether any connection is ever kept.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        !self.idle_timeout.is_zero() && self.max_idle_per_key > 0 && self.max_keys > 0
    }
}

/// What an idle connection may serve: the address the attempt's own
/// lookup was checked at, the host name its TLS session was made for (the
/// name or the address in the URL) and the scheme. The TLS settings are
/// the sender's: the pool belongs to one [`HyperSender`], so no other
/// configuration shares it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    address: SocketAddr,
    https: bool,
    host: PoolHost,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PoolHost {
    Name(String),
    Ip(IpAddr),
}

impl PoolKey {
    fn of(request: &SendRequest) -> Self {
        Self {
            address: request.address,
            https: request.destination.https,
            host: match &request.destination.host {
                Host::Name(name) => PoolHost::Name(name.clone()),
                Host::Ip(ip) => PoolHost::Ip(*ip),
            },
        }
    }
}

/// An HTTP/1.1 connection and the task driving it, which is aborted (the
/// socket goes with it) whenever the connection is dropped: when its
/// attempt ends without returning it, when the attempt's time limit cuts
/// the attempt, when the pool evicts it. Detached, a receiver that stops
/// reading would keep it for as long as it wished (a TLS close waits for
/// the peer).
struct Connection {
    sender: hyper::client::conn::http1::SendRequest<Full<Bytes>>,
    _driver: Driver,
}

/// A task aborted when dropped.
struct Driver(tokio::task::JoinHandle<()>);

impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A connection waiting for its next attempt, and the task closing it when
/// its idle time is up (so an endpoint deleted or never called again
/// leaves a connection for the idle time at most).
struct Idle {
    id: u64,
    connection: Connection,
    since: Instant,
    _reaper: Driver,
}

#[derive(Default)]
struct PoolState {
    next: u64,
    idle: HashMap<PoolKey, VecDeque<Idle>>,
}

/// The idle connections of one sender.
struct Pool {
    settings: PoolSettings,
    state: Mutex<PoolState>,
}

/// How long a pooled connection may take to say it is ready: an idle
/// one does at once; one the peer closed never does.
const READY_WITHIN: Duration = Duration::from_millis(100);

impl Pool {
    fn new(settings: PoolSettings) -> Arc<Self> {
        Arc::new(Self {
            settings,
            state: Mutex::default(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An idle connection for exactly `key`, the most recently used first;
    /// one past its idle time, or closed by its peer, is dropped.
    async fn take(&self, key: &PoolKey) -> Option<Connection> {
        if !self.settings.enabled() {
            return None;
        }
        loop {
            let entry = {
                let mut state = self.lock();
                let queue = state.idle.get_mut(key)?;
                let entry = queue.pop_back();
                if queue.is_empty() {
                    state.idle.remove(key);
                }
                entry?
            };
            let Idle {
                mut connection,
                since,
                ..
            } = entry;
            if since.elapsed() >= self.settings.idle_timeout || !ready(&mut connection).await {
                continue;
            }
            return Some(connection);
        }
    }

    /// Keep `connection` idle for `key`, within the bounds: it is closed
    /// instead when the pool is off, the key's queue is full of newer
    /// ones, or the key would be one too many.
    fn put(self: &Arc<Self>, key: PoolKey, connection: Connection) {
        if !self.settings.enabled() {
            return;
        }
        let evicted = {
            let mut state = self.lock();
            if !state.idle.contains_key(&key) && state.idle.len() >= self.settings.max_keys {
                return;
            }
            state.next = state.next.wrapping_add(1);
            let id = state.next;
            let weak = Arc::downgrade(self);
            let (reaper_key, idle) = (key.clone(), self.settings.idle_timeout);
            let reaper = Driver(tokio::spawn(async move {
                tokio::time::sleep(idle).await;
                if let Some(pool) = Weak::upgrade(&weak) {
                    pool.remove(&reaper_key, id);
                }
            }));
            let queue = state.idle.entry(key).or_default();
            queue.push_back(Idle {
                id,
                connection,
                since: Instant::now(),
                _reaper: reaper,
            });
            if queue.len() > self.settings.max_idle_per_key {
                queue.pop_front()
            } else {
                None
            }
        };
        drop(evicted);
    }

    /// Close the idle connection `id` of `key`, if it is still idle.
    fn remove(&self, key: &PoolKey, id: u64) {
        let removed = {
            let mut state = self.lock();
            let Some(queue) = state.idle.get_mut(key) else {
                return;
            };
            let removed = queue
                .iter()
                .position(|e| e.id == id)
                .and_then(|i| queue.remove(i));
            if queue.is_empty() {
                state.idle.remove(key);
            }
            removed
        };
        drop(removed);
    }
}

/// Whether `connection` can take a request now.
async fn ready(connection: &mut Connection) -> bool {
    !connection.sender.is_closed()
        && matches!(
            tokio::time::timeout(READY_WITHIN, connection.sender.ready()).await,
            Ok(Ok(()))
        )
}

/// Sends attempts over TCP and TLS to the pinned address. Cheap to clone:
/// clones share one connection pool.
#[derive(Clone)]
pub struct HyperSender {
    tls: Arc<ClientConfig>,
    pool: Arc<Pool>,
}

impl std::fmt::Debug for HyperSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HyperSender")
            .field("pool", &self.pool.settings)
            .finish_non_exhaustive()
    }
}

impl HyperSender {
    /// Trusting the platform's certificate store, with the default pool
    /// ([`PoolSettings::default`]).
    ///
    /// # Errors
    ///
    /// [`SenderError`]: the protocol versions, or the platform's store.
    pub fn new() -> Result<Self, SenderError> {
        let config = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| SenderError("the protocol versions"))?
        .with_platform_verifier()
        .map_err(|_| SenderError("the platform's trust store"))?
        .with_no_client_auth();
        Ok(Self::from_config(config))
    }

    /// Trusting `roots` only (tests, a private CA), with the default pool.
    ///
    /// # Errors
    ///
    /// [`SenderError`]: the protocol versions.
    pub fn with_roots(roots: RootCertStore) -> Result<Self, SenderError> {
        let config = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| SenderError("the protocol versions"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self::from_config(config))
    }

    fn from_config(config: ClientConfig) -> Self {
        Self {
            tls: Arc::new(config),
            pool: Pool::new(PoolSettings::default()),
        }
    }

    /// The same sender with `settings` for its pool, empty:
    /// [`PoolSettings::off`] keeps no connection.
    #[must_use]
    pub fn with_pool(mut self, settings: PoolSettings) -> Self {
        self.pool = Pool::new(settings);
        self
    }

    /// How many connections sit idle in the pool now.
    #[must_use]
    pub fn idle_connections(&self) -> usize {
        self.pool.lock().idle.values().map(VecDeque::len).sum()
    }

    /// A new connection to the pinned address (TLS for `https`).
    async fn connect(&self, request: &SendRequest) -> Result<Connection, SendError> {
        let tcp = TcpStream::connect(request.address)
            .await
            .map_err(|_| SendError::Connect)?;
        if !request.destination.https {
            return handshake(tcp).await;
        }
        let name = match &request.destination.host {
            Host::Name(name) => ServerName::try_from(name.clone()).map_err(|_| SendError::Tls)?,
            Host::Ip(ip) => ServerName::IpAddress((*ip).into()),
        };
        let tls = tokio_rustls::TlsConnector::from(self.tls.clone())
            .connect(name, tcp)
            .await
            .map_err(|_| SendError::Tls)?;
        handshake(tls).await
    }

    /// One attempt. `request.address` was checked by the core's fresh
    /// lookup just before this call, and only a connection made for
    /// exactly that address, port, host name and scheme is taken from
    /// the pool: the pool is consulted here, after the check, never
    /// before it, so an address the allow-list refuses never reaches it.
    async fn attempt(&self, mut request: SendRequest) -> Result<u16, SendError> {
        let key = PoolKey::of(&request);
        let body = Bytes::from(std::mem::take(&mut request.body));
        if let Some(mut connection) = self.pool.take(&key).await {
            match exchange(&mut connection, &request, body.clone()).await {
                Ok((status, reusable)) => {
                    if reusable {
                        self.pool.put(key, connection);
                    }
                    return Ok(status);
                }
                // The peer closed it between the check and the write:
                // nothing was sent, so a new connection may.
                Err(Failure::NotSent) => {}
                Err(Failure::Failed(error)) => return Err(error),
            }
        }
        let mut connection = self.connect(&request).await?;
        match exchange(&mut connection, &request, body).await {
            Ok((status, reusable)) => {
                if reusable {
                    self.pool.put(key, connection);
                }
                Ok(status)
            }
            Err(Failure::NotSent) => Err(SendError::Protocol),
            Err(Failure::Failed(error)) => Err(error),
        }
    }
}

/// The most of an answer hyper buffers at once, its head included (its
/// default is about 400 KiB): a head past it is [`SendError::Protocol`].
/// hyper's floor is 8 KiB.
const MAX_ANSWER_BUFFER: usize = 16 * 1024;

/// HTTP/1.1 over `io`, its connection driven by a task the returned
/// [`Connection`] owns.
async fn handshake<I>(io: I) -> Result<Connection, SendError>
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (sender, connection) = hyper::client::conn::http1::Builder::new()
        .max_buf_size(MAX_ANSWER_BUFFER)
        .handshake(TokioIo::new(io))
        .await
        .map_err(|_| SendError::Protocol)?;
    Ok(Connection {
        sender,
        _driver: Driver(tokio::spawn(async move {
            let _ = connection.await;
        })),
    })
}

/// How an exchange failed.
enum Failure {
    /// The connection was not usable and the request was not written.
    NotSent,
    /// The attempt failed.
    Failed(SendError),
}

/// One `POST` on `connection`: its status, and whether the connection may
/// serve another attempt (the answer's body ended within
/// `max_response_bytes` and the connection is still open). At most
/// `max_response_bytes` of the body is read; past it the connection is
/// not reusable and goes.
async fn exchange(
    connection: &mut Connection,
    request: &SendRequest,
    body: Bytes,
) -> Result<(u16, bool), Failure> {
    let mut builder = Request::post(request.destination.path_and_query.as_str())
        .header(header::HOST, request.destination.authority.as_str());
    for (name, value) in &request.headers {
        builder = builder.header(*name, value.as_str());
    }
    let outgoing = builder
        .body(Full::new(body))
        .map_err(|_| Failure::Failed(SendError::Protocol))?;
    let response = connection
        .sender
        .try_send_request(outgoing)
        .await
        .map_err(|e| {
            if e.message().is_some() {
                Failure::NotSent
            } else {
                Failure::Failed(SendError::Protocol)
            }
        })?;
    let status = response.status().as_u16();
    let mut body = response.into_body();
    let mut read = 0usize;
    let mut ended = false;
    while read <= request.max_response_bytes {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    read = read.saturating_add(data.len());
                }
            }
            Some(Err(_)) => break,
            None => {
                ended = true;
                break;
            }
        }
    }
    // The rest of an unfinished answer is never read: the connection goes.
    let reusable = ended && ready(connection).await;
    Ok((status, reusable))
}

#[async_trait]
impl Sender for HyperSender {
    async fn send(&self, request: SendRequest) -> Result<u16, SendError> {
        let timeout = request.timeout;
        tokio::time::timeout(timeout, self.attempt(request))
            .await
            .map_err(|_| SendError::Timeout)?
    }
}

/// The system's resolver, through tokio (`getaddrinfo` on a blocking
/// thread).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

#[async_trait]
impl Resolver for SystemResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, ResolveError> {
        let addresses = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| ResolveError)?;
        Ok(addresses.map(|a| a.ip()).collect())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code: a panic is the report
mod tests {
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use meta_whatsapp_server_core::webhooks_out::destination::DestinationPolicy;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use super::*;

    /// What a receiver got: its request's head, as text.
    type Seen = Arc<Mutex<Vec<String>>>;

    /// A plain HTTP receiver on 127.0.0.1 answering `answer` (raw bytes,
    /// written whole or until the client goes) to every request; how many
    /// bytes it managed to write to the last one.
    async fn receiver(answer: Vec<u8>) -> (SocketAddr, Seen, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Seen = Arc::default();
        let written = Arc::new(AtomicUsize::new(0));
        let (s, w) = (seen.clone(), written.clone());
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let (s, w, answer) = (s.clone(), w.clone(), answer.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|x| x == b"\r\n\r\n") {
                        let Ok(n) = tcp.read(&mut buf).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        head.extend_from_slice(&buf[..n]);
                    }
                    s.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&head).into_owned());
                    let mut sent = 0;
                    for chunk in answer.chunks(16 * 1024) {
                        if tcp.write_all(chunk).await.is_err() {
                            break;
                        }
                        sent += chunk.len();
                        w.store(sent, Ordering::SeqCst);
                    }
                });
            }
        });
        (addr, seen, written)
    }

    fn request(url: &str, address: SocketAddr) -> SendRequest {
        let policy = DestinationPolicy::parse("receiver.test", true).unwrap();
        SendRequest {
            destination: policy.check_url(url).unwrap(),
            address,
            headers: vec![
                ("content-type", "application/json".to_owned()),
                ("webhook-id", "evt_1".to_owned()),
            ],
            body: b"{\"a\":1}".to_vec(),
            timeout: Duration::from_secs(5),
            max_response_bytes: 1024,
        }
    }

    /// The attempt goes to the pinned address with the URL's host as
    /// `Host` (`receiver.test` resolves nowhere: no lookup was made), its
    /// path and query, its headers and body; a 3xx is the answer, its
    /// `Location` never asked. Decisive: the connection to
    /// `request.address`, and no redirect followed.
    #[tokio::test]
    async fn an_attempt_goes_to_the_pinned_address_and_follows_no_redirect() {
        let (target, target_seen, _) =
            receiver(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec()).await;
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nlocation: http://{target}/stolen\r\ncontent-length: 0\r\n\r\n"
        );
        let (addr, seen, _) = receiver(redirect.into_bytes()).await;
        let status = HyperSender::new()
            .unwrap()
            .send(request("http://receiver.test:8080/hook?x=1", addr))
            .await;
        assert_eq!(status, Ok(302));
        let head = seen.lock().unwrap()[0].clone();
        assert!(head.starts_with("POST /hook?x=1 HTTP/1.1\r\n"), "{head}");
        assert!(
            head.to_ascii_lowercase()
                .contains("host: receiver.test:8080\r\n"),
            "{head}"
        );
        assert!(head.contains("webhook-id: evt_1\r\n"), "{head}");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            target_seen.lock().unwrap().is_empty(),
            "the redirect was not followed"
        );
    }

    /// At most `max_response_bytes` of an answer is read: a receiver
    /// sending 64 MiB writes only what the sockets' buffers hold before
    /// the connection goes. Decisive: the read's bound.
    #[tokio::test]
    async fn at_most_1_kib_of_the_answer_is_read() {
        let size = 64 * 1024 * 1024;
        let mut answer = format!("HTTP/1.1 200 OK\r\ncontent-length: {size}\r\n\r\n").into_bytes();
        answer.resize(answer.len() + size, b'x');
        let (addr, _, written) = receiver(answer).await;
        let status = HyperSender::new()
            .unwrap()
            .send(request("http://receiver.test/", addr))
            .await;
        assert_eq!(status, Ok(200));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let written = written.load(Ordering::SeqCst);
        assert!(written < size / 2, "the receiver wrote {written} bytes");
    }

    /// A receiver that never answers costs the attempt its time limit,
    /// then `Timeout`; a closed port is `Connect`.
    #[tokio::test]
    async fn a_silent_receiver_times_out() {
        // A port closed here must stay closed until it is tried.
        let _ports = crate::LOOPBACK_PORTS.lock().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let held = tokio::spawn(async move {
            let accepted = listener.accept().await;
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(accepted);
        });
        let mut silent = request("http://receiver.test/", addr);
        silent.timeout = Duration::from_millis(300);
        let started = tokio::time::Instant::now();
        assert_eq!(
            HyperSender::new().unwrap().send(silent).await,
            Err(SendError::Timeout)
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        held.abort();
        let closed = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        assert_eq!(
            HyperSender::new()
                .unwrap()
                .send(request("http://receiver.test/", closed))
                .await,
            Err(SendError::Connect)
        );
    }

    /// A TLS receiver whose certificate is the URL host's, reached at the
    /// pinned address: the handshake names the host (SNI) and checks the
    /// certificate against it; a certificate for another name, or of an
    /// untrusted issuer, is `Tls`, nothing sent.
    #[tokio::test]
    async fn tls_checks_the_hosts_certificate_at_the_pinned_address() {
        let make = |name: &str| {
            let key = rcgen::KeyPair::generate().unwrap();
            let cert = rcgen::CertificateParams::new(vec![name.to_owned()])
                .unwrap()
                .self_signed(&key)
                .unwrap();
            (cert.der().clone(), key.serialize_der())
        };
        let (good, good_key) = make("receiver.test");
        let (other, other_key) = make("other.test");
        let serve = |cert: CertificateDer<'static>, key: Vec<u8>| async move {
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )
            .unwrap();
            let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let heads: Seen = Arc::default();
            let h = heads.clone();
            tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let (acceptor, h) = (acceptor.clone(), h.clone());
                    tokio::spawn(async move {
                        let Ok(mut tls) = acceptor.accept(tcp).await else {
                            return;
                        };
                        let sni = tls.get_ref().1.server_name().map(str::to_owned);
                        let mut buf = vec![0u8; 4096];
                        let n = tls.read(&mut buf).await.unwrap_or(0);
                        h.lock()
                            .unwrap()
                            .push(format!("{sni:?} {}", String::from_utf8_lossy(&buf[..n])));
                        let _ = tls.write_all(b"HTTP/1.1 204 No Content\r\n\r\n").await;
                        let _ = tls.shutdown().await;
                    });
                }
            });
            (addr, heads)
        };
        let mut roots = RootCertStore::empty();
        roots.add(good.clone()).unwrap();
        roots.add(other.clone()).unwrap();
        let sender = HyperSender::with_roots(roots).unwrap();
        let (addr, heads) = serve(good, good_key).await;
        assert_eq!(
            sender
                .send(request("https://receiver.test/hook", addr))
                .await,
            Ok(204)
        );
        let head = heads.lock().unwrap()[0].clone();
        assert!(
            head.starts_with("Some(\"receiver.test\") POST /hook"),
            "{head}"
        );
        let (wrong, wrong_heads) = serve(other, other_key).await;
        assert_eq!(
            sender
                .send(request("https://receiver.test/hook", wrong))
                .await,
            Err(SendError::Tls),
            "a certificate for another name"
        );
        assert!(wrong_heads.lock().unwrap().is_empty());
        let untrusting = HyperSender::with_roots(RootCertStore::empty()).unwrap();
        assert_eq!(
            untrusting
                .send(request("https://receiver.test/hook", addr))
                .await,
            Err(SendError::Tls),
            "an untrusted issuer"
        );
    }

    /// A rustls server config presenting a fresh self-signed certificate
    /// for `name`, and that certificate.
    fn tls_server(name: &str) -> (Arc<rustls::ServerConfig>, CertificateDer<'static>) {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec![name.to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let der = cert.der().clone();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![der.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
        (Arc::new(config), der)
    }

    /// A slow-loris receiver (roadmap M2c's security review): it completes
    /// the TLS handshake, then never reads. The attempt times out, and its
    /// connection goes with it: the receiver's writes then meet a closed
    /// socket. Kept open, every such attempt would hold a socket and a
    /// task for as long as the receiver wished (a TLS close waits for the
    /// peer to read). Decisive: the connection's task aborted when the
    /// attempt is dropped.
    #[tokio::test]
    async fn a_timed_out_attempt_closes_its_connection() {
        let (config, cert) = tls_server("receiver.test");
        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (held_tx, held_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(tcp).await.unwrap();
            let _ = held_tx.send(tls);
        });
        let mut roots = RootCertStore::empty();
        roots.add(cert).unwrap();
        let sender = HyperSender::with_roots(roots).unwrap();
        let mut slow = request("https://receiver.test/hook", addr);
        // More than the sockets' buffers hold: the write blocks.
        slow.body = vec![b'x'; 32 * 1024 * 1024];
        slow.timeout = Duration::from_millis(500);
        assert_eq!(sender.send(slow).await, Err(SendError::Timeout));
        let mut tls = held_rx.await.unwrap();
        let (tcp, _) = tls.get_mut();
        let mut closed = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if tcp.write_all(b"x").await.is_err() {
                closed = true;
                break;
            }
        }
        assert!(closed, "the attempt's connection outlived it");
    }

    /// An answer's head is bounded too: a receiver sending 64 KiB of
    /// headers is `Protocol`, not buffered whole (hyper's default buffer
    /// is about 400 KiB, 32 attempts at once). Decisive:
    /// `MAX_ANSWER_BUFFER`.
    #[tokio::test]
    async fn an_oversized_answer_head_is_refused() {
        let mut answer = b"HTTP/1.1 200 OK\r\n".to_vec();
        for i in 0..64 {
            answer.extend_from_slice(format!("x-pad-{i}: {}\r\n", "y".repeat(1000)).as_bytes());
        }
        answer.extend_from_slice(b"content-length: 0\r\n\r\n");
        let (addr, _, _) = receiver(answer).await;
        assert_eq!(
            HyperSender::new()
                .unwrap()
                .send(request("http://receiver.test/", addr))
                .await,
            Err(SendError::Protocol)
        );
        let (small, _, _) =
            receiver(b"HTTP/1.1 204 No Content\r\nx-small: 1\r\n\r\n".to_vec()).await;
        assert_eq!(
            HyperSender::new()
                .unwrap()
                .send(request("http://receiver.test/", small))
                .await,
            Ok(204)
        );
    }

    /// A delivery's body is the envelope `GET /v1/events/{id}` answers,
    /// field for field (the core builds it without the HTTP crate), with
    /// `truncated: false`; over 256 KiB, the same but `data: null` and
    /// `truncated: true`. Decisive: a field the core's envelope renames or
    /// drops.
    #[test]
    fn a_deliverys_body_is_the_events_envelope() {
        use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
        use meta_whatsapp_server_core::model::TenantId;
        use meta_whatsapp_server_core::outbox::StoredEvent;
        use meta_whatsapp_server_core::webhooks_out::MAX_DELIVERED_DATA_BYTES;
        use meta_whatsapp_server_core::webhooks_out::dispatch::event_body;
        let tenant = TenantId::parse("merchant-42").unwrap();
        let mut event = StoredEvent {
            sequence: 18342,
            id: "evt_3f9c0a6e1b2d4c58a7e9f0b1c2d3e4f5".to_owned(),
            tenant: Some(tenant.clone()),
            phone_number_id: Some(PhoneNumberId::new("106540352242922").unwrap()),
            waba_id: Some(WabaId::new("102290129340398").unwrap()),
            event_type: "message_received".to_owned(),
            data: r#"{"event":"message_received","message":{"id":"wamid.x"}}"#.to_owned(),
            created_at: time::macros::datetime!(2026-09-24 10:00:01 UTC),
        };
        let delivered: serde_json::Value =
            serde_json::from_slice(&event_body(&tenant, &event)).unwrap();
        let polled =
            serde_json::to_value(crate::api::events::envelope(&tenant, event.clone()).unwrap())
                .unwrap();
        assert_eq!(delivered, polled);
        event.phone_number_id = None;
        event.data = format!(
            r#"{{"event":"history","pad":"{}"}}"#,
            "x".repeat(MAX_DELIVERED_DATA_BYTES)
        );
        let truncated: serde_json::Value =
            serde_json::from_slice(&event_body(&tenant, &event)).unwrap();
        let mut whole =
            serde_json::to_value(crate::api::events::envelope(&tenant, event).unwrap()).unwrap();
        whole["truncated"] = serde_json::Value::Bool(true);
        whole["data"] = serde_json::Value::Null;
        assert_eq!(truncated, whole);
    }

    /// The system resolver answers `localhost` from the hosts file (the
    /// core refuses a loopback address unless a listed CIDR holds it).
    #[tokio::test]
    async fn the_system_resolver_looks_up() {
        let ips = SystemResolver.resolve("localhost", 443).await.unwrap();
        assert!(
            !ips.is_empty() && ips.iter().all(IpAddr::is_loopback),
            "{ips:?}"
        );
    }

    // ---- the connection pool (roadmap M2j) ----

    use meta_whatsapp_server_core::webhooks_out::destination::{Refusal, pin};

    /// What a keep-alive receiver counts.
    #[derive(Clone, Default)]
    struct Counts {
        /// TCP connections accepted.
        accepted: Arc<AtomicUsize>,
        /// Requests answered.
        requests: Arc<AtomicUsize>,
        /// Connections the client closed.
        closed: Arc<AtomicUsize>,
    }

    impl Counts {
        fn accepted(&self) -> usize {
            self.accepted.load(Ordering::SeqCst)
        }
        fn requests(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }
        fn closed(&self) -> usize {
            self.closed.load(Ordering::SeqCst)
        }
    }

    /// Answers `204` to every request of `io` until the client closes it
    /// (or, with `one_shot`, after the first).
    async fn serve_requests<S>(mut io: S, counts: Counts, one_shot: bool)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let head_end = loop {
                if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
                let n = io.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    counts.closed.fetch_add(1, Ordering::SeqCst);
                    return;
                }
                data.extend_from_slice(&buf[..n]);
            };
            let head = String::from_utf8_lossy(&data[..head_end]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while data.len() < head_end + length {
                let n = io.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                data.extend_from_slice(&buf[..n]);
            }
            data.drain(..head_end + length);
            counts.requests.fetch_add(1, Ordering::SeqCst);
            if io
                .write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
                .await
                .is_err()
                || one_shot
            {
                return;
            }
        }
    }

    /// A plain keep-alive receiver on `ip`, on `port` (0: any).
    async fn keepalive(ip: &str, port: u16, one_shot: bool) -> (SocketAddr, Counts) {
        let listener = TcpListener::bind((ip, port)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let counts = Counts::default();
        let c = counts.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                c.accepted.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(serve_requests(tcp, c.clone(), one_shot));
            }
        });
        (addr, counts)
    }

    /// Two plain receivers on one port, at 127.0.0.1 and 127.0.0.2.
    async fn twin_receivers() -> (u16, Counts, Counts) {
        for _ in 0..50 {
            let (first, a) = keepalive("127.0.0.1", 0, false).await;
            let Ok(listener) = TcpListener::bind(("127.0.0.2", first.port())).await else {
                continue;
            };
            let counts = Counts::default();
            let c = counts.clone();
            tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    c.accepted.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(serve_requests(tcp, c.clone(), false));
                }
            });
            return (first.port(), a, counts);
        }
        panic!("no free port on both 127.0.0.1 and 127.0.0.2");
    }

    /// A lookup answering the address it is set to.
    struct Answer(Mutex<IpAddr>);

    impl Answer {
        fn new(ip: &str) -> Self {
            Self(Mutex::new(ip.parse().unwrap()))
        }
        fn set(&self, ip: &str) {
            *self.0.lock().unwrap() = ip.parse().unwrap();
        }
    }

    #[async_trait]
    impl Resolver for Answer {
        async fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, ResolveError> {
            Ok(vec![*self.0.lock().unwrap()])
        }
    }

    /// One attempt the way the dispatcher makes it: the URL checked and its
    /// host looked up afresh by the core (`pin`), then the sender asked
    /// to send to the address that check pinned.
    async fn deliver(
        sender: &HyperSender,
        policy: &DestinationPolicy,
        resolver: &dyn Resolver,
        url: &str,
    ) -> Result<Result<u16, SendError>, Refusal> {
        let pinned = pin(policy, resolver, url).await?;
        Ok(sender
            .send(SendRequest {
                destination: pinned.destination,
                address: pinned.address,
                headers: vec![("content-type", "application/json".to_owned())],
                body: b"{\"a\":1}".to_vec(),
                timeout: Duration::from_secs(5),
                max_response_bytes: 1024,
            })
            .await)
    }

    async fn settle() {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    fn loopback() -> DestinationPolicy {
        DestinationPolicy::parse("127.0.0.0/8", true).unwrap()
    }

    /// Roadmap M2j's three decisive cases, through the core's check and
    /// the sender. (1) Two attempts to one endpoint within the idle time
    /// make one TCP connection. (2) When the fresh lookup answers another
    /// address, the attempt opens a connection to that one: the pooled
    /// connection to the first is not used, though the host name, port and
    /// path are the same. (3) A pooled connection to an address the list
    /// no longer admits is never used: the attempt is refused before the
    /// sender, and the receiver sees nothing more.
    #[tokio::test]
    async fn a_pooled_connection_serves_only_the_address_its_attempt_checked() {
        let (port, first, second) = twin_receivers().await;
        let url = format!("http://receiver.test:{port}/hook");
        let sender = HyperSender::new().unwrap();
        let resolver = Answer::new("127.0.0.1");
        let policy = loopback();

        assert_eq!(
            deliver(&sender, &policy, &resolver, &url).await,
            Ok(Ok(204))
        );
        assert_eq!(
            deliver(&sender, &policy, &resolver, &url).await,
            Ok(Ok(204))
        );
        assert_eq!((first.accepted(), first.requests()), (1, 2), "(1)");
        assert_eq!(sender.idle_connections(), 1);

        resolver.set("127.0.0.2");
        assert_eq!(
            deliver(&sender, &policy, &resolver, &url).await,
            Ok(Ok(204))
        );
        assert_eq!(
            (second.accepted(), second.requests()),
            (1, 1),
            "(2) a connection to the address checked"
        );
        assert_eq!(
            (first.accepted(), first.requests()),
            (1, 2),
            "(2) the pooled connection to the other address was not used"
        );
        assert_eq!(sender.idle_connections(), 2);

        // The list narrows to the second address; the lookup answers the
        // first, whose connection is still pooled.
        resolver.set("127.0.0.1");
        let narrowed = DestinationPolicy::parse("127.0.0.2/32", true).unwrap();
        assert_eq!(
            deliver(&sender, &narrowed, &resolver, &url).await,
            Err(Refusal::Address)
        );
        settle().await;
        assert_eq!((first.accepted(), first.requests()), (1, 2), "(3)");
        assert_eq!(first.closed(), 0, "the pooled connection waits, unused");
        // Admitted again, it is used again.
        assert_eq!(
            deliver(&sender, &policy, &resolver, &url).await,
            Ok(Ok(204))
        );
        assert_eq!((first.accepted(), first.requests()), (1, 3));
    }

    /// The pool is keyed by the checked address and the host name, never
    /// by the name alone: one name that two lookups answered differently
    /// shares nothing (above), and two names at one address share
    /// nothing either. Plain `http` here; TLS below.
    #[tokio::test]
    async fn two_host_names_at_one_address_do_not_share_a_connection() {
        let (addr, counts) = keepalive("127.0.0.1", 0, false).await;
        let sender = HyperSender::new().unwrap();
        let resolver = Answer::new("127.0.0.1");
        let policy = loopback();
        let port = addr.port();
        for host in [
            "one.test", "two.test", "one.test", "two.test", "one.test", "two.test",
        ] {
            let url = format!("http://{host}:{port}/");
            assert_eq!(
                deliver(&sender, &policy, &resolver, &url).await,
                Ok(Ok(204))
            );
        }
        assert_eq!((counts.accepted(), counts.requests()), (2, 6));
        // An address in the URL is a third key.
        assert_eq!(
            deliver(&sender, &policy, &resolver, &format!("http://127.0.0.1:{port}/")).await,
            Ok(Ok(204))
        );
        assert_eq!(counts.accepted(), 3);
    }

    /// Over TLS: the reuse needs the same host name (SNI and certificate)
    /// as well as the address; the certificate is checked once, at the
    /// handshake the connection was made by.
    #[tokio::test]
    async fn over_tls_a_connection_is_reused_for_the_same_name_only() {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["one.test".to_owned(), "two.test".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let der = cert.der().clone();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![der.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let counts = Counts::default();
        let c = counts.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                c.accepted.fetch_add(1, Ordering::SeqCst);
                let (acceptor, c) = (acceptor.clone(), c.clone());
                tokio::spawn(async move {
                    if let Ok(tls) = acceptor.accept(tcp).await {
                        serve_requests(tls, c, false).await;
                    }
                });
            }
        });
        let mut roots = RootCertStore::empty();
        roots.add(der).unwrap();
        let sender = HyperSender::with_roots(roots).unwrap();
        let resolver = Answer::new("127.0.0.1");
        let policy = loopback();
        for host in ["one.test", "one.test", "two.test", "one.test", "two.test"] {
            assert_eq!(
                deliver(
                    &sender,
                    &policy,
                    &resolver,
                    &format!("https://{host}:{port}/hook")
                )
                .await,
                Ok(Ok(204)),
                "{host}"
            );
        }
        assert_eq!((counts.accepted(), counts.requests()), (2, 5));
        // A name the certificate does not cover is refused at the
        // handshake, though a connection to the address sits in the pool.
        assert_eq!(
            deliver(
                &sender,
                &policy,
                &resolver,
                &format!("https://three.test:{port}/hook")
            )
            .await,
            Ok(Err(SendError::Tls))
        );
        assert_eq!(counts.requests(), 5);
    }

    /// An idle connection is closed once the idle time is up, though
    /// nothing asks for it (an endpoint deleted, never called again, holds
    /// a socket for the idle time at most); the next attempt opens a
    /// connection. Decisive: the idle timeout.
    #[tokio::test]
    async fn an_idle_connection_is_closed_after_the_idle_time() {
        let (addr, counts) = keepalive("127.0.0.1", 0, false).await;
        let url = format!("http://receiver.test:{}/", addr.port());
        let sender = HyperSender::new().unwrap().with_pool(PoolSettings {
            idle_timeout: Duration::from_millis(300),
            ..PoolSettings::default()
        });
        let resolver = Answer::new("127.0.0.1");
        let policy = loopback();
        assert_eq!(
            deliver(&sender, &policy, &resolver, &url).await,
            Ok(Ok(204))
        );
        assert_eq!((sender.idle_connections(), counts.closed()), (1, 0));
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert_eq!(
            (sender.idle_connections(), counts.closed()),
            (0, 1),
            "closed by the pool"
        );
        assert_eq!(
            deliver(&sender, &policy, &resolver, &url).await,
            Ok(Ok(204))
        );
        assert_eq!(counts.accepted(), 2);
    }

    /// Pooling off keeps nothing: every attempt opens a connection and
    /// closes it.
    #[tokio::test]
    async fn with_the_pool_off_every_attempt_opens_its_own_connection() {
        let (addr, counts) = keepalive("127.0.0.1", 0, false).await;
        let url = format!("http://receiver.test:{}/", addr.port());
        let sender = HyperSender::new().unwrap().with_pool(PoolSettings::off());
        let resolver = Answer::new("127.0.0.1");
        let policy = loopback();
        for _ in 0..3 {
            assert_eq!(
                deliver(&sender, &policy, &resolver, &url).await,
                Ok(Ok(204))
            );
        }
        settle().await;
        assert_eq!(
            (counts.accepted(), sender.idle_connections(), counts.closed()),
            (3, 0, 3)
        );
        // Each zero is an off switch of its own.
        for settings in [
            PoolSettings {
                idle_timeout: Duration::ZERO,
                ..PoolSettings::default()
            },
            PoolSettings {
                max_idle_per_key: 0,
                ..PoolSettings::default()
            },
            PoolSettings {
                max_keys: 0,
                ..PoolSettings::default()
            },
        ] {
            assert!(!settings.enabled());
        }
        assert!(PoolSettings::default().enabled());
        assert!(!PoolSettings::off().enabled());
    }

    /// The bounds: at most `max_idle_per_key` idle connections to a key
    /// (three attempts at once open three, two are kept), and at most
    /// `max_keys` keys (a third destination is never pooled).
    #[tokio::test]
    async fn the_pool_is_bounded_per_key_and_in_keys() {
        let (a, a_counts) = keepalive("127.0.0.1", 0, false).await;
        let (b, b_counts) = keepalive("127.0.0.1", 0, false).await;
        let sender = HyperSender::new().unwrap().with_pool(PoolSettings {
            max_idle_per_key: 2,
            max_keys: 1,
            ..PoolSettings::default()
        });
        let resolver = Answer::new("127.0.0.1");
        let policy = loopback();
        let url_a = format!("http://receiver.test:{}/", a.port());
        let url_b = format!("http://receiver.test:{}/", b.port());
        let three = futures_three(&sender, &policy, &resolver, &url_a).await;
        assert_eq!(three, [Ok(Ok(204)), Ok(Ok(204)), Ok(Ok(204))]);
        settle().await;
        assert_eq!(a_counts.accepted(), 3);
        assert_eq!(sender.idle_connections(), 2, "two kept of three");
        assert_eq!(a_counts.closed(), 1, "the third closed");
        // A second key is over the bound: used, then closed.
        for _ in 0..2 {
            assert_eq!(
                deliver(&sender, &policy, &resolver, &url_b).await,
                Ok(Ok(204))
            );
        }
        settle().await;
        assert_eq!(b_counts.accepted(), 2);
        assert_eq!(sender.idle_connections(), 2);
    }

    /// Three attempts at once.
    async fn futures_three(
        sender: &HyperSender,
        policy: &DestinationPolicy,
        resolver: &dyn Resolver,
        url: &str,
    ) -> [Result<Result<u16, SendError>, Refusal>; 3] {
        let (x, y, z) = tokio::join!(
            deliver(sender, policy, resolver, url),
            deliver(sender, policy, resolver, url),
            deliver(sender, policy, resolver, url)
        );
        [x, y, z]
    }

    /// A connection the peer closed while it sat idle is never handed out
    /// to fail an attempt: the next attempt opens another and succeeds.
    #[tokio::test]
    async fn a_connection_the_peer_closed_is_replaced_not_failed() {
        let (addr, counts) = keepalive("127.0.0.1", 0, true).await;
        let url = format!("http://receiver.test:{}/", addr.port());
        let sender = HyperSender::new().unwrap();
        let resolver = Answer::new("127.0.0.1");
        let policy = loopback();
        for _ in 0..3 {
            assert_eq!(
                deliver(&sender, &policy, &resolver, &url).await,
                Ok(Ok(204))
            );
            settle().await;
        }
        assert_eq!((counts.accepted(), counts.requests()), (3, 3));
    }

    /// An answer whose body is not read whole (past
    /// `max_response_bytes`) leaves the connection mid-answer: it is
    /// closed, never pooled for a request to read someone else's body.
    #[tokio::test]
    async fn a_connection_with_an_unread_answer_is_not_pooled() {
        let size = 64 * 1024;
        let mut answer = format!("HTTP/1.1 200 OK\r\ncontent-length: {size}\r\n\r\n").into_bytes();
        answer.resize(answer.len() + size, b'x');
        let (addr, _, _) = receiver(answer).await;
        let sender = HyperSender::new().unwrap();
        let resolver = Answer::new("127.0.0.1");
        let url = format!("http://receiver.test:{}/", addr.port());
        assert_eq!(
            deliver(&sender, &loopback(), &resolver, &url).await,
            Ok(Ok(200))
        );
        assert_eq!(sender.idle_connections(), 0);
    }
}
