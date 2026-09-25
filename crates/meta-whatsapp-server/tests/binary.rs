//! The real binary: `serve` starts both listeners and stops on `SIGTERM`,
//! `healthcheck` reaches the internal one, a refused configuration exits
//! without quoting the value, and (live) the CLI's first admin key works
//! against the served API.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_meta-whatsapp-server");

/// Two free local ports.
fn two_ports() -> (SocketAddr, SocketAddr) {
    let a = TcpListener::bind("127.0.0.1:0").unwrap();
    let b = TcpListener::bind("127.0.0.1:0").unwrap();
    (a.local_addr().unwrap(), b.local_addr().unwrap())
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

/// Wait until `GET /livez` answers 200.
fn wait_live(addr: SocketAddr, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            let mut stderr = String::new();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("the service exited ({status}): {stderr}");
        }
        if http(addr, "GET", "/livez", &[], "").is_some_and(|(s, _)| s == 200) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("{addr} never answered /livez");
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
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
}

#[test]
fn serve_answers_on_both_listeners_and_stops_on_sigterm() {
    let (public, internal) = two_ports();
    let mut child = base(&mut Command::new(BIN), public, internal)
        .env("WA_SERVER_ENV", "development")
        .arg("serve")
        .spawn()
        .unwrap();
    wait_live(internal, &mut child);
    wait_live(public, &mut child);
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
    terminate(&mut child);
    let down = Command::new(BIN)
        .env_clear()
        .env("WA_SERVER_INTERNAL_BIND", internal.to_string())
        .arg("healthcheck")
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!down.success(), "healthcheck against a stopped service");
}

#[test]
fn a_refused_configuration_exits_naming_the_variable_not_the_value() {
    let (public, internal) = two_ports();
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

/// The CLI mints the first admin key into Postgres; the served API
/// accepts it, and `admin revoke-key` stops it.
#[test]
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
    let (public, internal) = two_ports();
    let with_db = |command: &mut Command| {
        base(command, public, internal)
            .env("DATABASE_URL", &url)
            .env(
                "WA_VAULT_KEY",
                "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=",
            )
            .env("WA_OTP_PEPPER", "a-pepper-of-at-least-thirty-two-bytes");
    };
    let mut mint = Command::new(BIN);
    with_db(&mut mint);
    let minted = mint
        .args(["admin", "create-admin-key", "--name", "ops"])
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

    let mut serve = Command::new(BIN);
    with_db(&mut serve);
    let mut child = serve.arg("serve").spawn().unwrap();
    wait_live(internal, &mut child);
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
    terminate(&mut child);
}
