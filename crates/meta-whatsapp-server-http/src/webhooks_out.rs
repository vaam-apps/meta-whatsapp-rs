//! Webhooks out over the network (roadmap M2c; design §4.4 and §6): the
//! core's [`Sender`] and [`Resolver`] ports, as the service runs them.
//!
//! - [`HyperSender`] connects to the address the core's destination check
//!   pinned ([`SendRequest::address`]) and nowhere else: it resolves
//!   nothing itself, so the host's name is used only for TLS (SNI and the
//!   certificate's check, on the platform's trust store and an explicit
//!   aws-lc-rs provider) and the `Host` header. One connection per attempt,
//!   HTTP/1.1, the whole attempt within its time limit, a redirect never
//!   followed (its status is the answer), at most
//!   [`SendRequest::max_response_bytes`] of the body read, the connection
//!   then dropped.
//! - [`SystemResolver`] asks the system's resolver (`getaddrinfo`, through
//!   tokio): every call a fresh lookup, which the core checks.

use std::net::IpAddr;
use std::sync::Arc;

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
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

/// Why a [`HyperSender`] could not be built.
#[derive(Debug, thiserror::Error)]
#[error("the TLS configuration of webhooks out could not be built: {0}")]
pub struct SenderError(&'static str);

/// Sends attempts over TCP and TLS to the pinned address. Cheap to clone.
#[derive(Clone)]
pub struct HyperSender {
    tls: Arc<ClientConfig>,
}

impl std::fmt::Debug for HyperSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HyperSender").finish_non_exhaustive()
    }
}

impl HyperSender {
    /// Trusting the platform's certificate store.
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
        Ok(Self {
            tls: Arc::new(config),
        })
    }

    /// Trusting `roots` only (tests, a private CA).
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
        Ok(Self {
            tls: Arc::new(config),
        })
    }

    async fn attempt(&self, request: SendRequest) -> Result<u16, SendError> {
        let tcp = TcpStream::connect(request.address)
            .await
            .map_err(|_| SendError::Connect)?;
        if !request.destination.https {
            return exchange(tcp, request).await;
        }
        let name = match &request.destination.host {
            Host::Name(name) => ServerName::try_from(name.clone()).map_err(|_| SendError::Tls)?,
            Host::Ip(ip) => ServerName::IpAddress((*ip).into()),
        };
        let tls = tokio_rustls::TlsConnector::from(self.tls.clone())
            .connect(name, tcp)
            .await
            .map_err(|_| SendError::Tls)?;
        exchange(tls, request).await
    }
}

/// One `POST` on `io`, its status, at most `max_response_bytes` of its
/// body read.
async fn exchange<I>(io: I, request: SendRequest) -> Result<u16, SendError>
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|_| SendError::Protocol)?;
    let driver = tokio::spawn(connection);
    let mut builder = Request::post(request.destination.path_and_query.as_str())
        .header(header::HOST, request.destination.authority.as_str());
    for (name, value) in &request.headers {
        builder = builder.header(*name, value.as_str());
    }
    let outgoing = builder
        .body(Full::new(Bytes::from(request.body)))
        .map_err(|_| SendError::Protocol);
    let result = async {
        let response = sender
            .send_request(outgoing?)
            .await
            .map_err(|_| SendError::Protocol)?;
        let status = response.status().as_u16();
        let mut body = response.into_body();
        let mut read = 0usize;
        while read < request.max_response_bytes {
            match body.frame().await {
                Some(Ok(frame)) => {
                    if let Some(data) = frame.data_ref() {
                        read = read.saturating_add(data.len());
                    }
                }
                Some(Err(_)) | None => break,
            }
        }
        Ok(status)
    }
    .await;
    // The rest of the answer is never read: the connection goes.
    driver.abort();
    result
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
}
