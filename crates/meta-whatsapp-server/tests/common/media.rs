//! The received-media scenario (roadmap M2e, `OPEN_QUESTIONS.md` #43),
//! run on memory (`media.rs`) and on Postgres (`live_postgres.rs`).

use http_body_util::BodyExt as _;
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use super::capture::sha256_hex;
use super::meta::{EXAMPLE_PN, EXAMPLE_WABA, bytes, with_ids};
use super::{Call, Harness};

/// The media id of Meta's image example (`webhooks/reference/messages/image`).
pub const RECEIVED_MEDIA_ID: &str = "1003383421387256";
/// The second tenant's WABA and number.
const WABA_B: &str = "102290129340399";
const PN_B: &str = "106540352242923";
/// The download URL shape of `business-phone-numbers/media`.
const URL: &str = "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1003383421387256&ext=1&hash=abc";

/// Meta's image example, received by `pn` of `waba`, dated now.
pub fn image(waba: &str, pn: &str) -> Value {
    with_ids(super::meta::fixture("messages/image.json"), waba, pn)
}

/// `GET /{media-id}`'s answer (`business-phone-numbers/media`, "Response
/// syntax") for Meta's image example, its file `file`.
fn info(file: &[u8]) -> Value {
    json!({"messaging_product": "whatsapp", "url": URL, "mime_type": "image/jpeg",
           "sha256": sha256_hex(file), "file_size": file.len().to_string(),
           "id": RECEIVED_MEDIA_ID})
}

/// `method` on the received media id, on `pn`, with `key`.
fn on_media(method: Method, pn: &str, key: &str) -> Call {
    Call::new(
        method,
        format!("/v1/numbers/{pn}/media/{RECEIVED_MEDIA_ID}"),
    )
    .key(key)
}

/// `404 not_found` for a download and a deletion of the received media id
/// on `pn` with `key`, and nothing asked of Meta.
async fn not_found_unasked(h: &Harness, pn: &str, key: &str, which: &str) {
    let asked = h.graph.requests().len();
    for method in [Method::GET, Method::DELETE] {
        let reply = h.call(on_media(method.clone(), pn, key)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{which}: {method}: {}",
            reply.text
        );
        assert!(
            !reply.text.contains(RECEIVED_MEDIA_ID),
            "{which}: the answer names the id"
        );
    }
    assert_eq!(
        h.graph.requests().len(),
        asked,
        "{which}: Meta was asked: {:?}",
        &h.graph.requests()[asked..]
    );
}

/// Roadmap M2e over HTTP, decisive: Meta's image example reaches
/// merchant-a's number through `/webhooks/meta`. merchant-b's key, on its
/// own number, gets `404 not_found` for that media id, download and
/// deletion alike, with zero requests to Meta (the same answer as an id
/// Meta refuses); merchant-a's key downloads it with `GET /{media_id}`,
/// no `phone_number_id`, then Meta's URL, both with merchant-a's token,
/// the file verified. Once the number moved to merchant-b with its WABA,
/// merchant-b's key on that number still gets `404` with zero requests.
pub async fn another_tenants_received_media_id_is_not_found_with_no_request(h: &Harness) {
    h.tenant("merchant-a").await;
    h.tenant("merchant-b").await;
    h.connect("merchant-a", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-A")
        .await;
    h.connect("merchant-b", WABA_B, &[PN_B], "TOKEN-OF-B").await;
    let a_key = h.tenant_key("merchant-a", &[Scope::Media]).await;
    let b_key = h.tenant_key("merchant-b", &[Scope::Media]).await;
    let received = h.webhook(&bytes(&image(EXAMPLE_WABA, EXAMPLE_PN))).await;
    assert_eq!(received.status, StatusCode::OK, "{}", received.text);

    // Another tenant's key, on its own number: not found, nothing asked.
    not_found_unasked(h, PN_B, &b_key, "another tenant").await;
    // An id Meta refuses on that number answers the same.
    h.graph.push_json(
        400,
        json!({"error": {"message": "Unsupported get request.", "type": "GraphMethodException",
                         "code": 100, "error_subcode": 33, "fbtrace_id": "A"}}),
    );
    let refused = h
        .call(Call::get(format!("/v1/numbers/{PN_B}/media/1003383421387299")).key(&b_key))
        .await;
    let hidden = h.call(on_media(Method::GET, PN_B, &b_key)).await;
    let (mut refused, mut hidden) = (refused.json(), hidden.json());
    refused["error"]["request_id"] = Value::Null;
    hidden["error"]["request_id"] = Value::Null;
    assert_eq!(hidden, refused, "a received id looks like a missing one");

    // The receiving tenant's key: no `phone_number_id`, its own token.
    let file = b"\xff\xd8\xff\xe0 the customer's photo".to_vec();
    h.graph.push_json(200, info(&file));
    h.graph.push_bytes(200, "image/jpeg", file.clone());
    let asked = h.graph.requests().len();
    let downloaded = h
        .internal
        .clone()
        .oneshot(on_media(Method::GET, EXAMPLE_PN, &a_key).build())
        .await
        .unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(
        downloaded.headers()["x-wa-sha256"],
        sha256_hex(&file).as_str()
    );
    let body = downloaded.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], &file[..]);
    let requests = h.graph.requests();
    let [lookup, url] = &requests[asked..] else {
        panic!("{requests:?}")
    };
    assert_eq!(
        (lookup.method.clone(), lookup.path()),
        (Method::GET, format!("/v25.0/{RECEIVED_MEDIA_ID}").as_str())
    );
    assert_eq!(
        lookup.query("phone_number_id"),
        None,
        "a received media id is asked without phone_number_id"
    );
    assert_eq!(lookup.bearer(), Some("TOKEN-OF-A"));
    assert_eq!(url.url.as_str(), URL);
    assert_eq!(url.bearer(), Some("TOKEN-OF-A"));

    // The number moves to merchant-b, with its WABA.
    assert!(
        h.store
            .unbind_waba(&WabaId::new(EXAMPLE_WABA).unwrap())
            .await
            .unwrap()
    );
    h.connect("merchant-b", EXAMPLE_WABA, &[EXAMPLE_PN], "TOKEN-OF-B")
        .await;
    not_found_unasked(h, EXAMPLE_PN, &b_key, "the number's new holder").await;
    assert_eq!(h.graph.remaining(), 0);
}
