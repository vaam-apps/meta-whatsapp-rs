//! Numbers and profile routes (scope `numbers`): the exact Graph request
//! each makes, with the tenant's token, and the answer built from Meta's
//! documented examples.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";

async fn connected() -> (Harness, String) {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
    (h, key)
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

fn lucky_shrub_view() -> Value {
    json!({
        "about": "Succulent specialists!",
        "address": "1 Hacker Way, Menlo Park, CA 94025",
        "description": "At Lucky Shrub, we specialize in providing a...",
        "email": "lucky@luckyshrub.com",
        "websites": ["https://www.luckyshrub.com/"],
        "vertical": "RETAIL"
    })
}

#[tokio::test]
async fn wabas_and_numbers_list_the_tenants_bindings_only() {
    let (h, key) = connected().await;
    h.tenant("merchant-43").await;
    h.connect("merchant-43", "999", &["998"], "OTHER").await;
    let wabas = h.call(Call::get("/v1/wabas").key(&key)).await;
    assert_eq!(wabas.status, StatusCode::OK);
    let wabas = wabas.json();
    assert_eq!(wabas["data"].as_array().unwrap().len(), 1);
    assert_eq!(wabas["data"][0]["waba_id"], WABA);
    assert_eq!(wabas["next_cursor"], Value::Null);
    let numbers = h.call(Call::get("/v1/numbers").key(&key)).await.json();
    assert_eq!(numbers["data"].as_array().unwrap().len(), 1);
    assert_eq!(numbers["data"][0]["phone_number_id"], PN);
    assert_eq!(numbers["data"][0]["waba_id"], WABA);
    assert_eq!(numbers["data"][0]["status"], "connected");
    assert!(
        h.graph.requests().is_empty(),
        "from the bindings, not from Meta"
    );
}

#[tokio::test]
async fn a_numbers_details_come_from_meta_with_the_tenants_token() {
    let (h, key) = connected().await;
    // solution-providers/manage-phone-numbers (id, display number, name
    // status) and the hosted-es listing (throughput), as documented.
    h.graph.push_json(
        200,
        json!({
            "id": PN,
            "display_phone_number": "+1 631-555-1111",
            "verified_name": "John's Cake Shop",
            "quality_rating": "GREEN",
            "name_status": "APPROVED",
            "throughput": {"level": "STANDARD"}
        }),
    );
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({
            "phone_number_id": PN,
            "waba_id": WABA,
            "status": "connected",
            "display_phone_number": "+1 631-555-1111",
            "verified_name": "John's Cake Shop",
            "quality_rating": "GREEN",
            "name_status": "APPROVED",
            "throughput": {"level": "STANDARD"}
        })
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{PN}"));
    assert_eq!(
        request.query("fields").as_deref(),
        Some("display_phone_number,verified_name,quality_rating,name_status,throughput")
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);
}

#[tokio::test]
async fn the_profile_is_read_with_the_documented_fields() {
    let (h, key) = connected().await;
    h.graph.push_json(200, lucky_shrub());
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/profile")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json(), lucky_shrub_view());
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(
        request.path(),
        format!("/v25.0/{PN}/whatsapp_business_profile")
    );
    assert_eq!(
        request.query("fields").as_deref(),
        Some("about,address,description,email,websites,vertical")
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);
}

#[tokio::test]
async fn a_profile_patch_posts_only_the_given_fields_then_reads_the_profile() {
    let (h, key) = connected().await;
    h.graph.push_json(200, json!({"success": true}));
    h.graph.push_json(200, lucky_shrub());
    let reply = h
        .call(
            Call::new(Method::PATCH, format!("/v1/numbers/{PN}/profile"))
                .key(&key)
                .json(
                    &json!({"about": "Succulent specialists!", "vertical": "RETAIL",
                              "websites": ["https://www.luckyshrub.com/"]}),
                ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json(), lucky_shrub_view());
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(
        requests[0].path(),
        format!("/v25.0/{PN}/whatsapp_business_profile")
    );
    assert_eq!(requests[0].bearer(), Some(TOKEN));
    assert_eq!(
        requests[0].json().unwrap(),
        json!({"messaging_product": "whatsapp", "about": "Succulent specialists!",
               "websites": ["https://www.luckyshrub.com/"], "vertical": "RETAIL"})
    );
    assert_eq!(requests[1].method, Method::GET);
    assert_eq!(h.graph.remaining(), 0);
}

#[tokio::test]
async fn an_empty_patch_writes_nothing_and_a_bad_one_sends_nothing() {
    let (h, key) = connected().await;
    h.graph.push_json(200, lucky_shrub());
    let reply = h
        .call(
            Call::new(Method::PATCH, format!("/v1/numbers/{PN}/profile"))
                .key(&key)
                .json(&json!({})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(h.graph.requests().len(), 1);
    assert_eq!(h.graph.requests()[0].method, Method::GET);

    for (body, field) in [
        (json!({"address": "x".repeat(257)}), "address"),
        (json!({"vertical": "UNDEFINED"}), "vertical"),
        (json!({"vertical": 7}), "body"),
        (json!({"profile_picture_handle": "h"}), "body"),
    ] {
        let reply = h
            .call(
                Call::new(Method::PATCH, format!("/v1/numbers/{PN}/profile"))
                    .key(&key)
                    .json(&body),
            )
            .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(reply.code(), "invalid_request");
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
        assert_eq!(reply.json()["error"]["may_have_been_sent"], false);
    }
    assert_eq!(
        h.graph.requests().len(),
        1,
        "no request for a refused patch"
    );
    assert_eq!(h.graph.remaining(), 0);
}

#[tokio::test]
async fn disconnecting_unsubscribes_first_and_deletes_only_after() {
    let (h, key) = connected().await;
    // Meta refuses: nothing deleted.
    h.graph.push_json(
        500,
        json!({"error": {"message": "An unexpected error occurred. Please retry your request",
                         "type": "GraphMethodException", "code": 2, "is_transient": true,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let failed = h
        .call(Call::new(Method::DELETE, format!("/v1/wabas/{WABA}")).key(&key))
        .await;
    assert_eq!(
        (failed.status, failed.code().as_str()),
        (StatusCode::BAD_GATEWAY, "service_unavailable")
    );
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_some());
    assert!(h.store.waba(&WabaId::new(WABA)).await.unwrap().is_some());
    assert!(
        h.store
            .number(&PhoneNumberId::new(PN))
            .await
            .unwrap()
            .is_some()
    );

    // Meta unsubscribes: then the token and bindings go.
    h.graph.push_json(200, json!({"success": true}));
    let done = h
        .call(Call::new(Method::DELETE, format!("/v1/wabas/{WABA}")).key(&key))
        .await;
    assert_eq!(done.status, StatusCode::NO_CONTENT, "{}", done.text);
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, Method::DELETE);
        assert_eq!(request.path(), format!("/v25.0/{WABA}/subscribed_apps"));
        assert_eq!(request.bearer(), Some(TOKEN));
    }
    assert_eq!(h.graph.remaining(), 0);
    assert!(h.vault.get(&WabaId::new(WABA)).await.unwrap().is_none());
    assert!(h.store.waba(&WabaId::new(WABA)).await.unwrap().is_none());
    assert!(
        h.store
            .number(&PhoneNumberId::new(PN))
            .await
            .unwrap()
            .is_none()
    );
    let gone = h
        .call(Call::get(format!("/v1/numbers/{PN}")).key(&key))
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_timeout_may_have_taken_effect() {
    let (h, key) = connected().await;
    h.graph
        .push_error(|| meta_whatsapp_rs::core::error::TransportError::Timeout);
    let reply = h
        .call(Call::new(Method::DELETE, format!("/v1/wabas/{WABA}")).key(&key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::GATEWAY_TIMEOUT, "timeout")
    );
    assert_eq!(reply.json()["error"]["may_have_been_sent"], true);
    assert!(
        h.vault.get(&WabaId::new(WABA)).await.unwrap().is_some(),
        "nothing deleted"
    );
    assert_eq!(h.graph.remaining(), 0);
}
