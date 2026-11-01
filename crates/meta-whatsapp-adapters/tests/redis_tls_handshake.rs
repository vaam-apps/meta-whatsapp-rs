//! `RedisTls` against an in-process TLS server (roadmap L21c): what the
//! live tests of `tests/redis_tls.rs` cannot set up on one shared Redis. The
//! server name is checked (a DNS name against DNS SANs, an IP address
//! against IP SANs only), an untrusted issuer is refused, the connection
//! and the handshake are bounded by the connection timeout, a reconnect
//! sends `AUTH` and `SELECT` again and replays no command, concurrent
//! commands share one failed reconnect attempt, and a refused `AUTH` does
//! not quote the password.
//!
//! The server speaks just enough RESP2 for redis-rs's connection setup and
//! a few commands; its certificates are made per test by rcgen (none is
//! committed). Like `tests/redis_tls.rs`, this binary links both rustls
//! providers and installs no process default.
#![cfg(feature = "redis")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use meta_whatsapp_adapters::store::{RedisTls, RedisTlsConnection};
use rcgen::{CertificateParams, DnType, KeyPair};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A certificate and its key, self-signed: trusted as its own root.
struct Identity {
    pem: String,
    der: CertificateDer<'static>,
    key: Vec<u8>,
}

/// A self-signed certificate for `names` (an IP address becomes an IP SAN,
/// anything else a DNS SAN).
fn identity(names: &[&str]) -> Identity {
    // A subject of its own: two identities never pass for each other's issuer.
    static SERIAL: AtomicUsize = AtomicUsize::new(0);
    let key = KeyPair::generate().unwrap();
    let mut params =
        CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>()).unwrap();
    params.distinguished_name.push(
        DnType::CommonName,
        format!("TEST-ONLY {}", SERIAL.fetch_add(1, Ordering::Relaxed)),
    );
    let cert = params.self_signed(&key).unwrap();
    Identity {
        pem: cert.pem(),
        der: cert.der().clone(),
        key: key.serialize_der(),
    }
}

/// A client that trusts `root` only.
fn trusting(root: &Identity) -> RedisTls {
    RedisTls::builder()
        .root_certificates_pem(root.pem.clone())
        .build()
        .unwrap()
}

/// What the server does with the next connection it accepts.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// The TLS handshake, then RESP: `+OK` to everything but `PING`
    /// (`+PONG`) and, when `reject_auth`, `AUTH` (`-WRONGPASS`); `DROPME`
    /// answers `+OK` and closes the connection.
    Serve { reject_auth: bool },
    /// Accept the TCP connection, wait, close it: a failed attempt.
    Refuse(Duration),
    /// Accept the TCP connection and never answer: the handshake hangs.
    StallHandshake,
    /// The handshake, then never answer: redis-rs's setup hangs.
    StallSetup,
}

/// An in-process TLS "Redis" on 127.0.0.1.
struct Server {
    addr: SocketAddr,
    mode: Arc<Mutex<Mode>>,
    accepted: Arc<AtomicUsize>,
    /// The commands each connection that finished its handshake received,
    /// in accept order.
    commands: Arc<Mutex<Vec<Vec<Vec<String>>>>>,
}

impl Server {
    async fn start(identity: &Identity) -> Self {
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![identity.der.clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone())),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Self {
            addr: listener.local_addr().unwrap(),
            mode: Arc::new(Mutex::new(Mode::Serve { reject_auth: false })),
            accepted: Arc::new(AtomicUsize::new(0)),
            commands: Arc::new(Mutex::new(Vec::new())),
        };
        let (mode, accepted, commands) = (
            Arc::clone(&server.mode),
            Arc::clone(&server.accepted),
            Arc::clone(&server.commands),
        );
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                accepted.fetch_add(1, Ordering::SeqCst);
                let mode = *mode.lock().unwrap();
                let (acceptor, commands) = (acceptor.clone(), Arc::clone(&commands));
                tokio::spawn(async move {
                    let reject_auth = match mode {
                        Mode::Refuse(delay) => {
                            tokio::time::sleep(delay).await;
                            return;
                        }
                        Mode::StallHandshake => {
                            tokio::time::sleep(Duration::from_secs(3600)).await;
                            drop(tcp);
                            return;
                        }
                        Mode::StallSetup => {
                            let tls = acceptor.accept(tcp).await;
                            tokio::time::sleep(Duration::from_secs(3600)).await;
                            drop(tls);
                            return;
                        }
                        Mode::Serve { reject_auth } => reject_auth,
                    };
                    // A refused handshake (the client's check failed) ends here.
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let index = {
                        let mut all = commands.lock().unwrap();
                        all.push(Vec::new());
                        all.len() - 1
                    };
                    let (read, mut write) = tokio::io::split(tls);
                    let mut read = BufReader::new(read);
                    while let Some(command) = read_command(&mut read).await {
                        let name = command[0].to_ascii_uppercase();
                        commands.lock().unwrap()[index].push(command);
                        let reply: &[u8] = match name.as_str() {
                            "AUTH" if reject_auth => {
                                b"-WRONGPASS invalid username-password pair or user is disabled.\r\n"
                            }
                            "PING" => b"+PONG\r\n",
                            _ => b"+OK\r\n",
                        };
                        if write.write_all(reply).await.is_err() || write.flush().await.is_err() {
                            return;
                        }
                        if name == "DROPME" {
                            let _ = write.shutdown().await;
                            return;
                        }
                    }
                });
            }
        });
        server
    }

    fn set_mode(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// The commands connection `index` received, as `NAME arg …` lines.
    fn commands(&self, index: usize) -> Vec<String> {
        self.commands.lock().unwrap()[index]
            .iter()
            .map(|c| c.join(" "))
            .collect()
    }

    fn sessions(&self) -> usize {
        self.commands.lock().unwrap().len()
    }

    fn url(&self, host: &str, rest: &str) -> String {
        format!("rediss://{rest}{host}:{}", self.addr.port())
    }
}

/// One RESP2 command (an array of bulk strings), or `None` at the end.
async fn read_command<R: AsyncBufRead + Unpin>(read: &mut R) -> Option<Vec<String>> {
    let mut line = String::new();
    if read.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let n: usize = line.trim_end().strip_prefix('*')?.parse().ok()?;
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        line.clear();
        read.read_line(&mut line).await.ok()?;
        let len: usize = line.trim_end().strip_prefix('$')?.parse().ok()?;
        let mut bytes = vec![0; len + 2];
        read.read_exact(&mut bytes).await.ok()?;
        bytes.truncate(len);
        args.push(String::from_utf8(bytes).ok()?);
    }
    Some(args)
}

async fn ping(conn: &RedisTlsConnection) -> redis::RedisResult<String> {
    redis::cmd("PING").query_async(&mut conn.clone()).await
}

async fn echo(conn: &RedisTlsConnection, what: &str) -> redis::RedisResult<redis::Value> {
    redis::cmd("ECHO")
        .arg(what)
        .query_async(&mut conn.clone())
        .await
}

/// Closes the server's side of `conn`'s connection, then sends `PING`s
/// until one fails as a lost connection does: the next command reconnects.
async fn lose_the_connection(conn: &RedisTlsConnection) {
    redis::cmd("DROPME")
        .query_async::<redis::Value>(&mut conn.clone())
        .await
        .unwrap();
    for _ in 0..5 {
        match ping(conn).await {
            Err(e) if e.is_unrecoverable_error() => return,
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    panic!("the closed connection never failed a command");
}

fn assert_refused(error: &redis::RedisError, why: &str) {
    assert!(
        error.to_string().contains(why),
        "expected the handshake to fail with {why}: {error}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_certificate_naming_the_host_connects() {
    assert!(CryptoProvider::get_default().is_none());
    let id = identity(&["localhost"]);
    let server = Server::start(&id).await;
    let conn = trusting(&id)
        .connect(&server.url("localhost", ""))
        .await
        .unwrap();
    assert_eq!(ping(&conn).await.unwrap(), "PONG");
    assert!(CryptoProvider::get_default().is_none());
}

/// The decisive test of hostname verification: the certificate is trusted,
/// but names another host.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_certificate_for_another_name_is_refused() {
    let id = identity(&["redis.example.com"]);
    let server = Server::start(&id).await;
    let tls = trusting(&id);
    for host in ["localhost", "127.0.0.1"] {
        let error = tls.connect(&server.url(host, "")).await.unwrap_err();
        assert_refused(&error, "not valid for name");
    }
    assert_eq!(server.sessions(), 0, "no handshake completed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ip_host_is_checked_against_ip_sans_only() {
    // A DNS SAN does not cover the IP address it resolves to.
    let dns = identity(&["localhost"]);
    let server = Server::start(&dns).await;
    let error = trusting(&dns)
        .connect(&server.url("127.0.0.1", ""))
        .await
        .unwrap_err();
    assert_refused(&error, "not valid for name");

    // An IP SAN covers that address, and not a name resolving to it.
    let ip = identity(&["127.0.0.1"]);
    let server = Server::start(&ip).await;
    let tls = trusting(&ip);
    let conn = tls.connect(&server.url("127.0.0.1", "")).await.unwrap();
    assert_eq!(ping(&conn).await.unwrap(), "PONG");
    let error = tls.connect(&server.url("localhost", "")).await.unwrap_err();
    assert_refused(&error, "not valid for name");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_certificate_from_an_untrusted_issuer_is_refused() {
    let server = Server::start(&identity(&["localhost"])).await;
    let error = trusting(&identity(&["localhost"]))
        .connect(&server.url("localhost", ""))
        .await
        .unwrap_err();
    assert_refused(&error, "UnknownIssuer");
    assert_eq!(server.sessions(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_connection_timeout_bounds_the_handshake_and_the_setup() {
    let id = identity(&["localhost"]);
    let server = Server::start(&id).await;
    let tls = trusting(&id).with_connection_timeout(Some(Duration::from_millis(200)));
    for mode in [Mode::StallHandshake, Mode::StallSetup] {
        server.set_mode(mode);
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            tls.connect(&server.url("localhost", "")),
        )
        .await
        .unwrap_or_else(|_| panic!("{mode:?}: connect was not bounded by its timeout"));
        let error = outcome.unwrap_err();
        assert!(error.is_timeout(), "{mode:?}: {error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reconnect_authenticates_and_selects_again_and_replays_nothing() {
    let id = identity(&["localhost"]);
    let server = Server::start(&id).await;
    let conn = trusting(&id)
        .connect(&format!("{}/3", server.url("localhost", ":hunter2@")))
        .await
        .unwrap();
    let setup = |index: usize| -> Vec<String> {
        server
            .commands(index)
            .into_iter()
            .filter(|c| c.starts_with("AUTH") || c.starts_with("SELECT"))
            .collect()
    };
    assert_eq!(setup(0), ["AUTH hunter2", "SELECT 3"]);

    lose_the_connection(&conn).await;
    // The command that met the dead connection is not replayed on the new
    // one: a write whose reply was lost may have happened.
    let before = server.sessions();
    assert_eq!(before, 1);
    echo(&conn, "after").await.unwrap();
    assert_eq!(server.sessions(), 2);
    assert_eq!(setup(1), ["AUTH hunter2", "SELECT 3"]);
    let replayed: Vec<String> = server
        .commands(1)
        .into_iter()
        .filter(|c| c.starts_with("PING") || c.starts_with("DROPME"))
        .collect();
    assert!(
        replayed.is_empty(),
        "replayed on the new connection: {replayed:?}"
    );
    assert!(server.commands(1).contains(&"ECHO after".to_owned()));
}

/// Commands that find the connection gone while a reconnect attempt runs
/// wait for it and share its failure, rather than each making its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_commands_share_one_failed_reconnect_attempt() {
    let id = identity(&["localhost"]);
    let server = Server::start(&id).await;
    let conn = trusting(&id)
        .connect(&server.url("localhost", ""))
        .await
        .unwrap();
    // Attempts from now on fail, slowly enough for every caller to queue.
    server.set_mode(Mode::Refuse(Duration::from_millis(400)));
    lose_the_connection(&conn).await;
    assert_eq!(
        server.accepted(),
        1,
        "losing a connection does not reconnect by itself"
    );

    let callers: Vec<_> = (0..8)
        .map(|_| {
            let conn = conn.clone();
            tokio::spawn(async move { ping(&conn).await })
        })
        .collect();
    for caller in callers {
        assert!(caller.await.unwrap().is_err(), "no server accepted it");
    }
    assert_eq!(server.accepted(), 2, "eight callers, one attempt");

    // The next command, after that attempt, makes its own.
    server.set_mode(Mode::Serve { reject_auth: false });
    assert_eq!(ping(&conn).await.unwrap(), "PONG");
    assert_eq!(server.accepted(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_password_is_not_quoted() {
    let id = identity(&["localhost"]);
    let server = Server::start(&id).await;
    server.set_mode(Mode::Serve { reject_auth: true });
    let error = trusting(&id)
        .connect(&server.url("localhost", "alice:hunter2@"))
        .await
        .unwrap_err();
    assert_eq!(server.commands(0)[0], "AUTH alice hunter2");
    for shown in [error.to_string(), format!("{error:?}")] {
        assert!(!shown.contains("hunter2"), "{shown}");
    }
}
