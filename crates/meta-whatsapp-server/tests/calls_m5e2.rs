//! Roadmap M5e2: what M5e did not route (docs/design/server.md, D410 to
//! D416): the SIP password read behind its own scope and the deployment's
//! setting, the voicemail announcement upload with Meta's
//! `use_case=call_voicemail_announcement`, a call's recording media
//! downloaded through the media route, and the call permission request
//! message through the send route.
//!
//! Each test sends Meta's documented request and answer
//! (`calling/call-settings`, `calling/call-recording`,
//! `calling/user-call-permissions`): the exact requests (method, path,
//! query, the tenant's token, the form or JSON), `remaining() == 0`. M5.1
//! over every route of the document is `tests/authz.rs`; the logs, the
//! audit events and the planted secrets are `tests/logs.rs`'s capture.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness, ogg_opus};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const OTHER_TENANT: &str = "merchant-43";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242924";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TOKEN: &str = "EAAG-other-token";
const PASSWORD: &str = "SIP-PASSWORD-SENTINEL-7K2";

/// Two tenants with a WABA and a number each, the first tenant's keys: one
/// with `calls` alone, one with `call_credentials` alone, one with every
/// scope but `call_credentials`.
struct World {
    h: Harness,
    calls: String,
    credentials: String,
    without_credentials: String,
}

async fn world(sip_credentials: bool) -> World {
    let h = Harness::with_settings(Settings {
        call_sip_credentials: sip_credentials,
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], OTHER_TOKEN)
        .await;
    let calls = h.tenant_key(TENANT, &[Scope::Calls]).await;
    let credentials = h.tenant_key(TENANT, &[Scope::CallCredentials]).await;
    let others: Vec<Scope> = Scope::ALL
        .into_iter()
        .filter(|scope| *scope != Scope::CallCredentials)
        .collect();
    let without_credentials = h.tenant_key(TENANT, &others).await;
    World {
        h,
        calls,
        credentials,
        without_credentials,
    }
}

fn sip_path(pn: &str) -> String {
    format!("/v1/numbers/{pn}/call-settings/sip-credentials")
}

/// calling/call-settings, "Include SIP user password in response".
fn meta_sip_answer() -> Value {
    json!({"calling": {"status": "ENABLED", "sip": {"status": "ENABLED", "servers": [
        {"hostname": "sip.example.com", "sip_user_password": PASSWORD}]}}})
}

// ─── The SIP password ────────────────────────────────────────────────────

/// calling/call-settings, "Include SIP user password in response": the
/// request is Meta's (`GET /{pn}/settings?include_sip_credentials=true`,
/// the tenant's token), the answer the servers with the password and
/// `Cache-Control: no-store`. The ordinary settings read of the same
/// deployment still never sends the parameter nor answers a password.
/// Decisive: `settings(true)` in `Calls::sip_credentials`; `settings(false)`
/// in `Calls::settings`.
#[tokio::test]
async fn the_password_is_asked_with_metas_parameter_and_answered_only_here() {
    let w = world(true).await;
    w.h.graph.push_json(200, meta_sip_answer());
    let reply =
        w.h.call(Call::get(sip_path(PN)).key(&w.credentials)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.headers["cache-control"], "no-store");
    assert_eq!(
        reply.json(),
        json!({"status": "ENABLED", "servers": [
            {"hostname": "sip.example.com", "port": null, "sip_user_password": PASSWORD}]})
    );
    let requests = w.h.graph.requests();
    let [read] = &requests[..] else {
        panic!("one request: {requests:?}")
    };
    assert_eq!(read.method, Method::GET);
    assert_eq!(read.path(), format!("/v25.0/{PN}/settings"));
    assert_eq!(read.url.query(), Some("include_sip_credentials=true"));
    assert_eq!(read.bearer(), Some(TOKEN));

    // A number whose SIP was never configured: no servers, no status.
    w.h.graph
        .push_json(200, json!({"calling": {"status": "ENABLED"}}));
    let reply =
        w.h.call(Call::get(sip_path(PN)).key(&w.credentials)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json(), json!({"status": null, "servers": []}));

    // The ordinary read, with the setting on and Meta sending a password
    // anyway: the parameter is not sent, the password is not answered.
    w.h.graph.push_json(200, meta_sip_answer());
    let reply =
        w.h.call(Call::get(format!("/v1/numbers/{PN}/call-settings")).key(&w.calls))
            .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert!(!reply.text.contains(PASSWORD), "{}", reply.text);
    assert!(!reply.text.contains("password"), "{}", reply.text);
    let ordinary = w.h.graph.last_request().unwrap();
    assert_eq!(ordinary.url.query(), None);
    assert_eq!(w.h.graph.remaining(), 0);
}

/// The scope `call_credentials` is its own: a key with `calls`, and a key
/// with every other scope, are `403 forbidden` before anything is read or
/// asked, with the setting on; the key holding it alone is answered.
/// Decisive: `Operation::scope` for `GetCallSipCredentials` (map it to
/// `Calls` and the first two are answered).
#[tokio::test]
async fn the_password_needs_the_scope_that_no_other_scope_stands_in_for() {
    let w = world(true).await;
    let reads = w.h.kv.vault_reads();
    for key in [&w.calls, &w.without_credentials] {
        let reply = w.h.call(Call::get(sip_path(PN)).key(key)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{}",
            reply.text
        );
        assert!(!reply.text.contains(PASSWORD));
    }
    assert_eq!(w.h.kv.vault_reads(), reads, "no token was opened");
    assert!(w.h.graph.requests().is_empty(), "Meta was not asked");
    // No key at all.
    let reply = w.h.call(Call::get(sip_path(PN))).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.text);
    // The scope is a name an operator can give.
    assert!(Scope::parse("call_credentials") == Some(Scope::CallCredentials));
    assert!(Scope::CallCredentials.as_str() == "call_credentials");
}

/// The setting is off by default (`Settings::default()`): the read is
/// `403 feature_not_available` for the key that holds the scope, **nothing
/// is sent to Meta**, and the answer says nothing of the password.
/// Decisive: the gate in `Calls::sip_credentials` (delete it and Meta is
/// asked).
#[tokio::test]
async fn with_the_setting_off_the_read_is_refused_and_nothing_is_sent() {
    assert!(
        !Settings::default().call_sip_credentials,
        "off unless the deployment enables it"
    );
    let w = world(false).await;
    w.h.graph.push_json(200, meta_sip_answer());
    let reply =
        w.h.call(Call::get(sip_path(PN)).key(&w.credentials)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::FORBIDDEN, "feature_not_available"),
        "{}",
        reply.text
    );
    assert!(!reply.text.contains(PASSWORD), "{}", reply.text);
    assert!(w.h.graph.requests().is_empty(), "nothing was sent");
    assert_eq!(w.h.graph.remaining(), 1, "Meta's answer was never read");
}

/// M5.2 for the read: another tenant's number (and one that does not
/// exist) is `404 not_found` with zero vault reads and nothing sent, the
/// setting on and the scope held; a Meta refusal answers its code and the
/// service's kind and never Meta's text, which here quotes the password; a
/// timeout is `504` and the password is in no error.
#[tokio::test]
async fn the_read_is_the_numbers_and_its_failures_carry_no_secret() {
    let w = world(true).await;
    let reads = w.h.kv.vault_reads();
    for pn in [OTHER_PN, "999999999999999"] {
        let reply = w.h.call(Call::get(sip_path(pn)).key(&w.credentials)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{pn}: {}",
            reply.text
        );
    }
    assert_eq!(w.h.kv.vault_reads(), reads, "no token was opened");
    assert!(w.h.graph.requests().is_empty());

    // 138000: calling is not enabled; the text quotes the secret.
    w.h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#138000) {PASSWORD}"), "type": "OAuthException",
                         "code": 138_000, "error_data": {"messaging_product": "whatsapp",
                                                          "details": PASSWORD}}}),
    );
    let reply =
        w.h.call(Call::get(sip_path(PN)).key(&w.credentials)).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.text);
    assert_eq!(reply.code(), "calling_not_enabled");
    assert!(!reply.text.contains(PASSWORD), "{}", reply.text);

    w.h.graph.push_error(|| TransportError::Timeout);
    let reply =
        w.h.call(Call::get(sip_path(PN)).key(&w.credentials)).await;
    assert_eq!(reply.status, StatusCode::GATEWAY_TIMEOUT, "{}", reply.text);
    assert!(!reply.text.contains(PASSWORD), "{}", reply.text);
    assert_eq!(w.h.graph.remaining(), 0);
}

// ─── The voicemail announcement ──────────────────────────────────────────

fn announcement_path(pn: &str) -> String {
    format!("/v1/numbers/{pn}/call-settings/voicemail-announcement")
}

fn announce(key: &str, file: &[u8]) -> Call {
    Call::new(Method::POST, announcement_path(PN))
        .key(key)
        .multipart(&[("file", Some("my-announcement.ogg"), file)])
}

/// calling/call-settings, "Upload a voicemail announcement media file":
/// `POST /{pn}/media`, multipart `file` (`audio/ogg`), `messaging_product`
/// and `use_case=call_voicemail_announcement`, with the tenant's token; the
/// answer is the media id, `201`. The id then goes into the settings, which
/// first look it up as the number's own (D211). Decisive:
/// `UploadOptions::voicemail_announcement()` in
/// `Calls::upload_voicemail_announcement`.
#[tokio::test]
async fn an_announcement_is_uploaded_with_the_use_case_and_named_in_the_settings() {
    let w = world(false).await;
    let file = ogg_opus(b"a short announcement");
    w.h.graph.push_json(200, json!({"id": "1037543291543636"}));
    let reply = w.h.call(announce(&w.calls, &file)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(reply.json(), json!({"media_id": "1037543291543636"}));
    let requests = w.h.graph.requests();
    let [sent] = &requests[..] else {
        panic!("one request: {requests:?}")
    };
    assert_eq!(sent.method, Method::POST);
    assert_eq!(sent.path(), format!("/v25.0/{PN}/media"));
    assert_eq!(sent.url.query(), None);
    assert_eq!(sent.bearer(), Some(TOKEN));
    let field = |name: &str| sent.multipart_field(name).unwrap();
    assert_eq!(&field("messaging_product").2[..], b"whatsapp");
    assert_eq!(&field("type").2[..], b"audio/ogg");
    assert_eq!(&field("use_case").2[..], b"call_voicemail_announcement");
    let (filename, content_type, data) = field("file");
    assert_eq!(filename, Some("announcement.ogg"));
    assert_eq!(content_type, Some("audio/ogg"));
    assert_eq!(&data[..], &file[..]);
    assert!(sent.multipart_field("description").is_none());

    // Named in the settings: looked up on the number, then set.
    w.h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "url": "https://lookaside.fbsbx.com/x",
               "mime_type": "audio/ogg", "sha256": "abc", "file_size": 1, "id": "1037543291543636"}),
    );
    w.h.graph.push_json(200, json!({"success": true}));
    let reply = w.h.call(
        Call::new(Method::PATCH, format!("/v1/numbers/{PN}/call-settings"))
            .key(&w.calls)
            .json(&json!({"voicemail": {"status": "ENABLED", "triggers": ["REJECT"],
                "audio": {"default": {"announcement_media_id": "1037543291543636"}}}})),
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let requests = w.h.graph.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1].query("phone_number_id").as_deref(),
        Some(PN),
        "the lookup is the number's"
    );
    assert_eq!(w.h.graph.remaining(), 0);
}

/// A file the service can see is not what Meta takes is refused `422` on
/// `file` with **no request**: the bytes decide, not the name or the part's
/// type (a PNG named `.ogg`, an MP3, an empty file, an Ogg stream that is
/// not Opus, an `OpusHead` past the first page's start); a form with
/// another field is `422` on `body`; a file past 16 MiB is `413`, the body
/// read no further. Decisive: `VoicemailAudio::new`, each clause.
#[tokio::test]
async fn a_file_that_is_not_ogg_opus_is_refused_with_no_request() {
    let w = world(false).await;
    let mut late = b"OggS".to_vec();
    late.extend([0u8; 200]);
    late.extend(b"OpusHead");
    let refused: [(&str, Vec<u8>); 6] = [
        ("a PNG", common::SAMPLE_PNG.to_vec()),
        ("an mp3", b"ID3\x04\x00\x00".to_vec()),
        ("empty", Vec::new()),
        ("Ogg Vorbis", b"OggS\x00\x02\x00\x00\x01vorbis".to_vec()),
        ("OpusHead without OggS", b"\x00\x00OpusHead".to_vec()),
        ("OpusHead far in", late),
    ];
    for (what, file) in refused {
        let reply = w.h.call(announce(&w.calls, &file)).await;
        assert_eq!(
            (
                reply.status,
                reply.code().as_str(),
                reply.json()["error"]["field"].as_str()
            ),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request", Some("file")),
            "{what}: {}",
            reply.text
        );
    }
    // Another field, and no file.
    let reply = w.h.call(
        Call::new(Method::POST, announcement_path(PN))
            .key(&w.calls)
            .multipart(&[
                ("use_case", None, b"call_voicemail_announcement"),
                ("file", Some("a.ogg"), &ogg_opus(b"")),
            ]),
    )
    .await;
    assert_eq!(
        (reply.status, reply.json()["error"]["field"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("body")),
        "{}",
        reply.text
    );
    let reply = w.h.call(
        Call::new(Method::POST, announcement_path(PN))
            .key(&w.calls)
            .multipart(&[]),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", reply.text);
    // Past 16 MiB.
    let mut big = ogg_opus(b"");
    big.resize(16 * 1024 * 1024 + 1, 0);
    let reply = w.h.call(announce(&w.calls, &big)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "media_too_large"),
        "{}",
        reply.text
    );
    assert!(w.h.graph.requests().is_empty(), "nothing reached Meta");
}

/// The upload takes the scope `calls`, and an `Idempotency-Key`: a repeat
/// of the same key and file never uploads twice, another file under the
/// key is `422 idempotency_key_reused`, and a timeout is `504` that the
/// key keeps (never repeated). Decisive: `Fingerprint::
/// upload_call_voicemail_announcement` and the route's `idempotent` call.
#[tokio::test]
async fn an_announcement_key_never_uploads_twice_and_a_timeout_is_not_repeated() {
    let w = world(false).await;
    let file = ogg_opus(b"one");
    let keyed = |key: &str, file: &[u8]| announce(&w.calls, file).header("idempotency-key", key);
    w.h.graph.push_json(200, json!({"id": "1037543291543636"}));
    let first = w.h.call(keyed("vm-1", &file)).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    let again = w.h.call(keyed("vm-1", &file)).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.text);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, first.text);
    let other = w.h.call(keyed("vm-1", &ogg_opus(b"two"))).await;
    assert_eq!(other.code(), "idempotency_key_reused", "{}", other.text);
    assert_eq!(w.h.graph.requests().len(), 1);

    w.h.graph.push_error(|| TransportError::Timeout);
    let cut = w.h.call(keyed("vm-2", &file)).await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    let replay = w.h.call(keyed("vm-2", &file)).await;
    assert_eq!(replay.status, StatusCode::GATEWAY_TIMEOUT, "{}", replay.text);
    assert_eq!(w.h.graph.requests().len(), 2, "not repeated");
    assert_eq!(w.h.graph.remaining(), 0);

    // The scope: `send` and every other scope do not stand in for `calls`.
    let others: Vec<Scope> = Scope::ALL
        .into_iter()
        .filter(|scope| *scope != Scope::Calls)
        .collect();
    let key = w.h.tenant_key(TENANT, &others).await;
    let reads = w.h.kv.vault_reads();
    let reply = w.h.call(announce(&key, &file)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden"),
        "{}",
        reply.text
    );
    assert_eq!(w.h.kv.vault_reads(), reads);
}

/// M5.2 for the upload: another tenant's number is `404` with zero vault
/// reads, nothing sent, whatever the file.
#[tokio::test]
async fn an_announcement_on_another_tenants_number_is_not_found_with_zero_vault_reads() {
    let w = world(false).await;
    let reads = w.h.kv.vault_reads();
    let reply = w.h.call(
        Call::new(Method::POST, announcement_path(OTHER_PN))
            .key(&w.calls)
            .multipart(&[("file", Some("a.ogg"), &ogg_opus(b"x"))]),
    )
    .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        reply.text
    );
    assert_eq!(w.h.kv.vault_reads(), reads);
    assert!(w.h.graph.requests().is_empty());
}

// ─── Recordings and transcripts ──────────────────────────────────────────

/// calling/call-recording, "Download the recording": the media id of the
/// `call_recording_available` event goes through the media route like any
/// media: `GET /{id}?phone_number_id={pn}`, then the URL. What the pages
/// do **not** say is whether Meta honours `phone_number_id` for a
/// recording's id (D414); this test pins what the service sends, not what
/// Meta does. A media id Meta refuses for the number (another number's) is
/// `404` with only the lookup sent. Decisive: the scoped lookup in
/// `Media::url`.
#[tokio::test]
async fn a_recordings_media_is_looked_up_on_the_number_and_another_numbers_is_not_found() {
    use base64::Engine as _;
    use sha2::Digest as _;
    let w = world(false).await;
    let key = w.h.tenant_key(TENANT, &[Scope::Media]).await;
    let recording = b"OggS recording bytes".to_vec();
    // Meta's digest for a recording is base64 (calling/call-recording).
    let digest = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(&recording));
    let id = "1002764438271669";
    w.h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "id": id, "mime_type": "audio/ogg; codecs=opus",
               "sha256": digest, "file_size": recording.len(),
               "url": "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1002764438271669"}),
    );
    w.h.graph.push_bytes(200, "audio/ogg", recording.clone());
    let reply = w.h.call(Call::get(format!("/v1/numbers/{PN}/media/{id}")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let requests = w.h.graph.requests();
    let [lookup, bytes] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_eq!(lookup.path(), format!("/v25.0/{id}"));
    assert_eq!(lookup.query("phone_number_id").as_deref(), Some(PN));
    assert_eq!(lookup.bearer(), Some(TOKEN));
    assert_eq!(bytes.bearer(), Some(TOKEN));

    // Meta refuses the id for this number: 404, the lookup is all that was sent.
    let asked = w.h.graph.requests().len();
    w.h.graph.push_json(
        400,
        json!({"error": {"message": "(#100) Unsupported get request", "type": "OAuthException",
                         "code": 100, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let reply = w.h.call(Call::get(format!("/v1/numbers/{PN}/media/{id}")).key(&key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        reply.text
    );
    assert_eq!(w.h.graph.requests().len(), asked + 1, "the lookup only");
    assert_eq!(
        w.h.graph.last_request().unwrap().query("phone_number_id").as_deref(),
        Some(PN)
    );
    assert_eq!(w.h.graph.remaining(), 0);
}

// ─── The permission request message ──────────────────────────────────────

/// calling/user-call-permissions, "Send free form call permission request
/// message": `POST /{pn}/messages` with the interactive object, by phone
/// number or by BSUID (`recipient`), through the send route (scope `send`):
/// the exact body Meta documents. Nothing new is routed for it (D415): the
/// send route has carried the type since roadmap M5a.
#[tokio::test]
async fn the_permission_request_is_the_send_routes_message_with_metas_exact_body() {
    let w = world(false).await;
    let key = w.h.tenant_key(TENANT, &[Scope::Send]).await;
    let text = "We would like to call you to help support your query on Order No: ON-12853.";
    for (recipient, wire) in [
        (
            json!({"to": {"phone": "+17863476655"}}),
            json!({"to": "+17863476655"}),
        ),
        // By BSUID alone: no `to` on the wire (the phone number may be
        // absent, AGENTS.md).
        (
            json!({"to": {"user_id": "US.13491208655302741918"}}),
            json!({"recipient": "US.13491208655302741918"}),
        ),
    ] {
        let mut body = json!({
            "type": "interactive",
            "interactive": {"type": "call_permission_request",
                            "body": {"text": text},
                            "action": {"name": "call_permission_request"}}
        });
        body.as_object_mut()
            .unwrap()
            .extend(recipient.as_object().unwrap().clone());
        w.h.graph.push_json(
            200,
            json!({"messaging_product": "whatsapp",
                   "contacts": [{"input": "+1-408-555-1234", "wa_id": "14085551234"}],
                   "messages": [{"id": "wamid.gBGGFlaCmZ9plHrf2Mh-o"}]}),
        );
        let reply = w.h.call(
            Call::new(Method::POST, format!("/v1/numbers/{PN}/messages"))
                .key(&key)
                .json(&body),
        )
        .await;
        assert_eq!(reply.status, StatusCode::ACCEPTED, "{body}: {}", reply.text);
        let sent = w.h.graph.last_request().unwrap();
        assert_eq!(sent.method, Method::POST);
        assert_eq!(sent.path(), format!("/v25.0/{PN}/messages"));
        assert_eq!(sent.bearer(), Some(TOKEN));
        let mut expected = json!({
            "messaging_product": "whatsapp", "recipient_type": "individual",
            "type": "interactive",
            "interactive": {"type": "call_permission_request",
                            "action": {"name": "call_permission_request"},
                            "body": {"text": text}}
        });
        expected
            .as_object_mut()
            .unwrap()
            .extend(wire.as_object().unwrap().clone());
        assert_eq!(sent.json().unwrap(), expected);
    }
    assert_eq!(w.h.graph.remaining(), 0);
}
