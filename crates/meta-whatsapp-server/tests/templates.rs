//! Templates (scope `templates`; docs/design/server.md, section 4.2):
//! list (cached 60 s per WABA), get, create (checked locally, Meta's
//! refusals classified), delete, with the WABA's token.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::time::Duration;

use common::{Call, Harness, sample_template_definition};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const WABA_2: &str = "102290129340399";
const PN_2: &str = "106540352242923";
const TOKEN: &str = "EAAG-merchant-token";
const FIELDS: &str = "name,language,status,category,components";

async fn connected_with(settings: Settings) -> (Harness, String) {
    let h = Harness::with_settings(settings);
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.connect(TENANT, WABA_2, &[PN_2], "EAAG-second-waba-token")
        .await;
    let key = h.tenant_key(TENANT, &[Scope::Templates]).await;
    (h, key)
}

async fn connected() -> (Harness, String) {
    connected_with(common::test_settings()).await
}

/// templates/template-management, "Get all templates" (its first
/// template), with a next page.
fn documented_list() -> Value {
    json!({
        "data": [{
            "name": "coupon_expiration_reminder_number_vars",
            "parameter_format": "POSITIONAL",
            "components": [
                {"type": "HEADER", "format": "TEXT", "text": "Act fast, {{1}}!", "example": {"header_text": ["Pablo"]}},
                {"type": "BODY", "text": "Just a quick reminder—your exclusive coupon code, {{1}}, *expires in only {{2}} days!*",
                 "example": {"body_text": [["SUMMER20", "10"]]}},
                {"type": "FOOTER", "text": "Lucky Shrub Succulents"},
                {"type": "BUTTONS", "buttons": [
                    {"type": "URL", "text": "See deals", "url": "https://www.luckyshrub.com/deals"},
                    {"type": "QUICK_REPLY", "text": "Unsubscribe"}
                ]}
            ],
            "language": "en",
            "status": "APPROVED",
            "category": "MARKETING",
            "sub_category": "CUSTOM",
            "id": "1304694804498707"
        }],
        "paging": {"cursors": {"before": "QVFIU...", "after": "QVFIUafter"},
                   "next": "https://graph.facebook.com/v23.0/10229..."}
    })
}

fn list(key: &str, waba: &str, query: &str) -> Call {
    Call::get(format!("/v1/wabas/{waba}/templates{query}")).key(key)
}

/// The documented list, as the service answers it, and the exact request.
#[tokio::test]
async fn a_list_is_metas_with_the_designs_fields() {
    let (h, key) = connected().await;
    h.graph.push_json(200, documented_list());
    let reply = h.call(list(&key, WABA, "")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let body = reply.json();
    assert_eq!(
        body["data"][0],
        json!({
            "id": "1304694804498707",
            "name": "coupon_expiration_reminder_number_vars",
            "language": "en",
            "status": "APPROVED",
            "category": "MARKETING",
            "components": documented_list()["data"][0]["components"]
        })
    );
    let cursor = body["next_cursor"].as_str().unwrap().to_owned();
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(request.query("fields").as_deref(), Some(FIELDS));
    assert_eq!(request.query("limit").as_deref(), Some("50"));
    assert_eq!(request.query("after"), None);
    assert_eq!(request.bearer(), Some(TOKEN));
    // Filters and the next page reach Meta as its parameters.
    h.graph.push_json(200, json!({"data": []}));
    let next = h
        .call(list(
            &key,
            WABA,
            &format!("?status=approved&name=order_confirmation&limit=5&cursor={cursor}"),
        ))
        .await;
    assert_eq!(next.status, StatusCode::OK, "{}", next.text);
    assert_eq!(next.json(), json!({"data": [], "next_cursor": null}));
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.query("status").as_deref(), Some("APPROVED"));
    assert_eq!(request.query("name").as_deref(), Some("order_confirmation"));
    assert_eq!(request.query("limit").as_deref(), Some("5"));
    assert_eq!(request.query("after").as_deref(), Some("QVFIUafter"));
    for bad in [
        "?status=a%26b",
        "?name=Order%20Confirmation",
        "?limit=101",
        "?cursor=zz",
    ] {
        let reply = h.call(list(&key, WABA, bad)).await;
        assert_eq!(reply.code(), "invalid_request", "{bad}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// Lists are cached 60 s per WABA: the same query on the same WABA asks
/// Meta once; another WABA of the tenant asks with its own token; a
/// creation drops the WABA's pages. Decisive: the WABA in the cache key.
#[tokio::test]
async fn lists_are_cached_per_waba() {
    let (h, key) = connected_with(Settings {
        template_cache_ttl: Duration::from_millis(300),
        ..common::test_settings()
    })
    .await;
    h.graph.push_json(200, documented_list());
    let first = h.call(list(&key, WABA, "")).await;
    let again = h.call(list(&key, WABA, "")).await;
    assert_eq!(again.text, first.text);
    assert_eq!(h.graph.requests().len(), 1, "cached");
    // The other WABA: its own list, its own token.
    h.graph.push_json(200, json!({"data": []}));
    let other = h.call(list(&key, WABA_2, "")).await;
    assert_eq!(other.json()["data"], json!([]), "not the first WABA's page");
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(
        h.graph.last_request().unwrap().bearer(),
        Some("EAAG-second-waba-token")
    );
    // Another query is another page.
    h.graph.push_json(200, json!({"data": []}));
    h.call(list(&key, WABA, "?status=APPROVED")).await;
    assert_eq!(h.graph.requests().len(), 3);
    // A creation on the WABA drops its pages, not the other WABA's.
    h.graph.push_json(
        200,
        json!({"id": "1627019861106475", "status": "PENDING", "category": "MARKETING"}),
    );
    let created = h
        .call(
            Call::new(Method::POST, format!("/v1/wabas/{WABA}/templates"))
                .key(&key)
                .json(&sample_template_definition()),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    h.graph.push_json(200, documented_list());
    h.call(list(&key, WABA, "")).await;
    h.call(list(&key, WABA_2, "")).await;
    assert_eq!(
        h.graph.requests().len(),
        5,
        "the first WABA asked again only"
    );
    // Past the TTL, Meta is asked again.
    tokio::time::sleep(Duration::from_millis(400)).await;
    h.graph.push_json(200, json!({"data": []}));
    h.call(list(&key, WABA_2, "")).await;
    assert_eq!(h.graph.requests().len(), 6);
    assert_eq!(h.graph.remaining(), 0);
}

/// One template: its name from `GET /{id}`, then the template itself
/// from the WABA's own list of that name (Meta's template object does not
/// name its WABA). Decisive: the id looked for in the WABA's list.
#[tokio::test]
async fn one_template_is_found_through_its_wabas_list() {
    let (h, key) = connected().await;
    // templates/overview, "Example response", with the name asked for.
    h.graph.push_json(
        200,
        json!({"name": "coupon_expiration_reminder_number_vars", "id": "1304694804498707"}),
    );
    h.graph.push_json(200, documented_list());
    let reply = h
        .call(Call::get(format!("/v1/wabas/{WABA}/templates/1304694804498707")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({
            "id": "1304694804498707",
            "name": "coupon_expiration_reminder_number_vars",
            "language": "en",
            "status": "APPROVED",
            "category": "MARKETING",
            "components": documented_list()["data"][0]["components"]
        })
    );
    let requests = h.graph.requests();
    let [named, listed] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_eq!(named.path(), "/v25.0/1304694804498707");
    assert_eq!(named.query("fields").as_deref(), Some("name"));
    assert_eq!(named.bearer(), Some(TOKEN));
    assert_eq!(listed.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(
        listed.query("name").as_deref(),
        Some("coupon_expiration_reminder_number_vars")
    );
    assert_eq!(listed.query("fields").as_deref(), Some(FIELDS));
    assert_eq!(listed.bearer(), Some(TOKEN));
    // The list goes on past a page without it; a name Meta does not give
    // back is not found.
    h.graph.push_json(
        200,
        json!({"name": "spring_sale", "id": "1627019861106475"}),
    );
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1627019861106476", "name": "spring_sale"}],
               "paging": {"cursors": {"after": "QVFIUafter"}, "next": "https://graph.facebook.com/v25.0/x"}}),
    );
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1627019861106475", "name": "spring_sale", "language": "de"}]}),
    );
    let reply = h
        .call(Call::get(format!("/v1/wabas/{WABA}/templates/1627019861106475")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json()["language"], "de");
    assert_eq!(
        h.graph.last_request().unwrap().query("after").as_deref(),
        Some("QVFIUafter")
    );
    let bad = h
        .call(Call::get(format!("/v1/wabas/{WABA}/templates/abc")).key(&key))
        .await;
    assert_eq!(bad.json()["error"]["field"], "id");
    assert_eq!(h.graph.remaining(), 0);
}

/// A creation posts Meta's JSON as given and answers `201`; a definition
/// breaking a documented limit is refused locally; Meta's refusals are
/// classified. Decisive: the local check before the request.
#[tokio::test]
async fn a_creation_is_checked_locally_then_posted() {
    let (h, key) = connected().await;
    let create = |body: &Value| {
        Call::new(Method::POST, format!("/v1/wabas/{WABA}/templates"))
            .key(&key)
            .json(body)
    };
    // custom-marketing-templates, example response.
    h.graph.push_json(
        200,
        json!({"id": "1627019861106475", "status": "PENDING", "category": "MARKETING"}),
    );
    let reply = h.call(create(&sample_template_definition())).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"id": "1627019861106475", "status": "PENDING", "category": "MARKETING"})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(request.json().unwrap(), sample_template_definition());
    // Refused locally: a name Meta refuses (`^[a-z0-9_]+$`), no body text.
    let before = h.graph.requests().len();
    let mut bad_name = sample_template_definition();
    bad_name["name"] = json!("Spring Sale");
    let reply = h.call(create(&bad_name)).await;
    assert_eq!(
        (
            reply.code().as_str(),
            reply.json()["error"]["field"].as_str()
        ),
        ("invalid_request", Some("name"))
    );
    let reply = h.call(create(&json!({"name": "x"}))).await;
    assert_eq!(reply.code(), "invalid_request");
    // A key the library would drop (misspelled, or not modelled) is
    // refused, never left out of what Meta receives.
    let mut misspelled = sample_template_definition();
    misspelled["sub_catgory"] = json!("CUSTOM");
    let reply = h.call(create(&misspelled)).await;
    assert_eq!(
        (reply.status, reply.json()["error"]["field"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("sub_catgory"))
    );
    // templates/authentication-templates/autofill-button-authentication-templates,
    // "Example request": the app's package and hash on the button itself.
    let autofill = json!({"name": "authentication_code_autofill_button", "language": "en_US",
        "category": "AUTHENTICATION", "components": [
            {"type": "BODY", "add_security_recommendation": true},
            {"type": "FOOTER", "code_expiration_minutes": 10},
            {"type": "BUTTONS", "buttons": [{"type": "OTP", "otp_type": "ONE_TAP", "text": "Copy Code",
                "autofill_text": "Autofill", "package_name": "com.example.luckyshrub",
                "signature_hash": "K8a/AINcGX7"}]}]});
    let reply = h.call(create(&autofill)).await;
    assert_eq!(
        reply.json()["error"]["field"],
        "components[2].buttons[0].package_name",
        "{}",
        reply.text
    );
    assert_eq!(h.graph.requests().len(), before, "nothing reached Meta");
    // Meta's refusals.
    for (code, expected) in [
        (2388019, (StatusCode::CONFLICT, "template_limit_reached")),
        (
            2388040,
            (StatusCode::UNPROCESSABLE_ENTITY, "template_rejected"),
        ),
    ] {
        h.graph.push_json(
            400,
            json!({"error": {"message": "x", "type": "OAuthException", "code": code,
                             "error_data": {"details": "Meta's reason"}}}),
        );
        let reply = h.call(create(&sample_template_definition())).await;
        assert_eq!((reply.status, reply.code().as_str()), expected, "{code}");
        assert_eq!(reply.json()["error"]["graph"]["details"], "Meta's reason");
        assert_eq!(reply.json()["error"]["may_have_been_sent"], false);
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// A creation with an `Idempotency-Key` never creates twice.
#[tokio::test]
async fn a_creation_key_replays() {
    let (h, key) = connected().await;
    let create = || {
        Call::new(Method::POST, format!("/v1/wabas/{WABA}/templates"))
            .key(&key)
            .header("idempotency-key", "spring-sale-en")
            .json(&sample_template_definition())
    };
    h.graph.push_json(
        200,
        json!({"id": "1627019861106475", "status": "PENDING", "category": "MARKETING"}),
    );
    let first = h.call(create()).await;
    let again = h.call(create()).await;
    assert_eq!(again.status, StatusCode::CREATED);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, first.text);
    assert_eq!(h.graph.requests().len(), 1);
}

/// Deletion by name, or by name and id, as Meta documents it.
#[tokio::test]
async fn deletions_are_metas_requests() {
    let (h, key) = connected().await;
    let delete = |query: &str| {
        Call::new(Method::DELETE, format!("/v1/wabas/{WABA}/templates{query}")).key(&key)
    };
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(delete("?name=order_confirmation")).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        (request.method.clone(), request.path()),
        (Method::DELETE, "/v25.0/102290129340398/message_templates")
    );
    assert_eq!(request.query("name").as_deref(), Some("order_confirmation"));
    assert_eq!(request.query("hsm_id"), None);
    assert_eq!(request.bearer(), Some(TOKEN));
    // By name and id: the id is looked for among the WABA's templates of
    // that name first.
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1407680676729941", "name": "order_confirmation"}]}),
    );
    h.graph.push_json(200, json!({"success": true}));
    let reply = h
        .call(delete("?name=order_confirmation&id=1407680676729941"))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let requests = h.graph.requests();
    let [.., looked_up, request] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(
        (looked_up.method.clone(), looked_up.path().to_owned()),
        (Method::GET, format!("/v25.0/{WABA}/message_templates"))
    );
    assert_eq!(
        looked_up.query("name").as_deref(),
        Some("order_confirmation")
    );
    assert_eq!(request.method, Method::DELETE);
    assert_eq!(request.query("hsm_id").as_deref(), Some("1407680676729941"));
    assert_eq!(request.query("name").as_deref(), Some("order_confirmation"));
    let before = h.graph.requests().len();
    for bad in ["", "?name=", "?name=Bad%20Name", "?name=a&id=x1"] {
        assert_eq!(h.call(delete(bad)).await.code(), "invalid_request", "{bad}");
    }
    assert_eq!(h.graph.requests().len(), before);
    assert_eq!(h.graph.remaining(), 0);
}

/// Template management's own bucket: 2 a second by default, per tenant.
#[tokio::test]
async fn template_management_is_rate_limited_by_default() {
    let (h, key) = connected_with(Settings {
        rate_limits: meta_whatsapp_server::ratelimit::RateLimits::default(),
        ..common::test_settings()
    })
    .await;
    h.graph.push_json(200, documented_list());
    for _ in 0..2 {
        assert_eq!(h.call(list(&key, WABA, "")).await.status, StatusCode::OK);
    }
    let reply = h.call(list(&key, WABA, "")).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_requests")
    );
    assert!(reply.headers.contains_key("retry-after"));
}

/// A page is cached for the tenant that asked: once the WABA is unbound
/// and bound to another tenant, that tenant's first list asks Meta, with
/// its own token. Decisive: the tenant in the cache key.
#[tokio::test]
async fn a_page_cached_for_one_tenant_is_never_answered_to_the_next() {
    use meta_whatsapp_rs::core::ids::WabaId;
    let (h, key) = connected().await;
    h.graph.push_json(200, documented_list());
    assert_eq!(h.call(list(&key, WABA, "")).await.status, StatusCode::OK);
    // The operator unbinds the WABA; another tenant attaches it.
    h.store.unbind_waba(&WabaId::new(WABA)).await.unwrap();
    h.tenant("merchant-43").await;
    h.connect("merchant-43", WABA, &[PN], "TOKEN-43").await;
    let theirs = h.tenant_key("merchant-43", &[Scope::Templates]).await;
    h.graph.push_json(200, json!({"data": []}));
    let listed = h.call(list(&theirs, WABA, "")).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(
        listed.json()["data"],
        json!([]),
        "not the first tenant's page"
    );
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(h.graph.last_request().unwrap().bearer(), Some("TOKEN-43"));
    assert_eq!(h.graph.remaining(), 0);
}
