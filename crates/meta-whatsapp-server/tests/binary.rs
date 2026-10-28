//! The real binary: `serve` starts both listeners and stops on `SIGTERM`,
//! `healthcheck` reaches the internal one, `serve` applies the configured
//! settings, a refused configuration exits without quoting the value, and
//! (live) the CLI's first admin key works against the served API.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_meta-whatsapp-server");

/// A spawned `meta-whatsapp-server`, killed and reaped when dropped: a
/// test that fails before [`terminate`] leaves no server behind, holding
/// its ports and, on Postgres, its connections and the housekeeping lock.
struct Served(Child);

impl std::ops::Deref for Served {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Served {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        // After `terminate` the child is reaped already, and both are
        // no-ops (std sends no signal to a reaped child).
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `command`, spawned, killed when the guard drops.
fn spawn(command: &mut Command) -> Served {
    Served(command.spawn().unwrap())
}

/// The ports of this file's tests. [`two_ports`] frees the two it finds
/// for the server it is about to start, and until that server binds them
/// they are free for any bind on port 0: another test's `two_ports`, run
/// at once, could be handed one, and of the two servers given it, the
/// one that lost the race exited while its test, having read its
/// development admin key (printed before binding), went on to talk to
/// the winner (CI on PR #47: `/v1/admin/tenants` answered 404, on the
/// other server's public listener). So `two_ports` holds this lock until
/// the server on its ports says it listens on both ([`serve`]), and a
/// test that checks its stopped server's ports refuse holds it from the
/// stop to the check. Every bind on port 0 here is under it.
static PORTS: Mutex<()> = Mutex::new(());

/// [`PORTS`], held. A test failing while holding it poisons it, which
/// says nothing about the ports.
fn hold_ports() -> MutexGuard<'static, ()> {
    PORTS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Two free local ports, and [`PORTS`] held until the server given them
/// listens ([`serve`] releases it) or, for a command that does not serve,
/// until this is dropped.
struct Ports {
    public: SocketAddr,
    internal: SocketAddr,
    held: MutexGuard<'static, ()>,
}

/// Two free local ports, [`PORTS`] held.
fn two_ports() -> Ports {
    let held = hold_ports();
    let a = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = TcpListener::bind("127.0.0.1:0").unwrap();
    Ports {
        public: a.local_addr().unwrap(),
        internal: b.local_addr().unwrap(),
        held,
    }
}

/// A `meta-whatsapp-server serve` that said it listens on its two ports:
/// they are its own until it stops. Its standard error is read to its end
/// (so it never blocks on it), and shown when the test fails.
struct Server {
    child: Served,
    /// The development admin key it printed on a plain line of standard
    /// error (memory storage), if any.
    admin_key: Option<String>,
    stderr: Arc<Mutex<Vec<String>>>,
    reader: Option<JoinHandle<()>>,
}

/// Start `command` (a `serve` on `ports`, from [`base`]) and wait until it
/// says it listens on both ports (its `listening` line, logged once both
/// are bound), then release [`PORTS`]. A server that exits first (one of
/// its ports taken, say) or never says it fails the test with its
/// standard error, rather than leaving the test to talk to whatever holds
/// the port.
fn serve(command: &mut Command, ports: Ports) -> Server {
    let mut child = spawn(command);
    let stderr = child.stderr.take().expect("serve's standard error piped");
    let lines = Arc::new(Mutex::new(Vec::new()));
    let (seen, next) = mpsc::channel();
    let reader = {
        let lines = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                lines.lock().unwrap().push(line.clone());
                let _ = seen.send(line);
            }
        })
    };
    let (public, internal) = (ports.public.to_string(), ports.internal.to_string());
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut admin_key = None;
    loop {
        match next.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => {
                // The key's own line, never a JSON log event.
                if serde_json::from_str::<serde_json::Value>(&line).is_err()
                    && let Some((_, key)) = line.split_once("shown once: ")
                {
                    admin_key = Some(key.trim().to_owned());
                }
                if line.contains("listening") && line.contains(&public) && line.contains(&internal)
                {
                    break;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                reader.join().unwrap();
                let status = child.wait().unwrap();
                panic!(
                    "the service exited ({status}) before listening on {public} and {internal}:\n{}",
                    lines.lock().unwrap().join("\n")
                );
            }
            Err(RecvTimeoutError::Timeout) => panic!(
                "the service never said it listens on {public} and {internal}:\n{}",
                lines.lock().unwrap().join("\n")
            ),
        }
    }
    drop(ports.held);
    Server {
        child,
        admin_key,
        stderr: lines,
        reader: Some(reader),
    }
}

impl Server {
    /// Its standard error so far.
    fn stderr(&self) -> String {
        // Also read while the test panics (`Drop`): never panic here.
        self.stderr
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .join("\n")
    }

    /// Fail the test, with its standard error, if it has exited.
    fn assert_running(&mut self) {
        if let Some(status) = self.child.try_wait().unwrap() {
            panic!("the service exited ({status}):\n{}", self.stderr());
        }
    }

    /// Wait until `GET /livez` answers 200 on `addr`, from this server.
    fn wait_live(&mut self, addr: SocketAddr) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            self.assert_running();
            if http(addr, "GET", "/livez", &[], "").is_some_and(|(s, _)| s == 200) {
                // Not another process on a port this one let go of.
                self.assert_running();
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("{addr} never answered /livez:\n{}", self.stderr());
    }

    /// Stop it with `SIGTERM`, check it exits cleanly, and return its whole
    /// standard error.
    fn terminate(&mut self) -> Vec<String> {
        self.assert_running();
        terminate(&mut self.child);
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
        self.stderr.lock().unwrap().clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("the service's standard error:\n{}", self.stderr());
        }
    }
}

/// `method path` over plain HTTP/1.1: the status and the body.
fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (name, value) in headers {
        write!(request, "{name}: {value}\r\n").unwrap();
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).ok()?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).ok()?;
    let status = answer.get(9..12)?.parse().ok()?;
    let body = answer
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    Some((status, body))
}

fn terminate(child: &mut Child) {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "exit after SIGTERM: {status}");
            return;
        }
        assert!(
            Instant::now() < deadline,
            "still running 30 s after SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn base(command: &mut Command, public: SocketAddr, internal: SocketAddr) -> &mut Command {
    command
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("WA_APP_SECRET", "app-secret-for-the-binary-test")
        .env("WA_VERIFY_TOKEN", "verify-token-for-the-binary-test")
        .env("WA_SERVER_PUBLIC_BIND", public.to_string())
        .env("WA_SERVER_INTERNAL_BIND", internal.to_string())
        .env("WA_SERVER_LOG_FORMAT", "text")
        // `serve`'s `listening` line is info: [`serve`] waits for it.
        .env("RUST_LOG", "warn,meta_whatsapp_server::serve=info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
}

#[test]
fn serve_answers_on_both_listeners_and_stops_on_sigterm() {
    let ports = two_ports();
    let (public, internal) = (ports.public, ports.internal);
    let mut server = serve(
        base(&mut Command::new(BIN), public, internal)
            .env("WA_SERVER_ENV", "development")
            .arg("serve"),
        ports,
    );
    server.wait_live(internal);
    server.wait_live(public);
    let (status, spec) = http(internal, "GET", "/v1/openapi.json", &[], "").unwrap();
    assert_eq!(status, 200);
    assert_eq!(spec, include_str!("../openapi/v1.json"));
    // Only Meta's check and /livez on the public listener.
    assert_eq!(
        http(public, "GET", "/v1/openapi.json", &[], "").unwrap().0,
        404
    );
    let (status, challenge) = http(
        public,
        "GET",
        "/webhooks/meta?hub.mode=subscribe&hub.challenge=42&hub.verify_token=verify-token-for-the-binary-test",
        &[],
        "",
    )
    .unwrap();
    assert_eq!((status, challenge.as_str()), (200, "42"));
    let health = Command::new(BIN)
        .env_clear()
        .env("WA_SERVER_INTERNAL_BIND", internal.to_string())
        .arg("healthcheck")
        .status()
        .unwrap();
    assert!(health.success(), "healthcheck against a live service");
    // Until the check below, no other test's server is given the port.
    let _held = hold_ports();
    server.terminate();
    let down = Command::new(BIN)
        .env_clear()
        .env("WA_SERVER_INTERNAL_BIND", internal.to_string())
        .arg("healthcheck")
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!down.success(), "healthcheck against a stopped service");
}

/// A test that fails while its server is up leaves none running: the
/// guard kills and reaps it as the panic unwinds, so it answers no more.
/// Decisive: `Served`'s `Drop`.
#[test]
fn a_failing_test_leaves_no_server_running() {
    let ports = two_ports();
    let (public, internal) = (ports.public, ports.internal);
    // Taken before the server is killed, kept past the check: no other
    // test's server is given its port meanwhile.
    let mut held = None;
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut server = serve(
            base(&mut Command::new(BIN), public, internal)
                .env("WA_SERVER_ENV", "development")
                .arg("serve"),
            ports,
        );
        server.wait_live(internal);
        held = Some(hold_ports());
        panic!("deliberate: a test failing while its server is up");
    }));
    assert!(failed.is_err(), "the scope did fail");
    assert!(held.is_some(), "failed while its server was up");
    assert!(
        http(internal, "GET", "/livez", &[], "").is_none(),
        "the server still answers after its test failed"
    );
}

/// A server whose port is taken when it starts fails its test, with why
/// in its standard error, rather than leaving the test to talk to what
/// holds the port: here a listener of this test's on the internal port
/// (on PR #47's CI, a neighbouring test's server on its public one). The
/// server prints its development admin key before binding, so the key
/// alone proves nothing. Decisive: [`serve`] waiting for the `listening`
/// line naming both ports (going on once the key is out, as the
/// rate-limit test did, serves on the taken port).
#[test]
fn a_server_that_cannot_listen_fails_its_test_with_its_standard_error() {
    let ports = two_ports();
    let (public, internal) = (ports.public, ports.internal);
    let taken = TcpListener::bind(internal).unwrap();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        serve(
            base(&mut Command::new(BIN), public, internal)
                .env("WA_SERVER_ENV", "development")
                .arg("serve"),
            ports,
        )
    }));
    drop(taken);
    let Err(failed) = failed else {
        panic!("served on a port that was taken");
    };
    let message = failed
        .downcast_ref::<String>()
        .expect("a formatted panic message");
    assert!(
        message.contains("the service exited")
            && message.contains("shown once: wak_")
            && message.contains(&format!(
                "cannot listen on WA_SERVER_INTERNAL_BIND {internal}"
            )),
        "{message}"
    );
}

/// In development on memory storage (which the CLI cannot reach), `serve`
/// writes a one-time admin key to standard error, which the served API
/// accepts (coordinator's decision DP9). The key is a plain line of its
/// own, never a log event: with JSON logs (the default format) on the same
/// stream, no log line holds it, so a log pipeline never ships it.
/// Decisive: minting it at start, and `eprintln!` rather than `tracing`.
#[test]
fn development_on_memory_prints_a_one_time_admin_key() {
    let ports = two_ports();
    let (public, internal) = (ports.public, ports.internal);
    let mut server = serve(
        base(&mut Command::new(BIN), public, internal)
            .env("WA_SERVER_ENV", "development")
            .env("WA_SERVER_LOG_FORMAT", "json")
            .arg("serve"),
        ports,
    );
    let key = server
        .admin_key
        .clone()
        .expect("no admin key on a plain line of standard error");
    assert!(
        key.starts_with("wak_") && !key.contains(char::is_whitespace),
        "{key}"
    );
    let bearer = format!("Bearer {key}");
    let (status, body) = http(
        internal,
        "POST",
        "/v1/admin/tenants",
        &[
            ("Authorization", &bearer),
            ("Content-Type", "application/json"),
        ],
        r#"{"id": "merchant-42"}"#,
    )
    .expect("the service never answered");
    assert_eq!(status, 201, "{body}");
    let lines = server.terminate();
    let secret = key.rsplit('_').next().unwrap();
    let (logs, plain): (Vec<&String>, Vec<&String>) = lines
        .iter()
        .partition(|line| serde_json::from_str::<serde_json::Value>(line).is_ok());
    assert!(
        logs.iter()
            .any(|line| line.contains("memory storage: everything is lost")),
        "the JSON logs were captured: {lines:#?}"
    );
    for line in &logs {
        assert!(
            !line.contains("wak_") && !line.contains(secret),
            "a log line holds the admin key: {line}"
        );
    }
    let holding: Vec<&&String> = plain.iter().filter(|l| l.contains(secret)).collect();
    assert_eq!(holding.len(), 1, "{lines:#?}");
    assert!(
        holding[0].starts_with("meta-whatsapp-server: development, memory storage: "),
        "{}",
        holding[0]
    );
}

/// Both listeners of the served binary cut off a client whose request
/// head does not arrive within 10 s (`listen::PUBLIC_LIMITS` and
/// `INTERNAL_LIMITS`, the header read timeout; hyper's own default is 30
/// s). Decisive: `serve` passing the listeners their limits.
#[test]
fn serve_cuts_off_a_slow_request_head_on_both_listeners() {
    let ports = two_ports();
    let (public, internal) = (ports.public, ports.internal);
    let mut server = serve(
        base(&mut Command::new(BIN), public, internal)
            .env("WA_SERVER_ENV", "development")
            .arg("serve"),
        ports,
    );
    server.wait_live(internal);
    server.wait_live(public);
    let slow = |addr: SocketAddr| {
        std::thread::spawn(move || {
            let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
            let started = Instant::now();
            stream
                .write_all(b"GET /livez HTTP/1.1\r\nHost: localhost\r\nX-Slow: ")
                .unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(40)))
                .unwrap();
            let mut answer = Vec::new();
            let closed = stream.read_to_end(&mut answer).is_ok();
            (closed, started.elapsed(), answer)
        })
    };
    let (public_slow, internal_slow) = (slow(public), slow(internal));
    for (name, handle) in [("public", public_slow), ("internal", internal_slow)] {
        let (closed, after, answer) = handle.join().unwrap();
        assert!(closed, "{name}: still open after {after:?}");
        assert!(
            after >= Duration::from_secs(9) && after < Duration::from_secs(20),
            "{name}: closed after {after:?}, not the 10 s header read timeout"
        );
        assert!(
            answer.is_empty() || answer.starts_with(b"HTTP/1.1 408"),
            "{name}: {}",
            String::from_utf8_lossy(&answer)
        );
    }
    server.terminate();
}

/// `vault rotate` refuses memory storage: that store is per process, so
/// the command's own would be empty (and the running service's out of
/// reach), and a rotation reporting success over nothing would let an
/// operator drop a key still in use. Decisive: the refusal.
#[test]
fn vault_rotate_refuses_memory_storage() {
    let Ports {
        public,
        internal,
        held: _held,
    } = two_ports();
    let output = base(&mut Command::new(BIN), public, internal)
        .env("WA_SERVER_ENV", "development")
        .stdout(Stdio::piped())
        .args(["vault", "rotate"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("memory storage is per process"), "{stderr}");
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("WABAs walked"),
        "nothing was walked"
    );
}

/// The served binary takes Meta's deliveries on the public listener,
/// verified with `WA_APP_SECRET` or `WA_APP_SECRET_PREVIOUS`, into its
/// outbox (an operator-only event here, counted on `/metrics`); an
/// unsigned or forged one is `401`. Decisive: `serve` wiring the
/// configured app secrets and stores into the pipeline.
#[test]
fn serve_receives_signed_deliveries() {
    use meta_whatsapp_rs::core::secret::AppSecret;
    use meta_whatsapp_rs::webhooks::sign;

    let ports = two_ports();
    let (public, internal) = (ports.public, ports.internal);
    let mut server = serve(
        base(&mut Command::new(BIN), public, internal)
            .env("WA_SERVER_ENV", "development")
            .env(
                "WA_APP_SECRET_PREVIOUS",
                "previous-app-secret-for-the-binary-test",
            )
            .arg("serve"),
        ports,
    );
    server.wait_live(internal);
    server.wait_live(public);
    let body = r#"{"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
        "changes": [{"field": "a_field_meta_adds_later", "value": {"note": "anything"}}]}]}"#;
    let post = |signature: Option<String>| {
        let mut headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
        if let Some(signature) = signature {
            headers.push(("X-Hub-Signature-256".to_owned(), signature));
        }
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(n, v)| (n.as_str(), v.as_str()))
            .collect();
        http(public, "POST", "/webhooks/meta", &headers, body)
            .unwrap()
            .0
    };
    assert_eq!(post(None), 401);
    assert_eq!(
        post(Some(sign(
            &AppSecret::new("a-forger-s-secret"),
            body.as_bytes()
        ))),
        401
    );
    for secret in [
        "app-secret-for-the-binary-test",
        "previous-app-secret-for-the-binary-test",
    ] {
        assert_eq!(
            post(Some(sign(&AppSecret::new(secret), body.as_bytes()))),
            200
        );
    }
    let (status, metrics) = http(internal, "GET", "/metrics", &[], "").unwrap();
    assert_eq!(status, 200);
    let recorded = metrics
        .lines()
        .find(|l| {
            l.starts_with(
                "wa_server_webhook_events_total{event_type=\"unknown\",audience=\"operator\"}",
            )
        })
        .unwrap_or_else(|| panic!("{metrics}"));
    // The same body twice: one event (the second is a duplicate).
    assert!(recorded.ends_with(" 1"), "{recorded}");
    assert!(
        metrics.contains("wa_server_webhook_deliveries_total{outcome=\"unauthenticated\"} 2"),
        "{metrics}"
    );
    server.terminate();
}

/// `serve` gives the service the settings it read (M1b's `WA_SERVER_*`
/// limits, plumbed next to M1c's webhook pipeline): with a read budget of
/// one a second and a burst of one, a tenant polling its events back to
/// back is soon `429 too_many_requests`, which the defaults (50 a second)
/// never answer to five calls. Decisive: `serve` passing the configured
/// settings, not the defaults, to the service's state.
#[test]
fn serve_applies_the_configured_rate_limits() {
    let ports = two_ports();
    let (public, internal) = (ports.public, ports.internal);
    let mut server = serve(
        base(&mut Command::new(BIN), public, internal)
            .env("WA_SERVER_ENV", "development")
            .env("WA_SERVER_RATE_READ", "1")
            .env("WA_SERVER_RATE_READ_BURST", "1")
            .arg("serve"),
        ports,
    );
    let admin = server
        .admin_key
        .clone()
        .expect("no development admin key on standard error");
    server.wait_live(internal);
    let bearer = format!("Bearer {admin}");
    let json = [
        ("Authorization", bearer.as_str()),
        ("Content-Type", "application/json"),
    ];
    let (status, body) = http(
        internal,
        "POST",
        "/v1/admin/tenants",
        &json,
        r#"{"id": "merchant-42"}"#,
    )
    .unwrap();
    assert_eq!(status, 201, "{body}");
    let (status, body) = http(
        internal,
        "POST",
        "/v1/admin/tenants/merchant-42/keys",
        &json,
        r#"{"scopes": ["events"]}"#,
    )
    .unwrap();
    assert_eq!(status, 201, "{body}");
    let minted: serde_json::Value = serde_json::from_str(&body).unwrap();
    let tenant = format!("Bearer {}", minted["key"].as_str().unwrap());
    let polls: Vec<(u16, String)> = (0..5)
        .map(|_| {
            http(
                internal,
                "GET",
                "/v1/events",
                &[("Authorization", tenant.as_str())],
                "",
            )
            .unwrap()
        })
        .collect();
    assert_eq!(polls[0].0, 200, "{}", polls[0].1);
    assert!(
        polls
            .iter()
            .any(|(status, body)| *status == 429 && body.contains("\"too_many_requests\"")),
        "{polls:?}"
    );
    server.terminate();
}

#[test]
fn a_refused_configuration_exits_naming_the_variable_not_the_value() {
    let Ports {
        public,
        internal,
        held: _held,
    } = two_ports();
    let output = base(&mut Command::new(BIN), public, internal)
        .env("WA_SERVER_ENV", "development")
        .env("WA_APP_SECRET", "  ")
        .env("WA_OTP_PEPPER", "short-pepper-value")
        .arg("serve")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WA_APP_SECRET is blank"), "{stderr}");
    let output = base(&mut Command::new(BIN), public, internal)
        .env("WA_SERVER_ENV", "development")
        .env("WA_OTP_PEPPER", "short-pepper-value")
        .arg("serve")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("WA_OTP_PEPPER must be at least 32 bytes"),
        "{stderr}"
    );
    assert!(!stderr.contains("short-pepper-value"), "{stderr}");
    // Production without a database: memory storage is refused.
    let output = base(&mut Command::new(BIN), public, internal)
        .arg("serve")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("memory storage"));
}

/// The CLI mints the first admin key into Postgres (with an expiry, and
/// lists it without its secret); the served API accepts it, and `admin
/// revoke-key` stops it.
#[test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
fn live_postgres_cli_bootstrap_key_works_against_the_served_api() {
    let Some(url) = common::postgres_url() else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let db = runtime.block_on(common::TestDb::new()).unwrap();
    // The schema through the URL, as an operator would (libpq `options`).
    let separator = if url.contains('?') { '&' } else { '?' };
    let url = format!("{url}{separator}options=-c%20search_path%3D{}", db.schema);
    let ports = two_ports();
    let (public, internal) = (ports.public, ports.internal);
    let with_db = |command: &mut Command| {
        base(command, public, internal)
            .env("DATABASE_URL", &url)
            .env(
                "WA_VAULT_KEY",
                "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
            )
            .env("WA_OTP_PEPPER", "a-pepper-of-at-least-thirty-two-bytes");
    };
    // An expiry in the past is refused.
    let mut past = Command::new(BIN);
    with_db(&mut past);
    let refused = past
        .args([
            "admin",
            "create-admin-key",
            "--expires-at",
            "2020-01-01T00:00:00Z",
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--expires-at"));
    let mut mint = Command::new(BIN);
    with_db(&mut mint);
    let minted = mint
        .args([
            "admin",
            "create-admin-key",
            "--name",
            "ops",
            "--expires-at",
            "2099-01-01T00:00:00Z",
        ])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        minted.status.success(),
        "{}",
        String::from_utf8_lossy(&minted.stderr)
    );
    let key = String::from_utf8(minted.stdout).unwrap().trim().to_owned();
    assert!(
        key.starts_with("wak_") && !key.contains(char::is_whitespace),
        "{key}"
    );
    let stderr = String::from_utf8_lossy(&minted.stderr);
    assert!(!stderr.contains(&key), "the key goes to stdout only");

    let mut serving = Command::new(BIN);
    with_db(&mut serving);
    let mut server = serve(serving.arg("serve"), ports);
    server.wait_live(internal);
    let bearer = format!("Bearer {key}");
    let (status, body) = http(
        internal,
        "POST",
        "/v1/admin/tenants",
        &[
            ("Authorization", &bearer),
            ("Content-Type", "application/json"),
        ],
        r#"{"id": "merchant-42"}"#,
    )
    .unwrap();
    assert_eq!(status, 201, "{body}");
    let key_id = key["wak_".len()..].split_once('_').unwrap().0.to_owned();
    let secret = key.rsplit('_').next().unwrap().to_owned();
    let list = || {
        let mut list = Command::new(BIN);
        with_db(&mut list);
        let listed = list
            .args(["admin", "list-keys"])
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        assert!(listed.status.success());
        let listed = String::from_utf8(listed.stdout).unwrap();
        assert!(!listed.contains(&secret), "{listed}");
        listed
            .lines()
            .find(|l| l.starts_with(&key_id))
            .unwrap_or_else(|| panic!("{key_id} not listed: {listed}"))
            .split('\t')
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let line = list();
    assert_eq!(line[1], "admin");
    assert_eq!(line[4], "ops");
    assert_eq!(line[6], "2099-01-01T00:00:00Z");
    assert_eq!(line[7], "-", "not revoked");
    let mut revoke = Command::new(BIN);
    with_db(&mut revoke);
    assert!(
        revoke
            .args(["admin", "revoke-key", &key_id])
            .status()
            .unwrap()
            .success()
    );
    let (status, _) = http(
        internal,
        "GET",
        "/v1/admin/tenants",
        &[("Authorization", &bearer)],
        "",
    )
    .unwrap();
    assert_eq!(
        status, 401,
        "revoked through the CLI, refused by the running service"
    );
    assert_ne!(list()[7], "-", "listed as revoked");
    // `vault rotate` walks the (empty) bindings with the service's
    // configuration.
    let mut rotate = Command::new(BIN);
    with_db(&mut rotate);
    let rotated = rotate
        .args(["vault", "rotate"])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        rotated.status.success(),
        "{}",
        String::from_utf8_lossy(&rotated.stderr)
    );
    assert!(String::from_utf8_lossy(&rotated.stdout).starts_with("0 WABAs walked"));
    let stderr = server.terminate().join("\n");
    // On Postgres the CLI mints admin keys: `serve` prints none (DP9 is
    // memory storage's alone).
    assert!(
        !stderr.contains("wak_") && !stderr.contains("shown once"),
        "serve on Postgres printed an admin key: {stderr}"
    );
}

/// `vault rotate` fails, listing the WABA, when a record cannot be
/// re-encrypted (sealed with a key no longer configured): the operator
/// must keep the previous key. The others are rotated all the same.
/// Decisive: the command's exit status on a failed record.
#[test]
fn live_postgres_cli_vault_rotate_fails_when_a_record_does() {
    use meta_whatsapp_rs::adapters::store::PostgresKvStore;
    use meta_whatsapp_rs::client::embedded_signup::{
        StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
    };
    use meta_whatsapp_rs::core::ids::WabaId;
    use meta_whatsapp_rs::core::secret::AccessToken;
    use meta_whatsapp_rs::core::store::KvStore;
    use meta_whatsapp_server::model::TenantId;
    use meta_whatsapp_server::store::{PgStore, RecordStore, migrate};

    const ACTIVE: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
    const PREVIOUS: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
    const LOST: &str = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";
    let Some(url) = common::postgres_url() else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let db = runtime.block_on(common::TestDb::new()).unwrap();
    let kv: Arc<dyn KvStore> = runtime.block_on(async {
        let pool = db.pool(3).await;
        migrate(&pool).await.unwrap();
        let store = PgStore::new(pool.clone());
        let kv: Arc<dyn KvStore> = Arc::new(PostgresKvStore::new(pool));
        let tenant = TenantId::parse("merchant-a").unwrap();
        store.create_tenant(&tenant, "").await.unwrap();
        for (waba, key) in [
            ("201", VaultKey::from_base64("k0", PREVIOUS).unwrap()),
            ("202", VaultKey::from_base64("kx", LOST).unwrap()),
        ] {
            store
                .bind_waba(&tenant, &WabaId::new(waba).unwrap(), &[])
                .await
                .unwrap();
            TokenVault::new(kv.clone(), VaultKeys::new(key))
                .unwrap()
                .store(&StoredBusinessToken::new(
                    WabaId::new(waba).unwrap(),
                    AccessToken::new(format!("TOKEN-{waba}")),
                ))
                .await
                .unwrap();
        }
        kv
    });
    let separator = if url.contains('?') { '&' } else { '?' };
    let url = format!("{url}{separator}options=-c%20search_path%3D{}", db.schema);
    let Ports {
        public,
        internal,
        held: _held,
    } = two_ports();
    let rotated = base(&mut Command::new(BIN), public, internal)
        .env("DATABASE_URL", &url)
        .env("WA_VAULT_KEY", ACTIVE)
        .env("WA_VAULT_PREVIOUS_KEYS", format!("k0:{PREVIOUS}"))
        .env("WA_OTP_PEPPER", "a-pepper-of-at-least-thirty-two-bytes")
        .stdout(Stdio::piped())
        .args(["vault", "rotate"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&rotated.stdout);
    let stderr = String::from_utf8_lossy(&rotated.stderr);
    assert!(!rotated.status.success(), "{stdout}{stderr}");
    assert_eq!(
        stdout.trim(),
        "2 WABAs walked, 1 records re-encrypted, 1 failed: 202",
        "{stderr}"
    );
    assert!(stderr.contains("keep the previous key"), "{stderr}");
    // The other record was rotated: it opens with the active key alone.
    let active_only = TokenVault::new(
        kv,
        VaultKeys::new(VaultKey::from_base64("k1", ACTIVE).unwrap()),
    )
    .unwrap();
    let token = runtime
        .block_on(active_only.get(&WabaId::new("201").unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(token.token.expose_secret(), "TOKEN-201");
}
