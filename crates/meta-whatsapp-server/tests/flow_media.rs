//! Roadmap M5d2 (design D454 to D457): a Flow's uploaded media, downloaded
//! from the CDN, verified and decrypted (`POST /v1/wabas/{waba_id}/flow-media`).
//!
//! The vector is `tests/flows_data/kat_media.json` (the library's own,
//! built from `flows/guides/media_upload`'s steps; Meta publishes none).
//! The CDN and the DNS are stubs that record what they were asked: the
//! decisive assertions are that **nothing was looked up or fetched** for a
//! URL the text refuses, or another tenant's WABA, and that **no byte of
//! the file is answered** when a check fails.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{Call, Harness};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode, header};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server_core::flow_media::{
    CdnFetcher, FetchError, FlowMediaDeployment, MAX_DOWNLOAD_BYTES,
};
use meta_whatsapp_server_core::webhooks_out::destination::{Host, Pinned, ResolveError, Resolver};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tokio::sync::Notify;

const A: &str = "merchant-a";
const B: &str = "merchant-b";
const WABA_A: &str = "102290129340398";
const WABA_B: &str = "202290129340399";
const PN_A: &str = "106540352242922";
const PN_B: &str = "206540352242923";
const KAT: &str = include_str!("flows_data/kat_media.json");
/// A public address (not in IANA's special-purpose registries).
const PUBLIC: &str = "157.240.22.35";

fn kat() -> Value {
    serde_json::from_str(KAT).unwrap()
}

fn cdn_file() -> Vec<u8> {
    STANDARD
        .decode(kat()["cdn_file"].as_str().unwrap())
        .unwrap()
}

fn plaintext() -> Vec<u8> {
    STANDARD
        .decode(kat()["plaintext"].as_str().unwrap())
        .unwrap()
}

/// What a fetch was asked.
#[derive(Debug, Clone)]
struct Fetched {
    host: String,
    path_and_query: String,
    address: std::net::SocketAddr,
    https: bool,
    max_bytes: usize,
    timeout: Duration,
}

/// A CDN that answers `answer` and records every fetch; with a gate, every
/// fetch waits for it.
struct Cdn {
    answer: Mutex<Result<Vec<u8>, FetchError>>,
    fetched: Mutex<Vec<Fetched>>,
    entered: AtomicUsize,
    gate: Option<Arc<Notify>>,
}

impl Cdn {
    fn new(answer: Result<Vec<u8>, FetchError>) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(answer),
            fetched: Mutex::new(Vec::new()),
            entered: AtomicUsize::new(0),
            gate: None,
        })
    }

    fn gated(answer: Vec<u8>, gate: Arc<Notify>) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(Ok(answer)),
            fetched: Mutex::new(Vec::new()),
            entered: AtomicUsize::new(0),
            gate: Some(gate),
        })
    }

    fn fetched(&self) -> Vec<Fetched> {
        self.fetched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl CdnFetcher for Cdn {
    async fn get(
        &self,
        target: &Pinned,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, FetchError> {
        self.fetched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Fetched {
                host: match &target.destination.host {
                    Host::Name(name) => name.clone(),
                    Host::Ip(ip) => ip.to_string(),
                },
                path_and_query: target.destination.path_and_query.clone(),
                address: target.address,
                https: target.destination.https,
                max_bytes,
                timeout,
            });
        self.entered.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.answer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A DNS that answers the same addresses for every name (or none), and
/// records every lookup.
struct Dns {
    answer: Vec<IpAddr>,
    looked_up: Mutex<Vec<String>>,
}

impl Dns {
    fn new(answer: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            answer: answer.iter().map(|a| a.parse().unwrap()).collect(),
            looked_up: Mutex::new(Vec::new()),
        })
    }

    fn looked_up(&self) -> Vec<String> {
        self.looked_up
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl Resolver for Dns {
    async fn resolve(&self, host: &str, _port: u16) -> Result<Vec<IpAddr>, ResolveError> {
        self.looked_up
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(host.to_owned());
        if self.answer.is_empty() {
            Err(ResolveError)
        } else {
            Ok(self.answer.clone())
        }
    }
}

struct Rig {
    h: Harness,
    cdn: Arc<Cdn>,
    dns: Arc<Dns>,
    key_a: String,
    key_b: String,
}

async fn rig_with(cdn: Arc<Cdn>, dns: Arc<Dns>) -> Rig {
    let deployment = FlowMediaDeployment::new(dns.clone(), cdn.clone());
    let h = Harness::with_settings(Settings {
        flow_media: Some(deployment),
        ..common::test_settings()
    });
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA_A, &[PN_A], "TOKEN-A").await;
    h.connect(B, WABA_B, &[PN_B], "TOKEN-B").await;
    let key_a = h.tenant_key(A, &[Scope::Flows]).await;
    let key_b = h.tenant_key(B, &[Scope::Flows]).await;
    Rig {
        h,
        cdn,
        dns,
        key_a,
        key_b,
    }
}

async fn rig() -> Rig {
    rig_with(Cdn::new(Ok(cdn_file())), Dns::new(&[PUBLIC])).await
}

fn post(waba: &str, key: &str, body: &Value) -> Call {
    Call::new(Method::POST, format!("/v1/wabas/{waba}/flow-media"))
        .key(key)
        .json(body)
}

/// A media object as Meta's data exchange request carries it, with `edit`
/// applied.
fn media(edit: impl FnOnce(&mut Value)) -> Value {
    let mut media = kat()["media"].clone();
    edit(&mut media);
    media
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// `media_upload`'s steps, end to end: the file is fetched from the
/// listed host at the address the check pinned, verified and decrypted;
/// the answer is the bytes, as an attachment, and Meta's API is never
/// called.
#[tokio::test]
async fn a_file_is_downloaded_verified_and_decrypted() {
    let r = rig().await;
    let reply = r.h.call(post(WABA_A, &r.key_a, &media(|_| {}))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.text, lossy(&plaintext()));
    let h = &reply.headers;
    assert_eq!(h[header::CONTENT_TYPE], "application/octet-stream");
    assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(h[header::CONTENT_DISPOSITION], "attachment");
    assert_eq!(h[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        h[header::CONTENT_SECURITY_POLICY],
        "sandbox; default-src 'none'"
    );

    let fetched = r.cdn.fetched();
    assert_eq!(fetched.len(), 1, "{fetched:?}");
    assert_eq!(fetched[0].host, "mmg.whatsapp.net");
    assert_eq!(fetched[0].path_and_query, "/v/redacted");
    assert_eq!(fetched[0].address, format!("{PUBLIC}:443").parse().unwrap());
    assert!(fetched[0].https);
    assert_eq!(fetched[0].max_bytes, MAX_DOWNLOAD_BYTES);
    assert_eq!(fetched[0].timeout, Duration::from_secs(60));
    assert_eq!(r.dns.looked_up(), ["mmg.whatsapp.net"]);
    assert!(r.h.graph.requests().is_empty(), "Meta is never called");
}

/// The object Meta sends is posted whole: `media_id` and `file_name` are
/// accepted and unused; another field is refused.
#[tokio::test]
async fn metas_object_is_accepted_whole_and_unknown_fields_are_refused() {
    let r = rig().await;
    let whole = media(|_| {});
    assert_eq!(
        r.h.call(post(WABA_A, &r.key_a, &whole)).await.status,
        StatusCode::OK
    );
    let mut extra = whole.clone();
    extra["surprise"] = json!(1);
    let refused = r.h.call(post(WABA_A, &r.key_a, &extra)).await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    let mut nested = whole;
    nested["encryption_metadata"]["surprise"] = json!(1);
    assert_eq!(
        r.h.call(post(WABA_A, &r.key_a, &nested)).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(r.cdn.fetched().len(), 1, "only the first was fetched");
}

/// **SSRF, by the URL's text.** Each URL is `422` on `cdn_url` with no
/// lookup and no fetch. Delete the host check and the non-Meta host, the
/// suffix tricks and the address literals fetch; delete the scheme, port
/// or user info checks and their rows fetch.
#[tokio::test]
async fn a_url_that_is_not_a_listed_https_host_name_is_refused_before_any_lookup() {
    let r = rig().await;
    for url in [
        "https://example.com/v/x",
        "https://mmg.whatsapp.net.evil.example/v/x",
        "https://evilwhatsapp.net/v/x",
        "https://whatsapp.net/v/x",
        "https://169.254.169.254/latest/meta-data/",
        "https://127.0.0.1/v/x",
        "https://10.0.0.5/v/x",
        "https://192.168.1.10/v/x",
        "https://[::1]/v/x",
        "https://[::ffff:10.0.0.5]/v/x",
        "https://0x7f000001/v/x",
        "https://2130706433/v/x",
        "https://localhost/v/x",
        "http://mmg.whatsapp.net/v/x",
        "ftp://mmg.whatsapp.net/v/x",
        "https://mmg.whatsapp.net:8443/v/x",
        "https://mmg.whatsapp.net@169.254.169.254/v/x",
        "https://user:pass@mmg.whatsapp.net/v/x",
        "//mmg.whatsapp.net/v/x",
        "not a url",
        "",
    ] {
        let reply =
            r.h.call(post(
                WABA_A,
                &r.key_a,
                &media(|m| m["cdn_url"] = json!(url)),
            ))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{url}: {}",
            reply.text
        );
        assert_eq!(reply.code(), "invalid_request", "{url}");
        assert_eq!(reply.json()["error"]["field"], "cdn_url", "{url}");
        assert!(
            !reply.text.contains(url) || url.is_empty(),
            "{url} was quoted"
        );
    }
    let long = format!("https://mmg.whatsapp.net/{}", "a".repeat(3000));
    let reply =
        r.h.call(post(
            WABA_A,
            &r.key_a,
            &media(|m| m["cdn_url"] = json!(long)),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.dns.looked_up().is_empty(), "{:?}", r.dns.looked_up());
    assert!(r.cdn.fetched().is_empty(), "{:?}", r.cdn.fetched());
}

/// **SSRF, by the address.** A listed name that resolves to a private,
/// loopback, link-local, metadata or unique-local address, or to a mix
/// with one, is refused after the lookup and before the connection
/// (DNS rebinding included: the check is on what the lookup answered).
/// Delete the address check and every row fetches.
#[tokio::test]
async fn a_listed_name_that_resolves_to_a_non_public_address_is_not_fetched() {
    for answer in [
        &["10.0.0.5"][..],
        &["127.0.0.1"],
        &["169.254.169.254"],
        &["192.168.0.1"],
        &["172.16.0.9"],
        &["100.64.0.1"],
        &["::1"],
        &["fd00:ec2::254"],
        &["fe80::1"],
        &["::ffff:10.0.0.5"],
        &["0.0.0.0"],
        &[PUBLIC, "10.0.0.5"],
        &["10.0.0.5", PUBLIC],
    ] {
        let r = rig_with(Cdn::new(Ok(cdn_file())), Dns::new(answer)).await;
        let reply = r.h.call(post(WABA_A, &r.key_a, &media(|_| {}))).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{answer:?}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], "cdn_url");
        assert_eq!(r.dns.looked_up(), ["mmg.whatsapp.net"], "{answer:?}");
        assert!(r.cdn.fetched().is_empty(), "{answer:?} was fetched");
        assert!(!reply.text.contains(answer[0]), "the address was quoted");
    }
    // A name that does not resolve is the download failing, not a refusal.
    let r = rig_with(Cdn::new(Ok(cdn_file())), Dns::new(&[])).await;
    let reply = r.h.call(post(WABA_A, &r.key_a, &media(|_| {}))).await;
    assert_eq!(reply.status.as_u16(), 502, "{}", reply.text);
    assert_eq!(reply.code(), "media_download_failed");
    assert!(r.cdn.fetched().is_empty());
}

/// A redirect is a failed download: the fetcher's contract (it follows
/// none; `HyperFetcher`'s own test proves it against a real receiver) and
/// the service's reading of it. The CDN failing in any way is `502
/// media_download_failed`, retryable, and nothing is answered.
#[tokio::test]
async fn a_redirect_or_a_failed_fetch_is_a_failed_download() {
    for error in [
        FetchError::Status(302),
        FetchError::Status(301),
        FetchError::Status(404),
        FetchError::Connect,
        FetchError::Tls,
        FetchError::Timeout,
        FetchError::Protocol,
    ] {
        let r = rig_with(Cdn::new(Err(error)), Dns::new(&[PUBLIC])).await;
        let reply = r.h.call(post(WABA_A, &r.key_a, &media(|_| {}))).await;
        assert_eq!(reply.status.as_u16(), 502, "{error:?}: {}", reply.text);
        assert_eq!(reply.code(), "media_download_failed");
        assert_eq!(reply.json()["error"]["retryable"], true);
        assert_eq!(r.cdn.fetched().len(), 1);
    }
}

/// **Nothing is returned unless every check passed.** The file altered in
/// one byte, cut short, with a wrong hash in the request, with the wrong
/// keys: `502 integrity`, and no part of the plaintext in the answer.
/// Delete the hash check (the file's SHA-256, the HMAC, or the result's
/// SHA-256 in the library's `decrypt_media`) and its row is answered `200`.
#[tokio::test]
async fn a_file_that_fails_a_check_is_refused_and_nothing_is_returned() {
    let good = cdn_file();
    let mut flipped = good.clone();
    flipped[3] ^= 1;
    let mut flipped_tag = good.clone();
    *flipped_tag.last_mut().unwrap() ^= 1;
    let wrong32 = STANDARD.encode([9u8; 32]);
    let wrong16 = STANDARD.encode([9u8; 16]);
    type Edit = Box<dyn Fn(&mut Value)>;
    let rows: Vec<(&str, Vec<u8>, Edit)> = vec![
        ("one byte of the file altered", flipped, Box::new(|_| {})),
        ("the HMAC altered", flipped_tag, Box::new(|_| {})),
        (
            "cut short",
            good[..good.len() - 1].to_vec(),
            Box::new(|_| {}),
        ),
        ("empty", Vec::new(), Box::new(|_| {})),
        (
            "the file's hash wrong",
            good.clone(),
            Box::new(move |m| m["encryption_metadata"]["encrypted_hash"] = json!(wrong32)),
        ),
        (
            "the plaintext hash wrong",
            good.clone(),
            Box::new({
                let wrong = STANDARD.encode([8u8; 32]);
                move |m| m["encryption_metadata"]["plaintext_hash"] = json!(wrong)
            }),
        ),
        (
            "the HMAC key wrong",
            good.clone(),
            Box::new({
                let wrong = STANDARD.encode([7u8; 32]);
                move |m| m["encryption_metadata"]["hmac_key"] = json!(wrong)
            }),
        ),
        (
            "the AES key wrong",
            good.clone(),
            Box::new({
                let wrong = STANDARD.encode([6u8; 32]);
                move |m| m["encryption_metadata"]["encryption_key"] = json!(wrong)
            }),
        ),
        (
            "the IV wrong",
            good.clone(),
            Box::new(move |m| m["encryption_metadata"]["iv"] = json!(wrong16)),
        ),
    ];
    for (name, file, edit) in rows {
        let r = rig_with(Cdn::new(Ok(file)), Dns::new(&[PUBLIC])).await;
        let reply = r.h.call(post(WABA_A, &r.key_a, &media(|m| edit(m)))).await;
        assert_eq!(reply.status.as_u16(), 502, "{name}: {}", reply.text);
        assert_eq!(reply.code(), "integrity", "{name}");
        assert!(!reply.text.contains("not really a JPEG"), "{name}: bytes answered");
        assert!(
            !reply.headers.contains_key(header::CONTENT_DISPOSITION),
            "{name}"
        );
        assert_eq!(r.cdn.fetched().len(), 1, "{name}");
    }
}

/// The size cap: the fetcher is told the bound (25 MiB and the padding and
/// HMAC), and a file past it is `413 media_too_large`.
#[tokio::test]
async fn a_file_past_the_cap_is_refused() {
    assert_eq!(MAX_DOWNLOAD_BYTES, 25_600 * 1024 + 64);
    let r = rig_with(Cdn::new(Err(FetchError::TooLarge)), Dns::new(&[PUBLIC])).await;
    let reply = r.h.call(post(WABA_A, &r.key_a, &media(|_| {}))).await;
    assert_eq!(reply.status.as_u16(), 413, "{}", reply.text);
    assert_eq!(reply.code(), "media_too_large");
    assert_eq!(r.cdn.fetched()[0].max_bytes, MAX_DOWNLOAD_BYTES);
}

/// Values that cannot open any file are `422` before any lookup: not
/// base64, or of the wrong length.
#[tokio::test]
async fn keys_of_the_wrong_shape_are_refused_before_any_lookup() {
    let r = rig().await;
    for (field, value) in [
        ("encrypted_hash", json!("AAAA")),
        ("iv", json!(STANDARD.encode([1u8; 12]))),
        ("encryption_key", json!(STANDARD.encode([1u8; 16]))),
        ("hmac_key", json!("not base64!")),
        ("plaintext_hash", json!(5)),
    ] {
        let reply =
            r.h.call(post(
                WABA_A,
                &r.key_a,
                &media(|m| m["encryption_metadata"][field] = value.clone()),
            ))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{field}: {}",
            reply.text
        );
        if field != "plaintext_hash" {
            assert_eq!(
                reply.json()["error"]["field"],
                format!("encryption_metadata.{field}")
            );
        }
    }
    assert!(r.dns.looked_up().is_empty());
    assert!(r.cdn.fetched().is_empty());
}

/// **M5.1 / M5.2 for the download.** Another tenant's WABA is `404`, with
/// nothing looked up, nothing fetched and no vault read; a key without the
/// scope `flows` is `403`.
#[tokio::test]
async fn another_tenants_waba_is_not_found_and_nothing_is_looked_up() {
    let r = rig().await;
    let reads = r.h.kv.vault_reads();
    // B's key on A's WABA, and A's on B's.
    for (waba, key) in [(WABA_A, &r.key_b), (WABA_B, &r.key_a)] {
        let reply = r.h.call(post(waba, key, &media(|_| {}))).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
        assert_eq!(reply.code(), "not_found");
    }
    assert_eq!(r.h.kv.vault_reads(), reads, "the vault was read");
    assert!(r.dns.looked_up().is_empty());
    assert!(r.cdn.fetched().is_empty());
    // The owner's own succeeds.
    assert_eq!(
        r.h.call(post(WABA_B, &r.key_b, &media(|_| {})))
            .await
            .status,
        StatusCode::OK
    );
    let narrow = r.h.tenant_key(A, &[Scope::Numbers]).await;
    let reply = r.h.call(post(WABA_A, &narrow, &media(|_| {}))).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
    assert_eq!(r.cdn.fetched().len(), 1);
}

/// With no download configured (the harness default), the route is `403
/// feature_not_available` and nothing is looked up.
#[tokio::test]
async fn a_deployment_without_a_download_answers_feature_not_available() {
    let h = Harness::with_settings(common::test_settings());
    h.tenant(A).await;
    h.connect(A, WABA_A, &[PN_A], "TOKEN-A").await;
    let key = h.tenant_key(A, &[Scope::Flows]).await;
    let reply = h.call(post(WABA_A, &key, &media(|_| {}))).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
    assert_eq!(reply.code(), "feature_not_available");
}

/// At most four downloads are in flight on a replica; the fifth is `429`
/// with `Retry-After`, and the slots come back. Delete the slot check and
/// the fifth fetches.
#[tokio::test]
async fn at_most_four_downloads_run_at_once() {
    let gate = Arc::new(Notify::new());
    let r = rig_with(Cdn::gated(cdn_file(), gate.clone()), Dns::new(&[PUBLIC])).await;
    let body = media(|_| {});
    let call = || r.h.call(post(WABA_A, &r.key_a, &body));
    let fifth = async {
        while r.cdn.entered.load(Ordering::SeqCst) < 4 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let refused = r.h.call(post(WABA_A, &r.key_a, &body)).await;
        gate.notify_waiters();
        refused
    };
    let (one, two, three, four, refused) = tokio::join!(call(), call(), call(), call(), fifth);
    assert_eq!(
        refused.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        refused.text
    );
    assert_eq!(refused.code(), "too_many_requests");
    assert!(refused.headers.contains_key(header::RETRY_AFTER));
    for reply in [one, two, three, four] {
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    }
    assert_eq!(r.cdn.fetched().len(), 4, "the fifth was not fetched");
    // The slots are back.
    let again = r.h.call(post(WABA_A, &r.key_a, &body));
    gate.notify_one();
    assert_eq!(again.await.status, StatusCode::OK);
}
