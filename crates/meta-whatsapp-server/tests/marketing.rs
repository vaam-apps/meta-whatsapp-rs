//! Roadmap M5h: the Marketing Messages API, max price, Click-to-WhatsApp
//! welcome message sequences and the max price agreement, through the
//! service (scope `marketing`, and an admin key for the agreement;
//! docs/design/server.md, section 4.2, D230 to D234).
//!
//! Each test sends the requests the pages document and checks the exact
//! requests that reach Meta (method, path, query, the tenant's token, the
//! JSON body) and `remaining() == 0`. The design's M5.2: a template or a
//! sequence is its WABA's, and a refusal sends no request that acts. M5.1
//! (another tenant's number or WABA, zero vault reads) is `tests/authz.rs`,
//! which iterates every route of the document, these included. The audit
//! events and the logs are `tests/common/capture.rs`'s `marketing`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::testing::RecordedBody;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TENANT: &str = "merchant-43";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242924";
const TEMPLATE: &str = "1234567890123456";
const SEQUENCE: &str = "186473890";
const BUSINESS: &str = "529216684107530";
const BUSINESS_TOKEN: &str = "EAAB-the-operators-business-token";

/// The harness, with the legal acts on or off; a tenant with a WABA and a
/// number, another with one; a key of the tenant for `marketing`.
async fn connected(legal_acts: bool) -> (Harness, String) {
    let h = Harness::with_settings(Settings {
        legal_acts,
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], "EAAG-other")
        .await;
    let key = h.tenant_key(TENANT, &[Scope::Marketing]).await;
    (h, key)
}

fn call(method: Method, path: String, key: &str) -> Call {
    Call::new(method, path).key(key)
}

/// The lookup of a template id by its name, then in the WABA's own list.
fn script_template(h: &Harness, id: &str, in_waba: bool) {
    h.graph
        .push_json(200, json!({"name": "seasonal_sale_promo", "id": id}));
    let data = if in_waba {
        json!([{"id": id, "name": "seasonal_sale_promo"}])
    } else {
        json!([])
    };
    h.graph.push_json(200, json!({ "data": data }));
}

fn sequence_json(id: &str) -> Value {
    json!([{"sequence_id": id, "name": "Driver Sign up",
            "welcome_message_sequence": {"text": "Welcome",
                "autofill_message": {"content": "Hello! Can I get more info on this!"},
                "ice_breakers": [{"title": "Quick reply 1"}]},
            "is_used_in_ad": false}])
}

fn sent_paths(h: &Harness) -> Vec<String> {
    h.graph
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path()))
        .collect()
}

// ─── The send ────────────────────────────────────────────────────────────

/// marketing-messages/send-marketing-messages: the send through
/// `/marketing_messages` with the options as Meta names them, the tenant's
/// token, `202` with the accepted message; a repeat under the same
/// `Idempotency-Key` is Meta's one request. Decisive: the endpoint and the
/// options' mapping.
#[tokio::test]
async fn a_marketing_send_goes_through_the_marketing_endpoint_with_its_options() {
    let (h, key) = connected(false).await;
    let accepted = json!({"messaging_product": "whatsapp",
        "contacts": [{"input": "+16505551234", "wa_id": "16505551234"}],
        "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]});
    h.graph.push_json(200, accepted);
    let body = json!({
        "to": {"phone": "+16505551234"},
        "template": {"name": "seasonal_sale_promo", "language": {"code": "en_US"}},
        "product_policy": "STRICT",
        "message_activity_sharing": true,
        "bid_spec": {"per_message_bid_multiplier": 1.5}
    });
    let send = || {
        call(
            Method::POST,
            format!("/v1/numbers/{PN}/marketing-messages"),
            &key,
        )
        .header("idempotency-key", "promo-1")
        .json(&body)
    };
    let reply = h.call(send()).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    assert_eq!(
        reply.json()["message_id"],
        "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"
    );
    assert_eq!(reply.json()["contacts"][0]["wa_id"], "16505551234");
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{PN}/marketing_messages"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        request.json(),
        Some(json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": "+16505551234",
            "type": "template",
            "template": {"name": "seasonal_sale_promo", "language": {"code": "en_US"}},
            "product_policy": "STRICT",
            "message_activity_sharing": true,
            "bid_spec": {"per_message_bid_multiplier": 1.5}
        }))
    );
    let again = h.call(send()).await;
    assert_eq!(again.status, StatusCode::ACCEPTED, "{}", again.text);
    assert_eq!(again.json(), reply.json());
    assert_eq!(h.graph.requests().len(), 1, "the repeat is the kept answer");
    assert_eq!(h.graph.remaining(), 0);
}

/// A send that is wrong is refused before any request, on its field; a
/// send cut by a timeout is `504` with `may_have_been_sent` and Meta is
/// asked once (never replayed). Decisive: `marketing_message`'s checks,
/// and the library's `send` not being idempotent.
#[tokio::test]
async fn a_wrong_send_is_refused_first_and_a_timeout_is_never_replayed() {
    let (h, key) = connected(false).await;
    let path = format!("/v1/numbers/{PN}/marketing-messages");
    let template = json!({"name": "seasonal_sale_promo", "language": {"code": "en_US"}});
    for (body, field) in [
        (
            json!({"to": {"phone": "16505551234"}, "template": template}),
            "to.phone",
        ),
        (json!({"to": {}, "template": template}), "to"),
        (
            json!({"to": {"phone": "+16505551234"}, "template": {"name": "x", "language": {"code": "en"}, "extra": 1}}),
            "template.extra",
        ),
        (
            json!({"to": {"phone": "+16505551234"}, "template": template,
                   "bid_spec": {"per_message_bid_multiplier": 0.0}}),
            "bid_spec.per_message_bid_multiplier",
        ),
        (
            json!({"to": {"phone": "+16505551234"}, "template": template, "product_policy": "NEVER"}),
            "body",
        ),
        (
            json!({"to": {"phone": "+16505551234"}, "template": template, "ttl": 5}),
            "body",
        ),
    ] {
        let reply = h
            .call(call(Method::POST, path.clone(), &key).json(&body))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
    }
    assert!(h.graph.requests().is_empty());
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h
        .call(
            call(Method::POST, path, &key)
                .json(&json!({"to": {"phone": "+16505551234"}, "template": template})),
        )
        .await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    assert_eq!(cut.json()["error"]["may_have_been_sent"], true);
    assert_eq!(h.graph.requests().len(), 1, "never replayed");
    assert_eq!(h.graph.remaining(), 0);
}

/// A catalog a template names is the number's WABA's, else `404` with only
/// the check asked (the send union's M5.2, shared). Decisive:
/// `admit_named_object` in `MarketingSends::send`.
#[tokio::test]
async fn a_catalog_of_another_waba_in_a_marketing_send_is_not_found_and_nothing_is_sent() {
    let (h, key) = connected(false).await;
    h.graph.push_json(
        200,
        json!({"data": [{"id": "194836987003835", "name": "Lucky Shrub"}]}),
    );
    let reply = h
        .call(
            call(
                Method::POST,
                format!("/v1/numbers/{PN}/marketing-messages"),
                &key,
            )
            .json(&json!({"to": {"phone": "+16505551234"},
                "template": {"name": "product_promo", "language": {"code": "en_US"},
                    "components": [{"type": "header", "parameters": [{"type": "product",
                        "product": {"product_retailer_id": "sku-1", "catalog_id": "999999999999999"}}]}]}})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    assert_eq!(
        sent_paths(&h),
        [format!("GET /v25.0/{WABA}/product_catalogs")]
    );
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Onboarding and the Cloud API switch ─────────────────────────────────

/// marketing-messages/onboarding: the WABA's status and its owner's terms
/// state, the two reads as the library makes them; the switch read and set
/// as the page writes it.
#[tokio::test]
async fn onboarding_and_the_cloud_api_switch_are_read_and_set_as_the_pages_write_them() {
    let (h, key) = connected(false).await;
    h.graph.push_json(
        200,
        json!({"marketing_messages_onboarding_status": "ELIGIBLE", "id": WABA}),
    );
    h.graph.push_json(
        200,
        json!({"owner_business_info": {"name": "WhatsApp PaidSend Testing", "id": BUSINESS,
               "marketing_messages_onboarding_status": {"status": "REQUEST_SENT", "time": "2025-08-13"}}}),
    );
    let reply = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/marketing/onboarding"),
            &key,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"onboarding_status": "ELIGIBLE", "owner_business": {"id": BUSINESS,
            "name": "WhatsApp PaidSend Testing", "terms_status": "REQUEST_SENT",
            "terms_time": "2025-08-13"}})
    );
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 2);
    for (request, fields) in requests.iter().zip([
        "marketing_messages_onboarding_status",
        "owner_business_info",
    ]) {
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.path(), format!("/v25.0/{WABA}"));
        assert_eq!(request.query("fields").as_deref(), Some(fields));
        assert_eq!(request.bearer(), Some(TOKEN));
    }
    h.graph.push_json(200, json!({"id": WABA}));
    let set = h
        .call(
            call(
                Method::PUT,
                format!("/v1/wabas/{WABA}/marketing/cloud-api-marketing"),
                &key,
            )
            .json(&json!({"disabled": true})),
        )
        .await;
    assert_eq!(set.status, StatusCode::NO_CONTENT, "{}", set.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{WABA}"));
    assert_eq!(
        request.json(),
        Some(json!({"disable_marketing_messages_on_cloud_api": true}))
    );
    h.graph.push_json(
        200,
        json!({"disable_marketing_messages_on_cloud_api": true, "id": WABA}),
    );
    let read = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/marketing/cloud-api-marketing"),
            &key,
        ))
        .await;
    assert_eq!(read.json(), json!({"disabled": true}));
    assert_eq!(
        h.graph.last_request().unwrap().query("fields").as_deref(),
        Some("disable_marketing_messages_on_cloud_api")
    );
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Max price ───────────────────────────────────────────────────────────

/// marketing-messages/pricing/duplicate-templates: the source looked for in
/// the WABA's templates, then the duplication as the page writes it; the
/// answer under Meta's names; a repeat under the same key is one
/// duplicate. A source of another WABA is `404` with only the lookup sent,
/// and a wrong request is refused with no request. Decisive:
/// `Templates::require_owned` in `duplicate_template`.
#[tokio::test]
async fn a_template_is_duplicated_at_another_price_only_when_the_wabas_own() {
    let (h, key) = connected(false).await;
    let duplicate = |name: &str| {
        call(
            Method::POST,
            format!("/v1/wabas/{WABA}/marketing/template-duplicates"),
            &key,
        )
        .json(&json!({"source_template_id": TEMPLATE, "name": name,
                      "optimization_spec": {"bid_amount": 87000}}))
    };
    script_template(&h, TEMPLATE, true);
    h.graph.push_json(
        200,
        json!({"id": "1733678867511493", "status": "APPROVED", "category": "MARKETING",
               "source_template_id": TEMPLATE}),
    );
    let reply = h
        .call(duplicate("seasonal_sale_promo_high_intent").header("idempotency-key", "dup-1"))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"id": "1733678867511493", "status": "APPROVED", "category": "MARKETING",
               "source_template_id": TEMPLATE})
    );
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 3, "{requests:?}");
    let post = &requests[2];
    assert_eq!(post.method, Method::POST);
    assert_eq!(post.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(post.bearer(), Some(TOKEN));
    assert_eq!(
        post.json(),
        Some(
            json!({"source_template_id": TEMPLATE, "name": "seasonal_sale_promo_high_intent",
                    "optimization_spec": {"bid_amount": 87000, "bid_strategy": "LOWEST_COST_WITH_BID_CAP"}})
        )
    );
    let again = h
        .call(duplicate("seasonal_sale_promo_high_intent").header("idempotency-key", "dup-1"))
        .await;
    assert_eq!(again.json(), reply.json());
    assert_eq!(h.graph.requests().len(), 3);
    // Another WABA's template: the lookup only.
    script_template(&h, TEMPLATE, false);
    let foreign = h.call(duplicate("another_name")).await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{}", foreign.text);
    assert_eq!(h.graph.requests().len(), 5, "no duplication");
    // Refused first.
    for (body, field) in [
        (
            json!({"source_template_id": "0123", "name": "n", "optimization_spec": {"bid_amount": 1}}),
            "source_template_id",
        ),
        (
            json!({"source_template_id": TEMPLATE, "name": "Not Valid", "optimization_spec": {"bid_amount": 1}}),
            "name",
        ),
        (
            json!({"source_template_id": TEMPLATE, "name": "n", "optimization_spec": {"bid_amount": 0}}),
            "optimization_spec.bid_amount",
        ),
        (
            json!({"source_template_id": TEMPLATE, "name": "n",
                   "optimization_spec": {"bid_amount": 1, "bid_strategy": "MANUAL"}}),
            "optimization_spec.bid_strategy",
        ),
    ] {
        let reply = h
            .call(
                call(
                    Method::POST,
                    format!("/v1/wabas/{WABA}/marketing/template-duplicates"),
                    &key,
                )
                .json(&body),
            )
            .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
    }
    assert_eq!(h.graph.requests().len(), 5);
    assert_eq!(h.graph.remaining(), 0);
}

/// A template's max price: read (Meta's list shape of the overrides),
/// set with the page's update body, cleared with the two `null`s; each
/// after the template is found among the WABA's, and `404` with only the
/// lookup sent when it is not.
#[tokio::test]
async fn a_templates_max_price_is_read_set_and_cleared_only_for_the_wabas_own() {
    let (h, key) = connected(false).await;
    let path = format!("/v1/wabas/{WABA}/marketing/templates/{TEMPLATE}/max-price");
    script_template(&h, TEMPLATE, true);
    h.graph.push_json(
        200,
        json!({"optimization_spec": {"bid_strategy": "LOWEST_COST_WITH_BID_CAP", "bid_amount": 87000,
               "bid_country_multiplier_overrides": [{"key": "IN", "value": 1.2}, {"key": "BR", "value": 1.5}]},
               "id": TEMPLATE}),
    );
    let read = h.call(call(Method::GET, path.clone(), &key)).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.text);
    assert_eq!(
        read.json(),
        json!({"bid_amount": 87000, "bid_strategy": "LOWEST_COST_WITH_BID_CAP",
               "bid_country_multiplier_overrides": {"BR": 1.5, "IN": 1.2}})
    );
    let get = h.graph.last_request().unwrap();
    assert_eq!(get.path(), format!("/v25.0/{TEMPLATE}"));
    assert_eq!(get.query("fields").as_deref(), Some("optimization_spec"));
    script_template(&h, TEMPLATE, true);
    h.graph.push_json(200, json!({"success": true}));
    let set = h
        .call(call(Method::PUT, path.clone(), &key).json(&json!({
                "bid_amount": 87000, "bid_country_multiplier_overrides": {"IN": 1.2}})))
        .await;
    assert_eq!(set.status, StatusCode::NO_CONTENT, "{}", set.text);
    let post = h.graph.last_request().unwrap();
    assert_eq!(post.method, Method::POST);
    assert_eq!(post.path(), format!("/v25.0/{TEMPLATE}"));
    assert_eq!(post.bearer(), Some(TOKEN));
    assert_eq!(
        post.json(),
        Some(json!({"optimization_spec": {"bid_amount": 87000,
            "bid_strategy": "LOWEST_COST_WITH_BID_CAP",
            "bid_country_multiplier_overrides": {"IN": 1.2}}}))
    );
    script_template(&h, TEMPLATE, true);
    h.graph.push_json(200, json!({"success": true}));
    let cleared = h.call(call(Method::DELETE, path.clone(), &key)).await;
    assert_eq!(cleared.status, StatusCode::NO_CONTENT, "{}", cleared.text);
    assert_eq!(
        h.graph.last_request().unwrap().json(),
        Some(json!({"optimization_spec": {"bid_strategy": null, "bid_amount": null}}))
    );
    // Not the WABA's: each of the three is `404` after the lookup alone.
    let before = h.graph.requests().len();
    for (method, body) in [
        (Method::GET, None),
        (Method::PUT, Some(json!({"bid_amount": 5}))),
        (Method::DELETE, None),
    ] {
        script_template(&h, TEMPLATE, false);
        let mut c = call(method.clone(), path.clone(), &key);
        if let Some(body) = &body {
            c = c.json(body);
        }
        let reply = h.call(c).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{method}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before + 6, "lookups only");
    // Refused first.
    let before = h.graph.requests().len();
    for (body, field) in [
        (json!({"bid_amount": 0}), "bid_amount"),
        (
            json!({"bid_amount": 5, "bid_country_multiplier_overrides": {"IN": 11.0}}),
            "bid_country_multiplier_overrides",
        ),
        (
            json!({"bid_amount": 5, "bid_country_multiplier_overrides": {"in": 1.0}}),
            "bid_country_multiplier_overrides",
        ),
    ] {
        let reply = h
            .call(call(Method::PUT, path.clone(), &key).json(&body))
            .await;
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
    }
    assert_eq!(h.graph.requests().len(), before);
    assert_eq!(h.graph.remaining(), 0);
}

/// marketing-messages/pricing, reach estimates: `targeting_spec` as the
/// JSON Meta requires and `date_interval`; refused first when wrong.
#[tokio::test]
async fn a_reach_estimate_asks_with_the_countries_and_the_interval() {
    let (h, key) = connected(false).await;
    h.graph.push_json(
        200,
        json!({"waba_currency": "USD", "estimates": [
            {"bid_amount": 400, "users": 1000, "deliveries_lower_bound": 500,
             "deliveries_upper_bound": 570, "cost_lower_bound": 389.74, "cost_upper_bound": 390.74}]}),
    );
    let reply = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/marketing/reach-estimates?countries=IN,US&date_interval=L7D"),
            &key,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json()["estimates"][0]["cost_upper_bound"], 390.74);
    assert_eq!(reply.json()["waba_currency"], "USD");
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.path(), format!("/v25.0/{WABA}/reachestimate"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        request.query("targeting_spec").as_deref(),
        Some(r#"{"geo_locations":{"countries":["IN","US"]}}"#)
    );
    assert_eq!(request.query("date_interval").as_deref(), Some("L7D"));
    for (query, field) in [
        ("countries=&date_interval=L7D", "countries"),
        ("countries=in&date_interval=L7D", "countries"),
        ("countries=IN&date_interval=L30D", "date_interval"),
        ("countries=IN", "query"),
    ] {
        let reply = h
            .call(call(
                Method::GET,
                format!("/v1/wabas/{WABA}/marketing/reach-estimates?{query}"),
                &key,
            ))
            .await;
        assert_eq!(reply.json()["error"]["field"], field, "{query}");
    }
    assert_eq!(h.graph.requests().len(), 1);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Welcome message sequences ───────────────────────────────────────────

/// ctwa/welcome-message-sequences: a creation as a form (`name`, then the
/// message as JSON text) with the guide's example, `201` with the id (a
/// repeat under the same key is one creation); the list; one read; a change
/// and a delete after the sequence is found in the WABA's own (`sequence_id`
/// filter). Decisive: the lookup before each change, and the creation's key.
#[tokio::test]
async fn welcome_message_sequences_are_created_read_changed_and_deleted() {
    let (h, key) = connected(false).await;
    let create = || {
        call(
            Method::POST,
            format!("/v1/wabas/{WABA}/welcome-sequences"),
            &key,
        )
        .header("idempotency-key", "seq-1")
        .json(&json!({"name": "Driver sign-up",
                "welcome_message_sequence": {
                    "text": "This is a welcome message authored in a 3P tool",
                    "autofill_message": {"content": "Hello! Can I get more info on this!"},
                    "ice_breakers": [{"title": "Quick reply 1"}, {"title": "Quick reply 2"}]}}))
    };
    h.graph.push_json(200, json!({"sequence_id": SEQUENCE}));
    let created = h.call(create()).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert_eq!(created.json(), json!({"sequence_id": SEQUENCE}));
    let post = h.graph.last_request().unwrap();
    assert_eq!(post.method, Method::POST);
    assert_eq!(
        post.path(),
        format!("/v25.0/{WABA}/welcome_message_sequences")
    );
    assert_eq!(post.bearer(), Some(TOKEN));
    let RecordedBody::Multipart(_) = &post.body else {
        panic!("a form: {:?}", post.body)
    };
    let (_, _, name) = post.multipart_field("name").unwrap();
    assert_eq!(name.as_ref(), b"Driver sign-up");
    let (_, _, message) = post.multipart_field("welcome_message_sequence").unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(message).unwrap(),
        json!({"text": "This is a welcome message authored in a 3P tool",
               "autofill_message": {"content": "Hello! Can I get more info on this!"},
               "ice_breakers": [{"title": "Quick reply 1"}, {"title": "Quick reply 2"}]})
    );
    let again = h.call(create()).await;
    assert_eq!(again.json(), created.json());
    assert_eq!(h.graph.requests().len(), 1);

    h.graph.push_json(200, sequence_json(SEQUENCE));
    let listed = h
        .call(call(
            Method::GET,
            format!("/v1/wabas/{WABA}/welcome-sequences?limit=5"),
            &key,
        ))
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(listed.json()["data"][0]["sequence_id"], SEQUENCE);
    assert_eq!(listed.json()["data"][0]["is_used_in_ad"], false);
    assert_eq!(
        listed.json()["data"][0]["welcome_message_sequence"]["ice_breakers"],
        json!([{"title": "Quick reply 1"}])
    );
    let get = h.graph.last_request().unwrap();
    assert_eq!(get.method, Method::GET);
    assert_eq!(get.query("limit").as_deref(), Some("5"));

    let one = format!("/v1/wabas/{WABA}/welcome-sequences/{SEQUENCE}");
    h.graph.push_json(200, sequence_json(SEQUENCE));
    let read = h.call(call(Method::GET, one.clone(), &key)).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.text);
    assert_eq!(read.json()["name"], "Driver Sign up");
    assert_eq!(
        h.graph
            .last_request()
            .unwrap()
            .query("sequence_id")
            .as_deref(),
        Some(SEQUENCE)
    );

    h.graph.push_json(200, sequence_json(SEQUENCE));
    h.graph.push_json(200, json!({"success": true}));
    let changed = h
        .call(
            call(Method::PATCH, one.clone(), &key)
                .json(&json!({"name": "Driver sign-up updated name"})),
        )
        .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT, "{}", changed.text);
    let post = h.graph.last_request().unwrap();
    assert_eq!(post.method, Method::POST);
    let (_, _, id) = post.multipart_field("sequence_id").unwrap();
    assert_eq!(id.as_ref(), SEQUENCE.as_bytes());
    assert!(post.multipart_field("welcome_message_sequence").is_none());

    h.graph.push_json(200, sequence_json(SEQUENCE));
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h.call(call(Method::DELETE, one.clone(), &key)).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text);
    let delete = h.graph.last_request().unwrap();
    assert_eq!(delete.method, Method::DELETE);
    assert_eq!(
        delete.path(),
        format!("/v25.0/{WABA}/welcome_message_sequences")
    );
    assert_eq!(delete.query("sequence_id").as_deref(), Some(SEQUENCE));
    assert_eq!(h.graph.remaining(), 0);
}

/// A sequence the WABA does not hold is `404` for the read, the change and
/// the delete, after the lookup alone: Meta answering nothing, or another
/// sequence than the one asked for, is not the WABA's. Refused first:
/// ids that are not digits, an empty change, a message without a part.
#[tokio::test]
async fn a_sequence_the_waba_does_not_hold_is_not_found_and_nothing_is_changed() {
    let (h, key) = connected(false).await;
    let one = format!("/v1/wabas/{WABA}/welcome-sequences/{SEQUENCE}");
    for answer in [json!([]), sequence_json("999999")] {
        for (method, body) in [
            (Method::GET, None),
            (Method::PATCH, Some(json!({"name": "x"}))),
            (Method::DELETE, None),
        ] {
            let before = h.graph.requests().len();
            h.graph.push_json(200, answer.clone());
            let mut c = call(method.clone(), one.clone(), &key);
            if let Some(body) = &body {
                c = c.json(body);
            }
            let reply = h.call(c).await;
            assert_eq!(
                reply.status,
                StatusCode::NOT_FOUND,
                "{method}: {}",
                reply.text
            );
            assert_eq!(
                h.graph.requests().len(),
                before + 1,
                "{method}: the lookup only"
            );
        }
    }
    let before = h.graph.requests().len();
    for (method, path, body, field) in [
        (
            Method::GET,
            format!("/v1/wabas/{WABA}/welcome-sequences/12a"),
            None,
            "sequence_id",
        ),
        (Method::PATCH, one.clone(), Some(json!({})), "update"),
        (
            Method::PATCH,
            one.clone(),
            Some(json!({"welcome_message_sequence": {}})),
            "welcome_message_sequence",
        ),
        (
            Method::PATCH,
            one.clone(),
            Some(json!({"name": ""})),
            "name",
        ),
        (
            Method::POST,
            format!("/v1/wabas/{WABA}/welcome-sequences"),
            Some(json!({"name": "n", "welcome_message_sequence": {"text": "t", "tex": "x"}})),
            "welcome_message_sequence.tex",
        ),
        (
            Method::POST,
            format!("/v1/wabas/{WABA}/welcome-sequences"),
            Some(json!({"name": "n"})),
            "body",
        ),
        (
            Method::GET,
            format!("/v1/wabas/{WABA}/welcome-sequences?limit=101"),
            None,
            "limit",
        ),
    ] {
        let mut c = call(method, path.clone(), &key);
        if let Some(body) = &body {
            c = c.json(body);
        }
        let reply = h.call(c).await;
        assert_eq!(reply.json()["error"]["field"], field, "{path}");
    }
    assert_eq!(h.graph.requests().len(), before);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Template creation with a GIF header (row 154) ───────────────────────

/// templates/components, header format `GIF`: a template created through
/// the service's creation route passes the definition to Meta with the
/// format as written (an `example` handle, as every media header).
#[tokio::test]
async fn a_gif_header_passes_through_template_creation() {
    let (h, _) = connected(false).await;
    let key = h.tenant_key(TENANT, &[Scope::Templates]).await;
    h.graph.push_json(
        200,
        json!({"id": "546151681022936", "status": "PENDING", "category": "MARKETING"}),
    );
    let definition = json!({
        "name": "animated_promo", "language": "en_US", "category": "MARKETING",
        "components": [
            {"type": "HEADER", "format": "GIF", "example": {"header_handle": ["4::aW1hZ2UvZ2lm:ARZ"]}},
            {"type": "BODY", "text": "Our sale is on!"}
        ]
    });
    let reply = h
        .call(call(Method::POST, format!("/v1/wabas/{WABA}/templates"), &key).json(&definition))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.path(), format!("/v25.0/{WABA}/message_templates"));
    let sent = request.json().unwrap();
    assert_eq!(sent["components"][0]["format"], "GIF");
    assert_eq!(
        sent["components"][0]["example"],
        json!({"header_handle": ["4::aW1hZ2UvZ2lm:ARZ"]})
    );
}

// ─── The max price agreement: a legal act ────────────────────────────────

fn sign(business: &str, admin: &str) -> Call {
    call(
        Method::POST,
        format!("/v1/admin/businesses/{business}/max-price-agreement"),
        admin,
    )
    .json(
        &json!({"signer_name": "Jane Doe", "signer_email": "jane.doe@example.com",
                  "token": BUSINESS_TOKEN}),
    )
}

/// With the legal acts off (the default) the agreement is `403
/// feature_not_available` and **nothing is asked of Meta**: whatever the
/// body (even a malformed one) says, and for a key that is not an admin's
/// still `403 forbidden` first. Decisive: `MaxPriceAgreements::sign`'s gate.
#[tokio::test]
async fn with_the_legal_acts_off_the_agreement_is_refused_without_a_request_to_meta() {
    let (h, _) = connected(false).await;
    let admin = h.admin_key().await;
    for call in [
        sign(BUSINESS, &admin),
        sign("not-an-id", &admin),
        call(
            Method::POST,
            format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement"),
            &admin,
        )
        .json(&json!({"signer_name": "", "signer_email": "", "token": ""})),
        // Malformed bodies: the gate answers before the body is read, so
        // none of them is a `422 body`.
        call(
            Method::POST,
            format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement"),
            &admin,
        )
        .json(&json!({"signer_name": "Jane Doe", "token": BUSINESS_TOKEN})),
        call(
            Method::POST,
            format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement"),
            &admin,
        )
        .json(&json!({"signer_name": "Jane Doe", "signer_email": "j@e.x",
                      "token": BUSINESS_TOKEN, "extra": 1})),
        call(
            Method::POST,
            format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement"),
            &admin,
        )
        .header("content-type", "application/json")
        .body(meta_whatsapp_rs::webhooks::axum::body::Body::from(
            "{not json",
        )),
        call(
            Method::POST,
            format!("/v1/admin/businesses/{BUSINESS}/max-price-agreement"),
            &admin,
        ),
    ] {
        let reply = h.call(call).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
        assert_eq!(reply.code(), "feature_not_available");
    }
    assert!(h.graph.requests().is_empty(), "{:?}", h.graph.requests());
    assert_eq!(h.graph.remaining(), 0);
    // A tenant's key, whatever its scopes, is not an admin's.
    let tenant = h.tenant_key(TENANT, &common::ALL_SCOPES).await;
    let reply = h.call(sign(BUSINESS, &tenant)).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.code(), "forbidden");
    assert!(h.graph.requests().is_empty());
}

/// With the legal acts on, the admin key's one call is exactly one request
/// to Meta: the page's body, the operator's token (not the tenant's, not
/// stored), the answer without the signer; Meta's refusal quoting the
/// signer answers its code and the service's kind, never its text; a
/// refusal or a timeout is never replayed. The inputs are checked first.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario: signed, refused, limited, cut, then the checks
async fn with_the_legal_acts_on_the_agreement_is_one_request_and_never_replayed() {
    let (h, _) = connected(true).await;
    let admin = h.admin_key().await;
    let writes = h.kv.writes().len();
    h.graph.push_json(
        200,
        json!({"status": "pending", "signer_name": "Jane Doe", "signer_email": "jane.doe@example.com",
               "message": "Agreement email sent to jane.doe@example.com"}),
    );
    let reply = h.call(sign(BUSINESS, &admin)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json(), json!({"status": "pending"}));
    assert!(!reply.text.contains("Jane") && !reply.text.contains("example.com"));
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::POST);
    assert_eq!(
        requests[0].path(),
        format!("/v25.0/{BUSINESS}/max_price_agreements")
    );
    assert_eq!(requests[0].bearer(), Some(BUSINESS_TOKEN));
    assert_eq!(
        requests[0].json(),
        Some(json!({"signer_name": "Jane Doe", "signer_email": "jane.doe@example.com"}))
    );
    assert_eq!(h.kv.writes().len(), writes, "the token is stored nowhere");
    // Meta refuses, quoting the signer: the code, never the text.
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#100) bad email jane.doe@example.com", "type": "OAuthException",
                         "code": 100, "error_data": {"details": "Jane Doe jane.doe@example.com"},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h.call(sign(BUSINESS, &admin)).await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.text
    );
    assert!(
        !refused.text.contains("Jane") && !refused.text.contains("example.com"),
        "{}",
        refused.text
    );
    assert_eq!(refused.json()["error"]["graph"]["details"], Value::Null);
    // A 429 within five minutes of the last email, and a timeout, are one
    // request each.
    h.graph.push_json(
        429,
        json!({"error": {"message": "wait Jane Doe jane.doe@example.com", "type": "OAuthException",
                         "code": 4, "fbtrace_id": "x"}}),
    );
    let limited = h.call(sign(BUSINESS, &admin)).await;
    assert_eq!(
        limited.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        limited.text
    );
    assert!(!limited.text.contains("Jane"));
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h.call(sign(BUSINESS, &admin)).await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    // A 5xx is one request too: the email may have been sent.
    h.graph.push_json(
        500,
        json!({"error": {"message": "boom Jane Doe", "type": "OAuthException",
                         "code": 2, "fbtrace_id": "x"}}),
    );
    let failed = h.call(sign(BUSINESS, &admin)).await;
    assert!(!failed.status.is_success(), "{}", failed.text);
    assert!(!failed.text.contains("Jane"), "{}", failed.text);
    assert_eq!(failed.json()["error"]["may_have_been_sent"], true);
    assert_eq!(
        h.graph.requests().len(),
        5,
        "one request per call, none replayed"
    );
    // Checked first.
    for (path, body, field) in [
        (
            "not-an-id".to_owned(),
            json!({"signer_name": "J", "signer_email": "j@e.x", "token": "t"}),
            "business_id",
        ),
        (
            BUSINESS.to_owned(),
            json!({"signer_name": " ", "signer_email": "j@e.x", "token": "t"}),
            "signer_name",
        ),
        (
            BUSINESS.to_owned(),
            json!({"signer_name": "J", "signer_email": "no-at", "token": "t"}),
            "signer_email",
        ),
        (
            BUSINESS.to_owned(),
            json!({"signer_name": "J", "signer_email": "j@e.x", "token": " "}),
            "token",
        ),
        (
            BUSINESS.to_owned(),
            json!({"signer_name": "J", "signer_email": "j@e.x"}),
            "body",
        ),
    ] {
        let reply = h
            .call(
                call(
                    Method::POST,
                    format!("/v1/admin/businesses/{path}/max-price-agreement"),
                    &admin,
                )
                .json(&body),
            )
            .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
    }
    assert_eq!(h.graph.requests().len(), 5);
    assert_eq!(h.graph.remaining(), 0);
}

/// **No other route sends the agreement**, with the legal acts on: every
/// route of the document that names a number or a WABA is called with a key
/// of every scope, Meta answering each request it gets, and no request to
/// `/max_price_agreements` is ever made; the routes that spend through max
/// price (a duplicate, a price set, a marketing send) answered by Meta's
/// refusal for want of the agreement return it as it is. Decisive: any
/// other operation calling `sign_max_price_agreement`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the walk, then the refusals
async fn no_other_route_ever_sends_the_agreement() {
    let (h, _) = connected(true).await;
    let key = h.tenant_key(TENANT, &common::ALL_SCOPES).await;
    let sample = common::Sample {
        tenant: TENANT.to_owned(),
        waba: WABA.to_owned(),
        pn: PN.to_owned(),
        key_id: "placeholder".to_owned(),
    };
    let mut walked = 0;
    for operation in common::spec_operations() {
        let numbered =
            operation.template.contains("{pn}") || operation.template.contains("{waba_id}");
        if operation.admin() || !numbered || operation.template.contains("stream") {
            continue;
        }
        for _ in 0..8 {
            h.graph.push_json(200, json!({}));
        }
        let _ = h
            .call(common::sample_call(&operation, &sample, Some(&key)))
            .await;
        walked += 1;
    }
    assert!(walked >= 100, "{walked} routes walked");
    let asked = sent_paths(&h);
    assert!(asked.len() > 50, "Meta was asked {} times", asked.len());
    assert!(
        asked.iter().all(|p| !p.contains("max_price_agreements")),
        "{:?}",
        asked
            .iter()
            .filter(|p| p.contains("max_price"))
            .collect::<Vec<_>>()
    );
    // Meta refusing for want of the agreement is returned, not answered by
    // signing (on a harness whose script the walk left empty).
    let (h, _) = connected(true).await;
    let key = h.tenant_key(TENANT, &common::ALL_SCOPES).await;
    let before = h.graph.requests().len();
    h.graph.push_json(
        403,
        json!({"error": {"message": "You need to sign the testing legal agreement", "type": "OAuthException",
                         "code": 100, "fbtrace_id": "x"}}),
    );
    script_template(&h, TEMPLATE, true);
    let refused = h
        .call(
            call(
                Method::PUT,
                format!("/v1/wabas/{WABA}/marketing/templates/{TEMPLATE}/max-price"),
                &key,
            )
            .json(&json!({"bid_amount": 5000})),
        )
        .await;
    assert!(!refused.status.is_success(), "{}", refused.text);
    let after: Vec<String> = sent_paths(&h)[before..].to_vec();
    assert!(
        after.iter().all(|p| !p.contains("max_price_agreements")),
        "{after:?}"
    );
}

/// The agreement's call is named by one module of the service and nowhere
/// else: no source of the service's crates but the core's `marketing`
/// names the library's signer or the agreement's path, and the admin route
/// is the only handler that reaches it. Decisive: a second caller.
#[test]
fn only_the_marketing_core_names_the_agreement() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let mut found = Vec::new();
    let mut stack = vec![crates.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                let service = path
                    .parent()
                    .is_some_and(|p| p == crates)
                    .then(|| name.starts_with("meta-whatsapp-server"));
                // The service's crates, their `src` only (the library's own
                // module is checked by its tests).
                if service == Some(false) || name == "target" || name == "tests" {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let source = std::fs::read_to_string(&path).unwrap();
                let code: String = source
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n");
                if code.contains(".sign_max_price_agreement(")
                    || code.contains("max_price_agreements")
                {
                    found.push(path.strip_prefix(crates).unwrap().display().to_string());
                }
            }
        }
    }
    found.sort();
    assert_eq!(
        found,
        ["meta-whatsapp-server-core/src/marketing.rs".to_owned()]
    );
}
