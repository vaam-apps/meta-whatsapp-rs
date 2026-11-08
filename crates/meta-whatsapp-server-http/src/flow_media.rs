//! A Flow's uploaded media over the network (roadmap M5d2, design D454):
//! the core's [`CdnFetcher`] port, as the service runs it.
//!
//! [`HyperFetcher`] connects to the address the core's check pinned
//! ([`Pinned::address`]) and nowhere else: it resolves nothing itself, so
//! the host's name is used only for TLS (SNI and the certificate's check,
//! on the platform's trust store and an explicit aws-lc-rs provider) and
//! the `Host` header. One `GET`, HTTP/1.1, **no credential of any kind**
//! (no `Authorization`, no cookie: the request has the headers below and
//! no more), **a redirect never followed** (any status but `200` is a
//! failure), the whole fetch within its time limit, the body read up to
//! the bound and no further (a declared length past it is refused before a
//! byte of the body is read), the connection closed afterwards (nothing is
//! pooled: a file is rare and its host is the CDN's).
//!
//! It logs nothing: the core logs a failure's kind, never the URL.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper_util::rt::TokioIo;
use meta_whatsapp_rs::webhooks::axum::http::{Request, header};
use meta_whatsapp_server_core::flow_media::{CdnFetcher, FetchError};
use meta_whatsapp_server_core::webhooks_out::destination::{Host, Pinned};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use rustls_platform_verifier::BuilderVerifierExt as _;
use tokio::net::TcpStream;

/// Why a [`HyperFetcher`] could not be built.
#[derive(Debug, thiserror::Error)]
#[error("the TLS configuration of Flow media downloads could not be built: {0}")]
pub struct FetcherError(&'static str);

/// The most of an answer's head hyper buffers (its default is about 400
/// KiB); hyper's floor is 8 KiB.
const MAX_HEAD_BUFFER: usize = 16 * 1024;

/// Fetches files from a CDN over TCP and TLS to a pinned address.
#[derive(Clone)]
pub struct HyperFetcher {
    tls: Arc<ClientConfig>,
}

impl std::fmt::Debug for HyperFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HyperFetcher").finish_non_exhaustive()
    }
}

impl HyperFetcher {
    /// Trusting the platform's certificate store.
    ///
    /// # Errors
    ///
    /// [`FetcherError`]: the protocol versions, or the platform's store.
    pub fn new() -> Result<Self, FetcherError> {
        let config = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| FetcherError("the protocol versions"))?
        .with_platform_verifier()
        .map_err(|_| FetcherError("the platform's trust store"))?
        .with_no_client_auth();
        Ok(Self {
            tls: Arc::new(config),
        })
    }

    /// Trusting `roots` only (tests, a private CA).
    ///
    /// # Errors
    ///
    /// [`FetcherError`]: the protocol versions.
    pub fn with_roots(roots: RootCertStore) -> Result<Self, FetcherError> {
        let config = ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| FetcherError("the protocol versions"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            tls: Arc::new(config),
        })
    }

    async fn fetch(&self, target: &Pinned, max_bytes: usize) -> Result<Vec<u8>, FetchError> {
        let tcp = TcpStream::connect(target.address)
            .await
            .map_err(|_| FetchError::Connect)?;
        if target.destination.https {
            let name = match &target.destination.host {
                Host::Name(name) => {
                    ServerName::try_from(name.clone()).map_err(|_| FetchError::Tls)?
                }
                Host::Ip(ip) => ServerName::IpAddress((*ip).into()),
            };
            let tls = tokio_rustls::TlsConnector::from(self.tls.clone())
                .connect(name, tcp)
                .await
                .map_err(|_| FetchError::Tls)?;
            exchange(tls, target, max_bytes).await
        } else {
            exchange(tcp, target, max_bytes).await
        }
    }
}

async fn exchange<I>(io: I, target: &Pinned, max_bytes: usize) -> Result<Vec<u8>, FetchError>
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
        .max_buf_size(MAX_HEAD_BUFFER)
        .handshake(TokioIo::new(io))
        .await
        .map_err(|_| FetchError::Protocol)?;
    // Spawned, and aborted when this future ends or is dropped (the time
    // limit, the caller going away): the connection goes with it.
    let _driver = AbortOnDrop(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let result = async {
        let request = Request::get(target.destination.path_and_query.as_str())
            .header(header::HOST, target.destination.authority.as_str())
            .header(header::USER_AGENT, "meta-whatsapp-rs")
            .header(header::ACCEPT, "*/*")
            .header(header::ACCEPT_ENCODING, "identity")
            .header(header::CONNECTION, "close")
            .body(Empty::<Bytes>::new())
            .map_err(|_| FetchError::Protocol)?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|_| FetchError::Protocol)?;
        let status = response.status().as_u16();
        // A redirect, or anything but the file: the answer is the failure,
        // and its body is never read.
        if status != 200 {
            return Err(FetchError::Status(status));
        }
        let declared = response
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if declared.is_some_and(|n| n > max_bytes as u64) {
            return Err(FetchError::TooLarge);
        }
        let mut body = response.into_body();
        let mut file = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| FetchError::Protocol)?;
            if let Some(data) = frame.data_ref() {
                if file.len().saturating_add(data.len()) > max_bytes {
                    return Err(FetchError::TooLarge);
                }
                file.extend_from_slice(data);
            }
        }
        // A body shorter than it declared is a truncated one.
        if declared.is_some_and(|n| n != file.len() as u64) {
            return Err(FetchError::Protocol);
        }
        Ok(file)
    }
    .await;
    result
}

/// Aborts the connection's task when dropped: on the way out, and when the
/// time limit drops the whole fetch half way (a silent CDN must not keep
/// its socket for as long as it likes).
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[async_trait]
impl CdnFetcher for HyperFetcher {
    async fn get(
        &self,
        target: &Pinned,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, FetchError> {
        tokio::time::timeout(timeout, self.fetch(target, max_bytes))
            .await
            .map_err(|_| FetchError::Timeout)?
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code: a panic is the report
mod tests {
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use meta_whatsapp_server_core::webhooks_out::destination::DestinationPolicy;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    use super::*;

    /// What a receiver got: each request's head, as text.
    type Seen = Arc<Mutex<Vec<String>>>;

    /// A plain HTTP receiver on 127.0.0.1 writing `answer` to every request
    /// then closing; with `hold`, it keeps the connection open and silent
    /// after the head.
    async fn receiver(answer: Vec<u8>, hold: bool) -> (SocketAddr, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Seen = Arc::default();
        let s = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let (s, answer) = (s.clone(), answer.clone());
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|x| x == b"\r\n\r\n") {
                        match tcp.read(&mut buf).await {
                            Ok(n) if n > 0 => head.extend_from_slice(&buf[..n]),
                            _ => return,
                        }
                    }
                    s.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&head).into_owned());
                    if hold {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        return;
                    }
                    let _ = tcp.write_all(&answer).await;
                    let _ = tcp.shutdown().await;
                });
            }
        });
        (addr, seen)
    }

    /// `media.test` resolves nowhere: the fetch can only reach the pinned
    /// address.
    fn pinned(path: &str, address: SocketAddr) -> Pinned {
        let policy = DestinationPolicy::parse("media.test", true).unwrap();
        Pinned {
            destination: policy
                .check_url(&format!("http://media.test:{}{path}", address.port()))
                .unwrap(),
            address,
        }
    }

    async fn get(answer: Vec<u8>, max: usize) -> (Result<Vec<u8>, FetchError>, Seen) {
        let (addr, seen) = receiver(answer, false).await;
        let got = HyperFetcher::new()
            .unwrap()
            .get(&pinned("/v/f?oh=TOKEN", addr), max, Duration::from_secs(5))
            .await;
        (got, seen)
    }

    /// A plain `GET` to the pinned address: the URL's host is only the
    /// `Host` header, the path and query are the request line's, and the
    /// request carries no credential (no `Authorization`, no `Cookie`).
    #[tokio::test]
    async fn the_file_is_fetched_from_the_pinned_address_with_no_credential() {
        let (got, seen) = get(
            b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello".to_vec(),
            1024,
        )
        .await;
        assert_eq!(got.unwrap(), b"hello");
        let seen = seen.lock().unwrap();
        let [head] = &seen[..] else {
            panic!("{seen:?}")
        };
        let lower = head.to_ascii_lowercase();
        assert!(head.starts_with("GET /v/f?oh=TOKEN HTTP/1.1\r\n"), "{head}");
        assert!(lower.contains("\r\nhost: media.test:"), "{head}");
        for header in ["authorization", "cookie", "proxy-authorization", "referer"] {
            assert!(!lower.contains(header), "{header} was sent: {head}");
        }
    }

    /// **A redirect is never followed.** The first receiver redirects to
    /// the second; the second is never contacted, and the fetch is the
    /// status. Delete the status check and the redirect's empty body is
    /// the "file".
    #[tokio::test]
    async fn a_redirect_is_a_failure_and_its_target_is_never_asked() {
        let (target, target_seen) = receiver(
            b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nevil".to_vec(),
            false,
        )
        .await;
        for status in [
            "301 Moved Permanently",
            "302 Found",
            "307 Temporary Redirect",
        ] {
            let redirect = format!(
                "HTTP/1.1 {status}\r\nlocation: http://{target}/stolen\r\ncontent-length: 0\r\n\r\n"
            );
            let (got, _) = get(redirect.into_bytes(), 1024).await;
            assert_eq!(
                got.unwrap_err(),
                FetchError::Status(status[..3].parse().unwrap())
            );
        }
        assert!(
            target_seen.lock().unwrap().is_empty(),
            "the target was asked"
        );
    }

    #[tokio::test]
    async fn anything_but_a_200_is_a_failure() {
        for code in [201u16, 204, 206, 403, 404, 500] {
            let (got, _) = get(
                format!("HTTP/1.1 {code} X\r\ncontent-length: 2\r\n\r\nok").into_bytes(),
                1024,
            )
            .await;
            assert_eq!(got.unwrap_err(), FetchError::Status(code));
        }
    }

    /// The bound: a declared length past it is refused before a byte of the
    /// body is read, and an undeclared (chunked) body is cut when it passes
    /// it. Exactly the bound is accepted.
    #[tokio::test]
    async fn the_body_is_bounded() {
        let (got, _) = get(
            b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\n\r\nhello world".to_vec(),
            10,
        )
        .await;
        assert_eq!(got.unwrap_err(), FetchError::TooLarge);
        let (got, _) = get(
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n6\r\nhello \r\n5\r\nworld\r\n0\r\n\r\n"
                .to_vec(),
            10,
        )
        .await;
        assert_eq!(got.unwrap_err(), FetchError::TooLarge);
        let (got, _) = get(
            b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\nhelloworld".to_vec(),
            10,
        )
        .await;
        assert_eq!(got.unwrap(), b"helloworld");
    }

    #[tokio::test]
    async fn a_truncated_body_is_a_failure() {
        let (got, _) = get(
            b"HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\nhello".to_vec(),
            1024,
        )
        .await;
        assert_eq!(got.unwrap_err(), FetchError::Protocol);
    }

    /// **The bound is enforced while streaming**, not after the body is in
    /// memory: a CDN that never stops (no length, an endless body) is cut
    /// at the bound with `TooLarge`, long before the time limit, and its
    /// connection is closed. Move the check after the loop and this fails
    /// with `Timeout` (the body never ends) having buffered all it got.
    #[tokio::test]
    async fn an_endless_body_is_cut_at_the_bound_while_it_streams() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = tcp.read(&mut buf).await;
            let _ = tcp.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await;
            let chunk = vec![b'z'; 16 * 1024];
            let mut sent = 0usize;
            // Until the fetcher hangs up (bounded, so a failure is a
            // failure and not a hang).
            while sent < 256 * 1024 * 1024 && tcp.write_all(&chunk).await.is_ok() {
                sent += chunk.len();
            }
            let _ = done_tx.send(sent);
        });
        let got = HyperFetcher::new()
            .unwrap()
            .get(&pinned("/v/f", addr), 1024 * 1024, Duration::from_secs(20))
            .await;
        assert_eq!(got.unwrap_err(), FetchError::TooLarge);
        let sent = tokio::time::timeout(Duration::from_secs(10), done_rx)
            .await
            .expect("the connection was left open")
            .unwrap();
        // The socket buffers hold some megabytes; far less than the cap.
        assert!(sent < 128 * 1024 * 1024, "{sent}");
    }

    /// **No decompression.** The request asks for `identity`; a CDN that
    /// answers `Content-Encoding: gzip` anyway gets its bytes returned as
    /// they came (they then fail the file hash), never expanded: a
    /// gzip bomb costs the bound, not its expansion.
    #[tokio::test]
    async fn a_content_encoding_is_never_expanded() {
        let gz = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 0xcb, 0x48, 0x2d, 0, 0];
        let mut answer =
            b"HTTP/1.1 200 OK\r\ncontent-encoding: gzip\r\ncontent-length: 15\r\n\r\n".to_vec();
        answer.extend_from_slice(&gz);
        let (got, seen) = get(answer, 1024).await;
        assert_eq!(got.unwrap(), gz);
        let head = seen.lock().unwrap()[0].to_ascii_lowercase();
        assert!(head.contains("\r\naccept-encoding: identity\r\n"), "{head}");
    }

    /// An answer's head past the buffer is a failure, not an allocation.
    #[tokio::test]
    async fn an_oversized_head_is_refused() {
        let mut answer = b"HTTP/1.1 200 OK\r\nx-pad: ".to_vec();
        answer.extend(std::iter::repeat_n(b'a', 64 * 1024));
        answer.extend_from_slice(b"\r\ncontent-length: 0\r\n\r\n");
        let (got, _) = get(answer, 1024).await;
        assert_eq!(got.unwrap_err(), FetchError::Protocol);
    }

    /// The time limit is for the whole fetch: a CDN that sends one byte at
    /// a time (a slow loris) is cut at the limit, and its connection is
    /// closed by the fetcher, not held until the CDN gives up.
    #[tokio::test]
    async fn a_trickling_cdn_is_cut_at_the_limit_and_its_connection_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = tcp.read(&mut buf).await;
            let _ = tcp
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\n")
                .await;
            // One byte at a time until the peer is gone.
            loop {
                if tcp.write_all(b"x").await.is_err() || tcp.flush().await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let _ = closed_tx.send(());
        });
        let started = std::time::Instant::now();
        let got = HyperFetcher::new()
            .unwrap()
            .get(&pinned("/v/f", addr), 1024, Duration::from_millis(300))
            .await;
        assert_eq!(got.unwrap_err(), FetchError::Timeout);
        assert!(started.elapsed() < Duration::from_secs(3));
        tokio::time::timeout(Duration::from_secs(5), closed_rx)
            .await
            .expect("the connection was left open")
            .unwrap();
    }

    /// TLS is by the host's name at the pinned address: the certificate
    /// must be for the name (SNI is the name), a certificate for another
    /// name or of an untrusted issuer is `Tls` and no request is sent.
    #[tokio::test]
    async fn tls_checks_the_certificate_against_the_name_at_the_pinned_address() {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let make = |name: &str| {
            let key = rcgen::KeyPair::generate().unwrap();
            let cert = rcgen::CertificateParams::new(vec![name.to_owned()])
                .unwrap()
                .self_signed(&key)
                .unwrap();
            (cert.der().clone(), key.serialize_der())
        };
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
            let seen: Seen = Arc::default();
            let s = seen.clone();
            tokio::spawn(async move {
                while let Ok((tcp, _)) = listener.accept().await {
                    let (acceptor, s) = (acceptor.clone(), s.clone());
                    tokio::spawn(async move {
                        let Ok(mut tls) = acceptor.accept(tcp).await else {
                            return;
                        };
                        let sni = tls.get_ref().1.server_name().map(str::to_owned);
                        let mut buf = vec![0u8; 4096];
                        let n = tls.read(&mut buf).await.unwrap_or(0);
                        s.lock()
                            .unwrap()
                            .push(format!("{sni:?} {}", String::from_utf8_lossy(&buf[..n])));
                        let _ = tls
                            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                            .await;
                        let _ = tls.shutdown().await;
                    });
                }
            });
            (addr, seen)
        };
        let tls_pinned = |address: SocketAddr| {
            let policy = DestinationPolicy::parse("media.test", false).unwrap();
            Pinned {
                destination: policy.check_url("https://media.test/v/f").unwrap(),
                address,
            }
        };
        let (good, good_key) = make("media.test");
        let (other, other_key) = make("other.test");
        let mut roots = RootCertStore::empty();
        roots.add(good.clone()).unwrap();
        roots.add(other.clone()).unwrap();
        let fetcher = HyperFetcher::with_roots(roots).unwrap();
        let timeout = Duration::from_secs(5);
        let (addr, seen) = serve(good, good_key).await;
        assert_eq!(
            fetcher.get(&tls_pinned(addr), 1024, timeout).await.unwrap(),
            b"ok"
        );
        let head = seen.lock().unwrap()[0].clone();
        assert!(
            head.starts_with("Some(\"media.test\") GET /v/f HTTP/1.1"),
            "{head}"
        );
        let (wrong, wrong_seen) = serve(other, other_key).await;
        assert_eq!(
            fetcher.get(&tls_pinned(wrong), 1024, timeout).await,
            Err(FetchError::Tls),
            "a certificate for another name"
        );
        assert!(wrong_seen.lock().unwrap().is_empty());
        let untrusting = HyperFetcher::with_roots(RootCertStore::empty()).unwrap();
        assert_eq!(
            untrusting.get(&tls_pinned(addr), 1024, timeout).await,
            Err(FetchError::Tls),
            "an untrusted issuer"
        );
    }

    #[tokio::test]
    async fn a_silent_cdn_times_out_and_nothing_listening_is_a_connect_failure() {
        let (addr, _) = receiver(Vec::new(), true).await;
        let got = HyperFetcher::new()
            .unwrap()
            .get(&pinned("/v/f", addr), 1024, Duration::from_millis(200))
            .await;
        assert_eq!(got.unwrap_err(), FetchError::Timeout);
        let closed = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap()
        };
        let got = HyperFetcher::new()
            .unwrap()
            .get(&pinned("/v/f", closed), 1024, Duration::from_secs(5))
            .await;
        assert_eq!(got.unwrap_err(), FetchError::Connect);
    }
}
