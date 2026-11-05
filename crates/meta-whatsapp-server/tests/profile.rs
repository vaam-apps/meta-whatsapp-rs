//! The profile picture and the display name (scope `numbers`;
//! docs/design/server.md, section 4.2; roadmap M5c2): a picture set from
//! the number's own completed resumable upload, a display name requested
//! of Meta and its status read. Each route's exact request to Meta, the
//! answer built from Meta's documented examples, every bound checked
//! before any request, and the design's M5.1 and M5.2: another tenant's
//! number, and another tenant's (or number's) upload, are `404` with
//! nothing asked of Meta.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::AppId;
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
/// Another number of the same tenant's WABA.
const PN_SECOND: &str = "106540352242999";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TENANT: &str = "merchant-43";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242923";
const OTHER_TOKEN: &str = "EAAG-other-merchant-token";
const APP_ID: &str = "634974688087057";
/// The session id and handle shapes of the Graph guide
/// (`docs/graph-api/guides/upload`), and `4::aW...` of the profile page's
/// example (`business-profiles`).
const SESSION: &str = "upload:MTphdHRhY2htZW50OjZiMDI1ZWI3";
const HANDLE: &str = "4::aW...";

async fn connected() -> (Harness, String) {
    let h = Harness::with_settings(Settings {
        app_id: Some(AppId::new(APP_ID).unwrap()),
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN, PN_SECOND], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], OTHER_TOKEN)
        .await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers, Scope::Media]).await;
    (h, key)
}

/// An upload of `len` bytes of `file_type` on `pn` of `key`'s tenant, its
/// bytes sent when `complete`: its upload id.
async fn upload(h: &Harness, pn: &str, key: &str, file_type: &str, complete: bool) -> String {
    h.graph.push_json(200, json!({"id": SESSION}));
    let started = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{pn}/media-uploads"))
                .key(key)
                .json(&json!({"file_name": "logo", "file_length": 12, "file_type": file_type})),
        )
        .await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.text);
    let id = started.json()["upload_id"].as_str().unwrap().to_owned();
    if complete {
        h.graph.push_json(200, json!({"h": HANDLE}));
        let chunk = h
            .call(
                Call::new(
                    Method::POST,
                    format!("/v1/numbers/{pn}/media-uploads/{id}?file_offset=0"),
                )
                .key(key)
                .header("content-type", "application/octet-stream")
                .body(Body::from(b"png bytes...".to_vec())),
            )
            .await;
        assert_eq!(chunk.status, StatusCode::OK, "{}", chunk.text);
        assert_eq!(chunk.json()["handle"], HANDLE);
    }
    id
}

fn patch(pn: &str, key: &str, body: &Value) -> Call {
    Call::new(Method::PATCH, format!("/v1/numbers/{pn}/profile"))
        .key(key)
        .json(body)
}

/// business-profiles, "Get your profile via the API" example response.
fn lucky_shrub() -> Value {
    json!({
        "data": [{
            "about": "Succulent specialists!",
            "address": "1 Hacker Way, Menlo Park, CA 94025",
            "description": "At Lucky Shrub, we specialize in providing a...",
            "email": "lucky@luckyshrub.com",
            "profile_picture_url": "https://pps.whatsapp.net/v/t61.24...",
            "websites": ["https://www.luckyshrub.com/"],
            "vertical": "RETAIL",
            "messaging_product": "whatsapp"
        }]
    })
}

// ─── The profile picture ─────────────────────────────────────────────────

/// Roadmap M5c2, decisive: the picture is the handle of the number's own
/// completed upload (the profile page's `profile_picture_handle`), sent in
/// the one `POST` of the change with the other fields, then the profile is
/// read, its `profile_picture_url` answered. Dropping the picture from the
/// request, or sending the upload's id instead of its handle, fails the
/// exact body.
#[tokio::test]
async fn a_picture_is_the_handle_of_the_numbers_own_completed_upload() {
    let (h, key) = connected().await;
    let id = upload(&h, PN, &key, "image/png", true).await;
    let before = h.graph.requests().len();
    h.graph.push_json(200, json!({"success": true}));
    h.graph.push_json(200, lucky_shrub());
    let reply = h
        .call(patch(
            PN,
            &key,
            &json!({"about": "Succulent specialists!", "profile_picture_upload_id": id}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({
            "about": "Succulent specialists!",
            "address": "1 Hacker Way, Menlo Park, CA 94025",
            "description": "At Lucky Shrub, we specialize in providing a...",
            "email": "lucky@luckyshrub.com",
            "profile_picture_url": "https://pps.whatsapp.net/v/t61.24...",
            "websites": ["https://www.luckyshrub.com/"],
            "vertical": "RETAIL"
        })
    );
    assert!(!reply.text.contains(HANDLE), "the handle stays in");
    let requests = h.graph.requests();
    let [posted, read] = &requests[before..] else {
        panic!("{requests:?}")
    };
    assert_eq!(posted.method, Method::POST);
    assert_eq!(
        posted.path(),
        format!("/v25.0/{PN}/whatsapp_business_profile")
    );
    assert_eq!(posted.bearer(), Some(TOKEN));
    assert_eq!(
        posted.json().unwrap(),
        json!({"messaging_product": "whatsapp", "about": "Succulent specialists!",
               "profile_picture_handle": HANDLE})
    );
    assert_eq!(read.method, Method::GET);
    assert_eq!(h.graph.remaining(), 0);

    // The picture alone is a change too (one `POST`), the upload reusable.
    h.graph.push_json(200, json!({"success": true}));
    h.graph.push_json(200, lucky_shrub());
    let again = h
        .call(patch(PN, &key, &json!({"profile_picture_upload_id": id})))
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    let requests = h.graph.requests();
    assert_eq!(
        requests[requests.len() - 2].json().unwrap(),
        json!({"messaging_product": "whatsapp", "profile_picture_handle": HANDLE})
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// Roadmap M5c2, decisive (the design's M5.1 and M5.2): an upload of
/// another tenant's number, of another number of the same tenant's, and
/// one nobody made are one answer, `404 not_found`, with zero requests to
/// Meta, and the answer names neither the upload nor a handle. Reading the
/// record without its tenant and number fails the first two.
#[tokio::test]
async fn a_picture_upload_of_another_tenant_or_number_is_not_found_with_zero_requests() {
    let (h, key) = connected().await;
    let other_key = h
        .tenant_key(OTHER_TENANT, &[Scope::Numbers, Scope::Media])
        .await;
    let theirs = upload(&h, OTHER_PN, &other_key, "image/png", true).await;
    let second = upload(&h, PN_SECOND, &key, "image/png", true).await;
    let mine = upload(&h, PN, &key, "image/png", true).await;
    let nobodys = "up_00000000000000000000000000000000".to_owned();
    let asked = h.graph.requests().len();
    let reads = h.kv.vault_reads();
    let mut answers = Vec::new();
    for (which, id) in [
        ("another tenant's", &theirs),
        ("another number's", &second),
        ("nobody's", &nobodys),
    ] {
        let reply = h
            .call(patch(PN, &key, &json!({"profile_picture_upload_id": id})))
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{which}: {}",
            reply.text
        );
        assert!(
            !reply.text.contains(id.as_str()) && !reply.text.contains(HANDLE),
            "{which}: {}",
            reply.text
        );
        let mut body = reply.json();
        body["error"]["request_id"] = Value::Null;
        answers.push(body);
    }
    assert_eq!(answers[0], answers[1]);
    assert_eq!(answers[1], answers[2], "the same answer for each");
    assert_eq!(
        h.graph.requests().len(),
        asked,
        "Meta was asked: {:?}",
        &h.graph.requests()[asked..]
    );
    assert!(
        h.kv.vault_reads() > reads,
        "the number's token is read (step 5), the upload decides after it"
    );
    // The other tenant, with its own number and its own upload, is served;
    // ours is not found for it.
    h.graph.push_json(200, json!({"success": true}));
    h.graph.push_json(200, lucky_shrub());
    let served = h
        .call(patch(
            OTHER_PN,
            &other_key,
            &json!({"profile_picture_upload_id": theirs}),
        ))
        .await;
    assert_eq!(served.status, StatusCode::OK, "{}", served.text);
    let requests = h.graph.requests();
    assert_eq!(requests[requests.len() - 2].bearer(), Some(OTHER_TOKEN));
    let asked = h.graph.requests().len();
    let refused = h
        .call(patch(
            OTHER_PN,
            &other_key,
            &json!({"profile_picture_upload_id": mine}),
        ))
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.text);
    assert_eq!(h.graph.requests().len(), asked);
    assert_eq!(h.graph.remaining(), 0);
}

/// An upload that is not an image, one that is not complete, and a name
/// that is no upload id are `422` on the field, with zero requests, and a
/// handle itself is not a field of the change.
#[tokio::test]
async fn a_picture_that_is_no_complete_image_upload_is_refused_before_meta() {
    let (h, key) = connected().await;
    let pdf = upload(&h, PN, &key, "application/pdf", true).await;
    let video = upload(&h, PN, &key, "video/mp4", true).await;
    let incomplete = upload(&h, PN, &key, "image/jpeg", false).await;
    let asked = h.graph.requests().len();
    for (body, field) in [
        (
            json!({"profile_picture_upload_id": pdf}),
            "profile_picture_upload_id",
        ),
        (
            json!({"profile_picture_upload_id": video}),
            "profile_picture_upload_id",
        ),
        (
            json!({"profile_picture_upload_id": incomplete}),
            "profile_picture_upload_id",
        ),
        (
            json!({"profile_picture_upload_id": "not-an-id"}),
            "profile_picture_upload_id",
        ),
        (
            json!({"profile_picture_upload_id": HANDLE}),
            "profile_picture_upload_id",
        ),
        (
            json!({"profile_picture_upload_id": ""}),
            "profile_picture_upload_id",
        ),
        (json!({"profile_picture_upload_id": 7}), "body"),
        // Meta's own field: a handle cannot be checked against its tenant.
        (json!({"profile_picture_handle": HANDLE}), "body"),
    ] {
        let reply = h.call(patch(PN, &key, &body)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
        assert_eq!(reply.json()["error"]["may_have_been_sent"], false);
        assert!(!reply.text.contains(HANDLE), "{body}: {}", reply.text);
    }
    assert_eq!(h.graph.requests().len(), asked, "nothing was sent");
    assert_eq!(h.graph.remaining(), 0);
}

/// A change that Meta refuses answers Meta's refusal (no Meta text), and
/// the picture's handle is in no answer.
#[tokio::test]
async fn metas_refusal_of_a_picture_is_answered_without_the_handle() {
    let (h, key) = connected().await;
    let id = upload(&h, PN, &key, "image/png", true).await;
    // reference/whatsapp-business-profile-api: "The profile picture handle
    // is invalid or expired", a 400.
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("The profile picture handle {HANDLE} is invalid or expired"),
                         "type": "OAuthException", "code": 100, "fbtrace_id": "A"}}),
    );
    let reply = h
        .call(patch(PN, &key, &json!({"profile_picture_upload_id": id})))
        .await;
    assert!(reply.status.is_client_error(), "{}", reply.text);
    assert!(!reply.text.contains(HANDLE), "{}", reply.text);
    assert!(!reply.text.contains("invalid or expired"), "{}", reply.text);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── The display name ────────────────────────────────────────────────────

fn request(pn: &str, key: &str, body: &Value) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{pn}/display-name"))
        .key(key)
        .json(body)
}

fn status(pn: &str, key: &str) -> Call {
    Call::get(format!("/v1/numbers/{pn}/display-name")).key(key)
}

/// Roadmap M5c2, decisive: `display-names`, "Update display name via
/// API": `POST /{pn}?new_display_name=Lucky%20Shrub` with the tenant's
/// token and no body, answered `202` with the name sent (trimmed). A
/// second request under the same key and body is the kept answer, with no
/// second request to Meta (a review is not asked twice); the same key with
/// another name is `422 idempotency_key_reused`, with nothing sent.
#[tokio::test]
async fn a_display_name_is_requested_as_meta_documents_and_once_per_key() {
    let (h, key) = connected().await;
    h.graph.push_json(200, json!({"success": true}));
    let call = || {
        request(PN, &key, &json!({"new_display_name": "  Lucky Shrub "}))
            .header("idempotency-key", "name-1")
    };
    let reply = h.call(call()).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    assert_eq!(reply.json(), json!({"new_display_name": "Lucky Shrub"}));
    let requests = h.graph.requests();
    let [asked] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(asked.method, Method::POST);
    assert_eq!(asked.path(), format!("/v25.0/{PN}"));
    assert_eq!(
        asked.query("new_display_name").as_deref(),
        Some("Lucky Shrub")
    );
    assert_eq!(asked.bearer(), Some(TOKEN));
    assert!(asked.json().is_none(), "no body: the name is a parameter");
    assert_eq!(h.graph.remaining(), 0);

    let replayed = h.call(call()).await;
    assert_eq!(replayed.status, StatusCode::ACCEPTED, "{}", replayed.text);
    assert_eq!(replayed.headers["idempotent-replayed"], "true");
    assert_eq!(replayed.json(), reply.json());
    assert_eq!(h.graph.requests().len(), 1, "no second review asked");

    let reused = h
        .call(
            request(PN, &key, &json!({"new_display_name": "Another Shrub"}))
                .header("idempotency-key", "name-1"),
        )
        .await;
    assert_eq!(
        (reused.status, reused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused"),
        "{}",
        reused.text
    );
    assert_eq!(h.graph.requests().len(), 1);
}

/// The name's bounds, before any request: trimmed, 1 to 256 characters, no
/// control character, nothing else in the body.
#[tokio::test]
async fn a_display_names_bounds_are_checked_before_any_request() {
    let (h, key) = connected().await;
    let longest = "é".repeat(256);
    h.graph.push_json(200, json!({"success": true}));
    let ok = h
        .call(request(PN, &key, &json!({"new_display_name": longest})))
        .await;
    assert_eq!(ok.status, StatusCode::ACCEPTED, "{}", ok.text);
    assert_eq!(h.graph.requests().len(), 1);
    for (body, field) in [
        (json!({"new_display_name": ""}), "new_display_name"),
        (json!({"new_display_name": "   "}), "new_display_name"),
        (
            json!({"new_display_name": "é".repeat(257)}),
            "new_display_name",
        ),
        (
            json!({"new_display_name": "Lucky\nShrub"}),
            "new_display_name",
        ),
        (
            json!({"new_display_name": "Lucky\u{0}"}),
            "new_display_name",
        ),
        (json!({}), "body"),
        (json!({"new_display_name": 7}), "body"),
        (json!({"new_display_name": "x", "pin": "123456"}), "body"),
    ] {
        let reply = h.call(request(PN, &key, &body)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
        assert_eq!(reply.json()["error"]["may_have_been_sent"], false);
    }
    assert_eq!(h.graph.requests().len(), 1, "nothing else was sent");
    assert_eq!(h.graph.remaining(), 0);
}

/// `display-names`, "Update display name via API": the status example
/// answered by Meta for `fields=new_display_name,new_name_status`, here
/// with the four fields the route asks, read as Meta names them.
#[tokio::test]
async fn the_status_reads_the_four_fields_the_page_names() {
    let (h, key) = connected().await;
    h.graph.push_json(
        200,
        json!({"new_display_name": "New Lucky Shrub", "new_name_status": "PENDING_REVIEW",
               "id": PN}),
    );
    let reply = h.call(status(PN, &key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"verified_name": null, "name_status": null,
               "new_display_name": "New Lucky Shrub", "new_name_status": "PENDING_REVIEW"})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{PN}"));
    assert_eq!(
        request.query("fields").as_deref(),
        Some("verified_name,name_status,new_display_name,new_name_status")
    );
    assert_eq!(request.bearer(), Some(TOKEN));

    // "Get display name and display name status via API", the example.
    h.graph.push_json(
        200,
        json!({"verified_name": "Lucky Shrub", "name_status": "APPROVED", "id": PN}),
    );
    let approved = h.call(status(PN, &key)).await;
    assert_eq!(
        approved.json(),
        json!({"verified_name": "Lucky Shrub", "name_status": "APPROVED",
               "new_display_name": null, "new_name_status": null})
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// M5.1 and M5.2 for both routes: another tenant's number is `404` and a
/// key without the scope `403`, each with zero requests to Meta, and the
/// token of the number's holder is not read for a foreign tenant.
#[tokio::test]
async fn the_display_name_routes_are_the_numbers_holders_only() {
    let (h, key) = connected().await;
    let other_key = h.tenant_key(OTHER_TENANT, &[Scope::Numbers]).await;
    let send_only = h.tenant_key(TENANT, &[Scope::Send]).await;
    let reads = h.kv.vault_reads();
    for call in [
        request(PN, &other_key, &json!({"new_display_name": "Lucky Shrub"})),
        status(PN, &other_key),
    ] {
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{}",
            reply.text
        );
    }
    for call in [
        request(PN, &send_only, &json!({"new_display_name": "Lucky Shrub"})),
        status(PN, &send_only),
    ] {
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{}",
            reply.text
        );
    }
    assert_eq!(h.kv.vault_reads(), reads, "no vault read for a refusal");
    assert!(h.graph.requests().is_empty(), "nothing asked of Meta");
    // The holder is served.
    h.graph
        .push_json(200, json!({"id": PN, "verified_name": "Lucky Shrub"}));
    let served = h.call(status(PN, &key)).await;
    assert_eq!(served.status, StatusCode::OK, "{}", served.text);
}

/// Meta's refusal of a display name is its error with no Meta text, and a
/// timeout may have gone through: `504 timeout`, the request not replayed
/// (one request), and the same key answers the kept timeout again, with no
/// second send.
#[tokio::test]
async fn metas_refusal_and_a_timeout_are_the_error_model() {
    let (h, key) = connected().await;
    h.graph.push_json(
        400,
        json!({"error": {"message": "Display name Lucky Shrub breaks the guidelines",
                         "type": "OAuthException", "code": 100, "fbtrace_id": "A"}}),
    );
    let refused = h
        .call(request(
            PN,
            &key,
            &json!({"new_display_name": "Lucky Shrub"}),
        ))
        .await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    assert!(
        !refused.text.contains("breaks the guidelines"),
        "{}",
        refused.text
    );
    assert_eq!(refused.json()["error"]["may_have_been_sent"], false);

    h.graph.push_error(|| TransportError::Timeout);
    let cut = h
        .call(
            request(PN, &key, &json!({"new_display_name": "Lucky Shrub"}))
                .header("idempotency-key", "name-cut"),
        )
        .await;
    assert_eq!(
        (cut.status, cut.code().as_str()),
        (StatusCode::GATEWAY_TIMEOUT, "timeout"),
        "{}",
        cut.text
    );
    assert_eq!(cut.json()["error"]["may_have_been_sent"], true);
    assert_eq!(
        h.graph.requests().len(),
        2,
        "a timed-out request is not replayed"
    );
    let repeated = h
        .call(
            request(PN, &key, &json!({"new_display_name": "Lucky Shrub"}))
                .header("idempotency-key", "name-cut"),
        )
        .await;
    // A kept failure that may have gone through is the answer again (a
    // new send is never made under the key).
    assert_eq!(
        (repeated.status, repeated.code().as_str()),
        (StatusCode::GATEWAY_TIMEOUT, "timeout"),
        "{}",
        repeated.text
    );
    assert_eq!(repeated.headers["idempotent-replayed"], "true");
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(h.graph.remaining(), 0);
}
