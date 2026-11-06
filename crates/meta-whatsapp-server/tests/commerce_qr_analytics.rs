//! Roadmap M5g: commerce, QR codes, analytics and the block list, through
//! the service (scopes `commerce`, `qr_codes`, `analytics`, `contacts`;
//! docs/design/server.md, section 4.2).
//!
//! Each test sends the requests the pages document and checks the exact
//! requests that reach Meta (method, path, query, the tenant's token, the
//! JSON body) and `remaining() == 0`. The design's M5.2: a catalog is its
//! WABA's, a QR code its number's, a template of an analytics query its
//! WABA's, and a refusal sends no request that acts. M5.1 (another
//! tenant's number or WABA, zero vault reads) is `tests/authz.rs`, which
//! iterates every route of the document, these included.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::clock::Clock as _;
use meta_whatsapp_rs::core::testing::{RecordedBody, RecordedRequest};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const PN_2: &str = "106540352242923";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TENANT: &str = "merchant-43";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242924";
const CATALOG: &str = "194836987003835";
const CODE: &str = "4O4YGZEG3RIVE1";

/// A tenant with a WABA and two numbers, another tenant with one, and a
/// key of the tenant with every scope of the roadmap's item.
async fn connected() -> (Harness, String) {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN, PN_2], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], "EAAG-other")
        .await;
    let key = h
        .tenant_key(
            TENANT,
            &[
                Scope::Commerce,
                Scope::QrCodes,
                Scope::Analytics,
                Scope::Contacts,
            ],
        )
        .await;
    (h, key)
}

/// Every query parameter of `request`, in order.
fn query(request: &RecordedRequest) -> Vec<(String, String)> {
    request
        .url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// The harness clock's now, to the second.
fn now(h: &Harness) -> OffsetDateTime {
    h.clock.now().replace_nanosecond(0).unwrap()
}

/// `at`, RFC 3339, with its `+` (none here: UTC is `Z`) percent-encoded.
fn rfc(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap()
}

fn get(path: String, key: &str) -> Call {
    Call::get(path).key(key)
}

fn call(method: Method, path: String, key: &str) -> Call {
    Call::new(method, path).key(key)
}

fn error(reply: &common::Reply) -> Value {
    reply.json()["error"].clone()
}

/// Nothing reached Meta.
fn assert_nothing_sent(h: &Harness) {
    assert_eq!(h.graph.requests().len(), 0, "{:?}", h.graph.requests());
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Commerce settings ───────────────────────────────────────────────────

/// catalogs/set-commerce-settings, "Get commerce settings" (its sample
/// response) and "Enable or disable cart": the read, then the change as the
/// library sends it (parameters, no body), `204`. Refused before any
/// request: nothing to change, a key outside the object. Decisive: the
/// change's `checked`, and the exact request.
#[tokio::test]
async fn commerce_settings_are_read_and_changed_as_the_page_writes_them() {
    let (h, key) = connected().await;
    h.graph.push_json(
        200,
        json!({"data": [{"is_cart_enabled": true, "is_catalog_visible": true, "id": "727705352028726"}]}),
    );
    let read = h
        .call(get(format!("/v1/numbers/{PN}/commerce-settings"), &key))
        .await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.text);
    assert_eq!(
        read.json(),
        json!({"id": "727705352028726", "is_cart_enabled": true, "is_catalog_visible": true})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(
        request.path(),
        format!("/v25.0/{PN}/whatsapp_commerce_settings")
    );
    assert_eq!(request.url.query(), None);
    assert_eq!(request.bearer(), Some(TOKEN));

    h.graph.push_json(200, json!({"success": true}));
    let changed = h
        .call(
            call(
                Method::PATCH,
                format!("/v1/numbers/{PN}/commerce-settings"),
                &key,
            )
            .json(&json!({"is_cart_enabled": false, "is_catalog_visible": true})),
        )
        .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT, "{}", changed.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(
        request.path(),
        format!("/v25.0/{PN}/whatsapp_commerce_settings")
    );
    assert_eq!(
        query(&request),
        pairs(&[("is_cart_enabled", "false"), ("is_catalog_visible", "true")])
    );
    assert_eq!(request.body, RecordedBody::Empty);
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);

    let sent = h.graph.requests().len();
    for (body, field) in [
        (json!({}), "update"),
        (json!({"is_cart_enabled": "yes"}), "body"),
        (json!({"is_card_enabled": true}), "body"),
    ] {
        let refused = h
            .call(
                call(
                    Method::PATCH,
                    format!("/v1/numbers/{PN}/commerce-settings"),
                    &key,
                )
                .json(&body),
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(error(&refused)["field"], field, "{body}");
    }
    assert_eq!(h.graph.requests().len(), sent, "refused before any request");
}

// ─── Catalogs ────────────────────────────────────────────────────────────

fn catalog_answer() -> Value {
    json!({"data": [{"id": CATALOG, "name": "Lucky Shrub", "product_count": 2, "vertical": "commerce"}]})
}

/// The catalog a WABA connects (`GET /{waba_id}/product_catalogs`), a
/// catalog of its own by id (the same list, nothing more), and its
/// products paged. Another WABA's catalog id is `404` with the list as the
/// only request, and its products are never asked for (D141). Refused
/// before any request: a non-Meta id, a page of 0 or 101. Decisive:
/// `Catalogs::get` before `products`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // three reads, then each refusal
async fn a_catalog_is_the_wabas_or_it_does_not_exist() {
    let (h, key) = connected().await;
    h.graph.push_json(200, catalog_answer());
    let listed = h
        .call(get(format!("/v1/wabas/{WABA}/catalogs"), &key))
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(
        listed.json(),
        json!({"data": [{"id": CATALOG, "name": "Lucky Shrub", "product_count": 2, "vertical": "commerce"}]})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{WABA}/product_catalogs"));
    assert_eq!(
        query(&request),
        pairs(&[("fields", "id,name,product_count,vertical")])
    );
    assert_eq!(request.bearer(), Some(TOKEN));

    h.graph.push_json(200, catalog_answer());
    let one = h
        .call(get(format!("/v1/wabas/{WABA}/catalogs/{CATALOG}"), &key))
        .await;
    assert_eq!(one.status, StatusCode::OK, "{}", one.text);
    assert_eq!(one.json()["id"], CATALOG);
    assert_eq!(h.graph.requests().len(), 2, "the list was the only request");

    // Another WABA's catalog: not among this WABA's, `404`, the list only.
    h.graph.push_json(200, catalog_answer());
    let other = h
        .call(get(format!("/v1/wabas/{WABA}/catalogs/999999999"), &key))
        .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.text);
    assert_eq!(error(&other)["code"], "not_found");
    h.graph.push_json(200, catalog_answer());
    let products = h
        .call(get(
            format!("/v1/wabas/{WABA}/catalogs/999999999/products"),
            &key,
        ))
        .await;
    assert_eq!(products.status, StatusCode::NOT_FOUND, "{}", products.text);
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 4);
    for request in &requests[2..] {
        assert_eq!(request.path(), format!("/v25.0/{WABA}/product_catalogs"));
    }
    assert_eq!(h.graph.remaining(), 0);

    // The products: the list (ownership), then the products edge.
    h.graph.push_json(200, catalog_answer());
    h.graph.push_json(
        200,
        json!({"data": [
            {"id": "5000000000000001", "retailer_id": "sku-1", "name": "Mug", "price": "10.00 USD",
             "availability": "in stock", "review_status": "approved", "visibility": "published"},
            {"id": "5000000000000002", "retailer_id": "sku-2", "name": "Cap", "price": "12.00 USD"}],
            "paging": {"cursors": {"before": "QVFI", "after": "MjQZD"},
                       "next": "https://graph.facebook.com/v25.0/x?after=MjQZD&access_token=SECRET"}}),
    );
    let page = h
        .call(get(
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products?limit=2&return_only_approved_products=true"),
            &key,
        ))
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let body = page.json();
    assert_eq!(body["data"].as_array().unwrap().len(), 2);
    assert_eq!(body["data"][0]["retailer_id"], "sku-1");
    assert_eq!(body["data"][0]["availability"], "in stock");
    assert_eq!(body["data"][1]["availability"], Value::Null);
    // The cursor is opaque (the hex of Meta's), and the link with the
    // token in it is never answered.
    assert_eq!(body["next_cursor"], hex::encode("MjQZD"));
    assert!(!page.text.contains("SECRET"), "{}", page.text);
    let requests = h.graph.requests();
    let products = &requests[requests.len() - 1];
    assert_eq!(products.method, Method::GET);
    assert_eq!(products.path(), format!("/v25.0/{CATALOG}/products"));
    assert_eq!(
        query(products),
        pairs(&[
            (
                "fields",
                "id,retailer_id,name,description,availability,condition,price,sale_price,currency,image_url,url,brand,review_status,visibility"
            ),
            ("limit", "2"),
            ("return_only_approved_products", "true"),
        ])
    );
    assert_eq!(products.bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);

    // The next page carries Meta's cursor back.
    h.graph.push_json(200, catalog_answer());
    h.graph.push_json(200, json!({"data": []}));
    let next = h
        .call(get(
            format!(
                "/v1/wabas/{WABA}/catalogs/{CATALOG}/products?cursor={}",
                hex::encode("MjQZD")
            ),
            &key,
        ))
        .await;
    assert_eq!(next.status, StatusCode::OK, "{}", next.text);
    assert_eq!(next.json()["next_cursor"], Value::Null);
    let requests = h.graph.requests();
    assert_eq!(
        query(&requests[requests.len() - 1])
            .last()
            .map(|(k, v)| (k.as_str(), v.as_str())),
        Some(("after", "MjQZD"))
    );

    let sent = h.graph.requests().len();
    for (path, field) in [
        (format!("/v1/wabas/{WABA}/catalogs/12a"), "catalog_id"),
        (
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products?limit=0"),
            "limit",
        ),
        (
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products?error_type=x"),
            "query",
        ),
        (
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products?limit=101"),
            "limit",
        ),
        (
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products?cursor=zz"),
            "cursor",
        ),
        (
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products?filter=x"),
            "filter",
        ),
    ] {
        let refused = h.call(get(path.clone(), &key)).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path}: {}",
            refused.text
        );
        assert_eq!(error(&refused)["field"], field, "{path}");
    }
    assert_eq!(h.graph.requests().len(), sent, "refused before any request");
}

/// A creation or a change Meta refuses answers its code, never its text,
/// `details` included: Meta's texts may quote the prefilled message. The
/// change's refusal comes after its lookup found the code. Decisive:
/// `QrCodes::failed_with_message`, on the creation and on the change.
#[tokio::test]
async fn a_refused_creation_or_change_answers_no_text_of_metas() {
    let (h, key) = connected().await;
    let refusal = || {
        json!({"error": {"message": "(#100) Invalid parameter SENTINEL-META-TEXT",
                         "type": "OAuthException", "code": 100,
                         "error_data": {"details": "SENTINEL-META-TEXT"},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}})
    };
    h.graph.push_json(400, refusal());
    let created = h
        .call(
            call(Method::POST, format!("/v1/numbers/{PN}/qr-codes"), &key)
                .json(&json!({"prefilled_message": "Cyber Monday"})),
        )
        .await;
    assert!(created.status.is_client_error(), "{}", created.text);
    assert!(
        !created.text.contains("SENTINEL-META-TEXT"),
        "{}",
        created.text
    );
    assert_eq!(error(&created)["graph"]["code"], 100);
    assert!(
        error(&created)["graph"]["details"].is_null(),
        "{}",
        created.text
    );

    h.graph
        .push_json(200, json!({"data": [code_answer("Cyber Monday")]}));
    h.graph.push_json(400, refusal());
    let changed = h
        .call(
            call(
                Method::PATCH,
                format!("/v1/numbers/{PN}/qr-codes/{CODE}"),
                &key,
            )
            .json(&json!({"prefilled_message": "Black Friday"})),
        )
        .await;
    assert!(changed.status.is_client_error(), "{}", changed.text);
    assert!(
        !changed.text.contains("SENTINEL-META-TEXT"),
        "{}",
        changed.text
    );
    assert_eq!(error(&changed)["graph"]["code"], 100);
    assert!(
        error(&changed)["graph"]["details"].is_null(),
        "{}",
        changed.text
    );
    assert_eq!(h.graph.remaining(), 0);
}

// ─── QR codes ────────────────────────────────────────────────────────────

fn code_answer(message: &str) -> Value {
    json!({"code": CODE, "prefilled_message": message,
           "deep_link_url": format!("https://wa.me/message/{CODE}")})
}

/// qr-codes, "Create QR code" (its example, the image asked for in SVG):
/// `201` with Meta's answer; the same `Idempotency-Key` and body replays
/// without a second request, the key with another body is
/// `idempotency_key_reused`. Refused before any request: an empty or a
/// 141-character message, a format other than `PNG` or `SVG`, a key
/// outside the object. Decisive: `new_qr_code`, the fingerprint.
#[tokio::test]
async fn a_qr_code_is_created_under_a_key_and_its_limits_are_metas() {
    let (h, key) = connected().await;
    let create = |body: &Value| {
        call(Method::POST, format!("/v1/numbers/{PN}/qr-codes"), &key)
            .header("idempotency-key", "qr-cyber-monday")
            .json(body)
    };
    let body = json!({"prefilled_message": "Cyber Monday", "generate_qr_image": "SVG"});
    let mut answer = code_answer("Cyber Monday");
    answer["qr_image_url"] = json!("https://scontent-iad3-2.xx.fbcdn.net/...");
    h.graph.push_json(200, answer);
    let created = h.call(create(&body)).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert_eq!(
        created.json(),
        json!({"code": CODE, "prefilled_message": "Cyber Monday",
               "deep_link_url": format!("https://wa.me/message/{CODE}"),
               "qr_image_url": "https://scontent-iad3-2.xx.fbcdn.net/..."})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{PN}/message_qrdls"));
    assert_eq!(request.url.query(), None);
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(request.json(), Some(body.clone()));

    let replay = h.call(create(&body)).await;
    assert_eq!(replay.status, StatusCode::CREATED, "{}", replay.text);
    assert_eq!(replay.json(), created.json());
    assert_eq!(
        replay.headers.get("idempotent-replayed").unwrap(),
        "true",
        "a repeat"
    );
    assert_eq!(h.graph.requests().len(), 1, "no second request");
    let reused = h
        .call(create(&json!({"prefilled_message": "Cyber Tuesday"})))
        .await;
    assert_eq!(reused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error(&reused)["code"], "idempotency_key_reused");
    assert_eq!(h.graph.requests().len(), 1);
    assert_eq!(h.graph.remaining(), 0);

    for (body, field) in [
        (json!({"prefilled_message": ""}), "prefilled_message"),
        (
            json!({"prefilled_message": "x".repeat(141)}),
            "prefilled_message",
        ),
        (
            json!({"prefilled_message": "hi", "generate_qr_image": "png"}),
            "generate_qr_image",
        ),
        (json!({"prefilled_message": "hi", "image": "PNG"}), "body"),
        (json!({}), "body"),
    ] {
        let invalid = h
            .call(
                call(Method::POST, format!("/v1/numbers/{PN}/qr-codes"), &key)
                    .header("idempotency-key", "qr-invalid")
                    .json(&body),
            )
            .await;
        assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(error(&invalid)["field"], field, "{body}");
    }
    // 140 characters are Meta's maximum.
    h.graph.push_json(200, code_answer(&"é".repeat(140)));
    let longest = h
        .call(
            call(Method::POST, format!("/v1/numbers/{PN}/qr-codes"), &key)
                .json(&json!({"prefilled_message": "é".repeat(140)})),
        )
        .await;
    assert_eq!(longest.status, StatusCode::CREATED, "{}", longest.text);
    assert_eq!(h.graph.requests().len(), 2, "only the two creations");
    assert_eq!(h.graph.remaining(), 0);
}

/// qr-codes, "Get list", "Get a QR code", "Update", "Delete": the list
/// takes Meta's limit (25) and a cursor; a read, a change and a deletion
/// look the code up in the **number's own list** first (`code` filter), and
/// act on it only when that list holds it. A code of another number (not in
/// the number's list) is `404`, with the lookup as the only request: nothing
/// is changed or deleted (D144). Decisive: `QrCodes::owned`, before the
/// update and the delete.
#[tokio::test]
#[allow(clippy::too_many_lines)] // five routes, then the other number's code
async fn a_qr_code_is_the_numbers_or_it_does_not_exist() {
    let (h, key) = connected().await;
    let lookup = |request: &RecordedRequest, fields: Option<&str>| {
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.path(), format!("/v25.0/{PN}/message_qrdls"));
        let mut expected = Vec::new();
        if let Some(fields) = fields {
            expected.push(("fields", fields));
        }
        expected.push(("code", CODE));
        expected.push(("limit", "25"));
        assert_eq!(query(request), pairs(&expected));
        assert_eq!(request.bearer(), Some(TOKEN));
    };

    // The list: Meta's cursor and limit, the cursor opaque.
    h.graph.push_json(
        200,
        json!({"data": [code_answer("Cyber Monday")],
               "paging": {"cursors": {"before": "QVFI", "after": "MjQZD"},
                          "next": "https://graph.facebook.com/v25.0/x?after=MjQZD"}}),
    );
    let listed = h
        .call(get(format!("/v1/numbers/{PN}/qr-codes?limit=25"), &key))
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(listed.json()["data"][0]["code"], CODE);
    assert_eq!(listed.json()["next_cursor"], hex::encode("MjQZD"));
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.path(), format!("/v25.0/{PN}/message_qrdls"));
    assert_eq!(query(&request), pairs(&[("limit", "25")]));
    h.graph.push_json(200, json!({"data": []}));
    let next = h
        .call(get(
            format!(
                "/v1/numbers/{PN}/qr-codes?cursor={}&image_format=SVG",
                hex::encode("MjQZD")
            ),
            &key,
        ))
        .await;
    assert_eq!(next.status, StatusCode::OK, "{}", next.text);
    assert_eq!(
        query(&h.graph.last_request().unwrap()),
        pairs(&[
            (
                "fields",
                "code,prefilled_message,deep_link_url,qr_image_url.format(SVG)"
            ),
            ("after", "MjQZD"),
        ])
    );

    // One code: the lookup is the read.
    h.graph
        .push_json(200, json!({"data": [code_answer("Cyber Monday")]}));
    let one = h
        .call(get(format!("/v1/numbers/{PN}/qr-codes/{CODE}"), &key))
        .await;
    assert_eq!(one.status, StatusCode::OK, "{}", one.text);
    assert_eq!(one.json()["prefilled_message"], "Cyber Monday");
    assert_eq!(one.json()["qr_image_url"], Value::Null);
    lookup(&h.graph.last_request().unwrap(), None);
    assert_eq!(h.graph.requests().len(), 3);

    // An update: the lookup, then Meta's update (the code in the body).
    h.graph
        .push_json(200, json!({"data": [code_answer("Cyber Monday")]}));
    h.graph.push_json(200, code_answer("Cyber Tuesday"));
    let updated = h
        .call(
            call(
                Method::PATCH,
                format!("/v1/numbers/{PN}/qr-codes/{CODE}"),
                &key,
            )
            .json(&json!({"prefilled_message": "Cyber Tuesday"})),
        )
        .await;
    assert_eq!(updated.status, StatusCode::OK, "{}", updated.text);
    assert_eq!(updated.json()["prefilled_message"], "Cyber Tuesday");
    let requests = h.graph.requests();
    lookup(&requests[3], None);
    assert_eq!(requests[4].method, Method::POST);
    assert_eq!(requests[4].path(), format!("/v25.0/{PN}/message_qrdls"));
    assert_eq!(
        requests[4].json(),
        Some(json!({"code": CODE, "prefilled_message": "Cyber Tuesday"}))
    );
    assert_eq!(requests[4].bearer(), Some(TOKEN));

    // A deletion: the lookup, then `DELETE /{pn}/message_qrdls/{code}`.
    h.graph
        .push_json(200, json!({"data": [code_answer("Cyber Tuesday")]}));
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h
        .call(call(
            Method::DELETE,
            format!("/v1/numbers/{PN}/qr-codes/{CODE}"),
            &key,
        ))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text);
    let requests = h.graph.requests();
    lookup(&requests[5], None);
    assert_eq!(requests[6].method, Method::DELETE);
    assert_eq!(
        requests[6].path(),
        format!("/v25.0/{PN}/message_qrdls/{CODE}")
    );
    assert_eq!(requests[6].bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);

    // The same code on the tenant's other number: Meta's list for that
    // number does not hold it (or holds another code), `404`, and the
    // lookup is all that was sent.
    for answer in [json!({"data": []}), json!({"data": [code_answer("x")]})] {
        let answer = if answer["data"].as_array().unwrap().is_empty() {
            answer
        } else {
            // A list that answers a different code than the one asked for.
            let mut other = answer;
            other["data"][0]["code"] = json!("OTHERCODE12345");
            other
        };
        for (method, body) in [
            (Method::GET, None),
            (
                Method::PATCH,
                Some(json!({"prefilled_message": "Hijacked"})),
            ),
            (Method::DELETE, None),
        ] {
            let before = h.graph.requests().len();
            h.graph.push_json(200, answer.clone());
            let mut request = call(
                method.clone(),
                format!("/v1/numbers/{PN_2}/qr-codes/{CODE}"),
                &key,
            );
            if let Some(body) = &body {
                request = request.json(body);
            }
            let refused = h.call(request).await;
            assert_eq!(
                refused.status,
                StatusCode::NOT_FOUND,
                "{method}: {}",
                refused.text
            );
            assert_eq!(error(&refused)["code"], "not_found");
            let requests = h.graph.requests();
            assert_eq!(requests.len(), before + 1, "{method}: only the lookup");
            assert_eq!(requests[before].method, Method::GET);
            assert_eq!(
                requests[before].path(),
                format!("/v25.0/{PN_2}/message_qrdls")
            );
        }
    }
    assert_eq!(h.graph.remaining(), 0);

    // Refused before any request.
    let sent = h.graph.requests().len();
    for (method, path, body, field) in [
        (Method::GET, "/qr-codes?limit=26", None, "limit"),
        (Method::GET, "/qr-codes?limit=0", None, "limit"),
        (
            Method::GET,
            "/qr-codes?image_format=GIF",
            None,
            "image_format",
        ),
        (Method::GET, "/qr-codes/short", None, "code"),
        (Method::DELETE, "/qr-codes/4O4YGZEG3RIVE-", None, "code"),
        (
            Method::PATCH,
            "/qr-codes/4O4YGZEG3RIVE1",
            Some(json!({"prefilled_message": ""})),
            "prefilled_message",
        ),
        (
            Method::PATCH,
            "/qr-codes/4O4YGZEG3RIVE1",
            Some(json!({"code": "x", "prefilled_message": "hi"})),
            "body",
        ),
    ] {
        let mut request = call(method.clone(), format!("/v1/numbers/{PN}{path}"), &key);
        if let Some(body) = &body {
            request = request.json(body);
        }
        let refused = h.call(request).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{method} {path}: {}",
            refused.text
        );
        assert_eq!(error(&refused)["field"], field, "{method} {path}");
    }
    assert_eq!(h.graph.requests().len(), sent);
}

// ─── Analytics ───────────────────────────────────────────────────────────

fn analytics_get(h: &Harness, key: &str, route: &str, query: &str) -> Call {
    let _ = h;
    get(format!("/v1/wabas/{WABA}/analytics/{route}?{query}"), key)
}

/// A range of 366 days from the look-back's start (a leap year's) is
/// allowed, so the bound is not narrower than Meta's year. Decisive:
/// `year_range`'s 366 days.
#[tokio::test]
async fn a_leap_years_range_is_allowed() {
    let (h, key) = connected().await;
    let now = now(&h);
    let a_year_back = now.replace_year(now.year() - 1).unwrap();
    let start = rfc(a_year_back + time::Duration::seconds(1));
    let end = rfc(a_year_back + time::Duration::days(366));
    h.graph.push_json(200, json!({"id": WABA}));
    let reply = h
        .call(analytics_get(
            &h,
            &key,
            "messaging",
            &format!("start={start}&end={end}&granularity=MONTH"),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// analytics, "Messaging analytics" (its example response): the field
/// expansion Meta documents, the data points answered with RFC 3339 times.
/// Decisive: the expansion's parameters, and the range's conversion.
#[tokio::test]
async fn messaging_analytics_are_a_field_expansion_with_the_ranges_times() {
    let (h, key) = connected().await;
    let end = now(&h) - time::Duration::days(1);
    let start = end - time::Duration::days(7);
    h.graph.push_json(
        200,
        json!({
            "analytics": {
                "phone_numbers": ["16505550111", "16505550112"],
                "country_codes": ["US", "BR"],
                "granularity": "DAY",
                "data_points": [
                    {"start": start.unix_timestamp(), "end": start.unix_timestamp() + 86400,
                     "sent": 196_093, "delivered": 179_715},
                    {"start": start.unix_timestamp() + 86400, "end": start.unix_timestamp() + 172_800,
                     "sent": 147_649, "delivered": 139_032}
                ]
            },
            "id": WABA
        }),
    );
    let reply = h
        .call(analytics_get(
            &h,
            &key,
            "messaging",
            &format!(
                "start={}&end={}&granularity=DAY&phone_numbers=16505550111,16505550112&country_codes=US,BR",
                rfc(start),
                rfc(end)
            ),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let body = reply.json();
    assert_eq!(body["granularity"], "DAY");
    assert_eq!(body["phone_numbers"], json!(["16505550111", "16505550112"]));
    assert_eq!(body["data_points"][0]["start"], rfc(start));
    assert_eq!(
        body["data_points"][0]["end"],
        rfc(start + time::Duration::days(1))
    );
    assert_eq!(body["data_points"][0]["sent"], 196_093);
    assert_eq!(body["data_points"][1]["delivered"], 139_032);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{WABA}"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        query(&request),
        pairs(&[(
            "fields",
            &format!(
                "analytics.start({}).end({}).granularity(DAY).phone_numbers([\"16505550111\",\"16505550112\"]).country_codes([\"US\",\"BR\"])",
                start.unix_timestamp(),
                end.unix_timestamp()
            )
        )])
    );
    assert_eq!(h.graph.remaining(), 0);

    // Meta has no object for the range: no data points, not an error.
    h.graph.push_json(200, json!({"id": WABA}));
    let empty = h
        .call(analytics_get(
            &h,
            &key,
            "messaging",
            &format!("start={}&end={}&granularity=MONTH", rfc(start), rfc(end)),
        ))
        .await;
    assert_eq!(empty.status, StatusCode::OK, "{}", empty.text);
    assert_eq!(empty.json()["data_points"], json!([]));
}

/// analytics, "Get monthly data, using all breakdowns" and "Pricing
/// analytics" (their example data points): every filter and breakdown is
/// written as the page does, the data points of every set answered flat.
/// Decisive: each enum written upper case, the dimensions' order.
#[tokio::test]
async fn conversation_and_pricing_analytics_carry_their_filters_and_breakdowns() {
    let (h, key) = connected().await;
    let end = now(&h);
    let start = end - time::Duration::days(30);
    let (s, e) = (start.unix_timestamp(), end.unix_timestamp());
    h.graph.push_json(
        200,
        json!({"conversation_analytics": {"data": [{"data_points": [
            {"start": s, "end": e, "conversation": 1558, "phone_number": "15550458206", "country": "US",
             "conversation_type": "REGULAR", "conversation_direction": "UNKNOWN",
             "conversation_category": "AUTHENTICATION", "cost": 15.58},
            {"start": s, "end": e, "conversation": 1433, "phone_number": "15550458206", "country": "US",
             "conversation_type": "FREE_ENTRY_POINT", "conversation_category": "SERVICE", "cost": 14.33}
        ]}]}, "id": WABA}),
    );
    let reply = h
        .call(analytics_get(
            &h,
            &key,
            "conversations",
            &format!(
                "start={}&end={}&granularity=MONTHLY&metric_types=COST,CONVERSATION&conversation_categories=AUTHENTICATION,SERVICE&conversation_types=REGULAR&conversation_directions=USER_INITIATED&dimensions=CONVERSATION_CATEGORY,COUNTRY,PHONE",
                rfc(start),
                rfc(end)
            ),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let points = reply.json()["data_points"].clone();
    assert_eq!(points.as_array().unwrap().len(), 2);
    assert_eq!(points[0]["conversation"], 1558);
    assert_eq!(points[0]["conversation_category"], "AUTHENTICATION");
    assert_eq!(points[0]["conversation_direction"], "UNKNOWN");
    assert_eq!(points[0]["cost"], 15.58);
    assert_eq!(points[1]["conversation_direction"], Value::Null);
    assert_eq!(
        query(&h.graph.last_request().unwrap()),
        pairs(&[(
            "fields",
            &format!(
                "conversation_analytics.start({s}).end({e}).granularity(MONTHLY).metric_types([\"COST\",\"CONVERSATION\"]).conversation_categories([\"AUTHENTICATION\",\"SERVICE\"]).conversation_types([\"REGULAR\"]).conversation_directions([\"USER_INITIATED\"]).dimensions([\"CONVERSATION_CATEGORY\",\"COUNTRY\",\"PHONE\"])"
            )
        )])
    );

    h.graph.push_json(
        200,
        json!({"pricing_analytics": {"data": [{"data_points": [
            {"start": s, "end": e, "country": "IN", "tier": "0:750000", "pricing_type": "REGULAR",
             "pricing_category": "AUTHENTICATION_INTERNATIONAL", "volume": 2, "cost": 4.6},
            {"start": s, "end": e, "country": "IN", "pricing_type": "FREE_CUSTOMER_SERVICE",
             "pricing_category": "SERVICE", "volume": 2, "cost": 0}
        ]}]}}),
    );
    let reply = h
        .call(analytics_get(
            &h,
            &key,
            "pricing",
            &format!(
                "start={}&end={}&granularity=DAILY&country_codes=US,IN&dimensions=PRICING_CATEGORY,PRICING_TYPE,TIER,COUNTRY&pricing_categories=MARKETING_LITE&metric_types=VOLUME",
                rfc(start),
                rfc(end)
            ),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let points = reply.json()["data_points"].clone();
    assert_eq!(points[0]["tier"], "0:750000");
    assert_eq!(
        points[0]["pricing_category"],
        "AUTHENTICATION_INTERNATIONAL"
    );
    assert_eq!(points[1]["tier"], Value::Null);
    assert_eq!(points[1]["pricing_type"], "FREE_CUSTOMER_SERVICE");
    assert_eq!(
        query(&h.graph.last_request().unwrap()),
        pairs(&[(
            "fields",
            &format!(
                "pricing_analytics.start({s}).end({e}).granularity(DAILY).country_codes([\"US\",\"IN\"]).metric_types([\"VOLUME\"]).pricing_categories([\"MARKETING_LITE\"]).dimensions([\"PRICING_CATEGORY\",\"PRICING_TYPE\",\"TIER\",\"COUNTRY\"])"
            )
        )])
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// analytics, "Getting all template analytics" (its example response): the
/// template ids are found among the WABA's own templates first (the walk,
/// D116); only then is `template_analytics` asked for, with the page's
/// parameters. A template id the WABA does not hold is `404` with the walk
/// as the only request (D145). Decisive: `all_templates_in_waba` before the
/// analytics.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the example, the other WABA's id, then the days
async fn template_analytics_ask_only_for_the_wabas_own_templates() {
    let (h, key) = connected().await;
    let end = now(&h);
    let start = end - time::Duration::days(3);
    let ids = "1421988012088524,2632273056924580";
    let day = start.unix_timestamp();
    let template_query = |ids: &str, extra: &str| {
        format!(
            "start={}&end={}&template_ids={ids}{extra}",
            rfc(start),
            rfc(end)
        )
    };
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1421988012088524"}, {"id": "2632273056924580"}, {"id": "9"}]}),
    );
    h.graph.push_json(
        200,
        json!({"data": [{
            "granularity": "DAILY", "product_type": "cloud_api",
            "data_points": [
                {"template_id": "1421988012088524", "start": day, "end": day + 86400,
                 "sent": 1, "delivered": 1, "read": 1,
                 "cost": [{"type": "amount_spent", "value": 0.01},
                          {"type": "cost_per_delivered", "value": 0.01}]},
                {"template_id": "2632273056924580", "start": day, "end": day + 86400,
                 "sent": 1, "delivered": 1, "read": 1,
                 "clicked": [{"type": "quick_reply_button", "button_content": "Contact Support", "count": 108}],
                 "cost": [{"type": "amount_spent", "value": 0.03}],
                 "website_purchases": 3}]}],
            "paging": {"cursors": {"before": "MAZDZD", "after": "MjQZD"},
                       "next": "https://graph.facebook.com/v25.0/x?after=MjQZD"}}),
    );
    let reply = h
        .call(analytics_get(
            &h,
            &key,
            "templates",
            &template_query(ids, "&metric_types=COST,CLICKED,DELIVERED,READ,SENT"),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let body = reply.json();
    assert_eq!(body["data"][0]["product_type"], "cloud_api");
    let second = &body["data"][0]["data_points"][1];
    assert_eq!(second["template_id"], "2632273056924580");
    assert_eq!(second["start"], rfc(start));
    assert_eq!(
        second["clicked"],
        json!([{"type": "quick_reply_button", "button_content": "Contact Support", "count": 108}])
    );
    assert_eq!(
        second["cost"],
        json!([{"type": "amount_spent", "value": 0.03}])
    );
    assert_eq!(
        second.get("website_purchases"),
        None,
        "not named by the page"
    );
    assert_eq!(body["next_cursor"], hex::encode("MjQZD"));
    let requests = h.graph.requests();
    let [walk, analytics] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_eq!(walk.method, Method::GET);
    assert_eq!(walk.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(query(walk), pairs(&[("fields", "id"), ("limit", "100")]));
    assert_eq!(walk.bearer(), Some(TOKEN));
    assert_eq!(
        analytics.path(),
        format!("/v25.0/{WABA}/template_analytics")
    );
    assert_eq!(
        query(analytics),
        pairs(&[
            ("start", &start.unix_timestamp().to_string()),
            ("end", &end.unix_timestamp().to_string()),
            ("granularity", "DAILY"),
            ("metric_types", "COST,CLICKED,DELIVERED,READ,SENT"),
            ("template_ids", "[1421988012088524,2632273056924580]"),
        ])
    );
    assert_eq!(analytics.bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);

    // A template of another WABA: the walk does not find it (the one page,
    // no next), `404`, and the analytics are not asked for.
    h.graph
        .push_json(200, json!({"data": [{"id": "1421988012088524"}]}));
    let other = h
        .call(analytics_get(
            &h,
            &key,
            "templates",
            &template_query("1421988012088524,777777777", ""),
        ))
        .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.text);
    assert_eq!(error(&other)["code"], "not_found");
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 3, "the walk only");
    assert_eq!(
        requests[2].path(),
        format!("/v25.0/{WABA}/message_templates")
    );
    assert_eq!(h.graph.remaining(), 0);

    // Days with the WABA's timezone: the dates as the page writes them.
    let (d0, d1) = (start.date(), end.date());
    h.graph
        .push_json(200, json!({"data": [{"id": "1421988012088524"}]}));
    h.graph.push_json(200, json!({"data": []}));
    let dated = h
        .call(analytics_get(
            &h,
            &key,
            "templates",
            &format!(
                "start={d0}&end={d1}&template_ids=1421988012088524&use_waba_timezone=true&cursor={}",
                hex::encode("MjQZD")
            ),
        ))
        .await;
    assert_eq!(dated.status, StatusCode::OK, "{}", dated.text);
    let last = h.graph.last_request().unwrap();
    assert_eq!(
        last.query("start").as_deref(),
        Some(d0.to_string().as_str())
    );
    assert_eq!(last.query("end").as_deref(), Some(d1.to_string().as_str()));
    assert_eq!(last.query("use_waba_timezone").as_deref(), Some("true"));
    assert_eq!(last.query("after").as_deref(), Some("MjQZD"));
    assert_eq!(h.graph.remaining(), 0);
}

/// The ranges are Meta's documented look-back and the service's cap: every
/// refusal is `422` on the field, with **no request** to Meta (D145).
/// Decisive: each bound of `analytics::year_range` and `template_query`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one table of refusals
async fn analytics_ranges_and_filters_are_bounded_before_any_request() {
    let (h, key) = connected().await;
    let now = now(&h);
    let ago = |days: i64| rfc(now - time::Duration::days(days));
    // The harness clock has a fraction of a second `now` (to the second)
    // lacks: a year back, to the second, is a second inside the look-back.
    let a_year_back = now.replace_year(now.year() - 1).unwrap();
    let ninety_days = rfc(now + time::Duration::seconds(1) - time::Duration::days(90));
    let year_ago = rfc(a_year_back + time::Duration::seconds(1));
    let a_second_older = rfc(a_year_back - time::Duration::seconds(1));
    let too_many = vec!["16505550111"; 51].join(",");
    let ten_ids = (1..=11)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let cases: Vec<(&str, String, &str)> = vec![
        // A year back is the look-back (messaging, conversation, pricing).
        (
            "messaging",
            format!("start={a_second_older}&end={}&granularity=DAY", ago(1)),
            "start",
        ),
        (
            "messaging",
            format!("start={}&end={}&granularity=DAY", ago(2), ago(2)),
            "end",
        ),
        (
            "messaging",
            format!("start={}&end={}&granularity=DAY", ago(1), ago(2)),
            "end",
        ),
        (
            "messaging",
            format!("start={}&end={}&granularity=DAILY", ago(2), ago(1)),
            "granularity",
        ),
        // Half hours: 31 days at most, the service's cap.
        (
            "messaging",
            format!("start={}&end={}&granularity=HALF_HOUR", ago(40), ago(8)),
            "end",
        ),
        (
            "conversations",
            format!("start={}&end={}&granularity=HALF_HOUR", ago(40), ago(8)),
            "end",
        ),
        (
            "pricing",
            format!("start={}&end={}&granularity=HALF_HOUR", ago(40), ago(8)),
            "end",
        ),
        // A range longer than the year.
        (
            "pricing",
            format!(
                "start={year_ago}&end={}&granularity=MONTHLY",
                rfc(now + time::Duration::days(10))
            ),
            "end",
        ),
        // A leap year's 366 days are allowed, one day more is not.
        (
            "pricing",
            format!(
                "start={year_ago}&end={}&granularity=MONTHLY",
                rfc(a_year_back + time::Duration::days(367))
            ),
            "end",
        ),
        (
            "pricing",
            "start=1700000000&end=1700003600&granularity=DAILY".to_owned(),
            "start",
        ),
        (
            "pricing",
            format!("start={}&end=x&granularity=DAILY", ago(2)),
            "end",
        ),
        // Filters: a value closing the expansion, a value Meta does not name.
        (
            "messaging",
            format!(
                "start={}&end={}&granularity=DAY&phone_numbers=1).end(1",
                ago(2),
                ago(1)
            ),
            "phone_numbers[0]",
        ),
        (
            "messaging",
            format!(
                "start={}&end={}&granularity=DAY&phone_numbers={too_many}",
                ago(2),
                ago(1)
            ),
            "phone_numbers",
        ),
        (
            "messaging",
            format!(
                "start={}&end={}&granularity=DAY&phone_numbers=165055501111650555011111",
                ago(2),
                ago(1)
            ),
            "phone_numbers[0]",
        ),
        (
            "messaging",
            format!(
                "start={}&end={}&granularity=DAY&country_codes=U1",
                ago(2),
                ago(1)
            ),
            "country_codes[0]",
        ),
        (
            "messaging",
            format!(
                "start={}&end={}&granularity=DAY&country_codes=US,USA",
                ago(2),
                ago(1)
            ),
            "country_codes[1]",
        ),
        (
            "conversations",
            format!(
                "start={}&end={}&granularity=DAILY&dimensions=COUNTRY,country",
                ago(2),
                ago(1)
            ),
            "dimensions[1]",
        ),
        (
            "conversations",
            format!(
                "start={}&end={}&granularity=DAILY&metric_types=VOLUME",
                ago(2),
                ago(1)
            ),
            "metric_types[0]",
        ),
        (
            "pricing",
            format!(
                "start={}&end={}&granularity=DAILY&pricing_types=PAID",
                ago(2),
                ago(1)
            ),
            "pricing_types[0]",
        ),
        (
            "messaging",
            format!("start={}&end={}&granularity=DAY&products=1", ago(2), ago(1)),
            "query",
        ),
        // Templates: 90 days, ten ids, Meta ids, days with the WABA's
        // timezone, Meta's metric names.
        (
            "templates",
            format!("start={}&end={}&template_ids=1", ago(91), ago(1)),
            "start",
        ),
        (
            "templates",
            format!(
                "start={}&end={}&template_ids=1",
                ago(80),
                rfc(now + time::Duration::days(11))
            ),
            "end",
        ),
        (
            "templates",
            format!("start={}&end={}&template_ids={ten_ids}", ago(2), ago(1)),
            "template_ids",
        ),
        (
            "templates",
            format!("start={}&end={}&template_ids=1,01", ago(2), ago(1)),
            "template_ids[1]",
        ),
        (
            "templates",
            format!(
                "start={}&end={}&template_ids=1&use_waba_timezone=true",
                ago(2),
                ago(1)
            ),
            "start",
        ),
        (
            "templates",
            format!(
                "start={}&end={}&template_ids=1&metric_types=sent",
                ago(2),
                ago(1)
            ),
            "metric_types[0]",
        ),
        (
            "templates",
            format!("start={}&end={}&template_ids=1&cursor=zz", ago(2), ago(1)),
            "cursor",
        ),
        (
            "templates",
            format!("start={}&end={}", ago(2), ago(1)),
            "query",
        ),
    ];
    for (route, query, field) in &cases {
        let reply = h.call(analytics_get(&h, &key, route, query)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{route}?{query}: {}",
            reply.text
        );
        assert_eq!(error(&reply)["field"], *field, "{route}?{query}");
    }
    assert_nothing_sent(&h);

    // The bounds themselves are accepted: exactly a year back, 31 days of
    // half hours, 90 days of templates.
    for (route, query, answer) in [
        (
            "messaging",
            format!("start={year_ago}&end={}&granularity=MONTH", ago(1)),
            json!({"id": WABA}),
        ),
        (
            "conversations",
            format!("start={}&end={}&granularity=HALF_HOUR", ago(31), ago(0)),
            json!({"id": WABA}),
        ),
    ] {
        h.graph.push_json(200, answer);
        let reply = h.call(analytics_get(&h, &key, route, &query)).await;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "{route}?{query}: {}",
            reply.text
        );
    }
    h.graph.push_json(200, json!({"data": [{"id": "1"}]}));
    h.graph.push_json(200, json!({"data": []}));
    let templates = h
        .call(analytics_get(
            &h,
            &key,
            "templates",
            &format!("start={ninety_days}&end={}&template_ids=1", ago(0)),
        ))
        .await;
    assert_eq!(templates.status, StatusCode::OK, "{}", templates.text);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── The block list ──────────────────────────────────────────────────────

const BLOCKED: &str = "+16505551234";
const BLOCKED_DIGITS: &str = "16505551234";
const BLOCKED_2: &str = "+14155559876";
const BLOCKED_USER_ID: &str = "US.13491208655302741918";

/// block-users, "Get blocked users" (its example): `limit` and Meta's
/// cursor, the paging link never answered. Refused before any request: a
/// page of 0 or 101.
#[tokio::test]
async fn the_block_list_is_paged_with_metas_cursor() {
    let (h, key) = connected().await;
    h.graph.push_json(
        200,
        json!({"data": [
            {"messaging_product": "whatsapp", "wa_id": BLOCKED_DIGITS},
            {"messaging_product": "whatsapp", "wa_id": "14155559876"}],
            "paging": {"cursors": {"after": "eyJvZAmZAzZAXQ", "before": "eyJvZAmZAzZAXQ"},
                       "next": "https://graph.facebook.com/v25.0/x?after=eyJvZAmZAzZAXQ&access_token=SECRET"}}),
    );
    let reply = h
        .call(get(
            format!("/v1/numbers/{PN}/blocked-users?limit=10"),
            &key,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let body = reply.json();
    assert_eq!(
        body["data"][0],
        json!({"wa_id": BLOCKED_DIGITS, "user_id": null, "parent_user_id": null})
    );
    assert_eq!(body["next_cursor"], hex::encode("eyJvZAmZAzZAXQ"));
    assert!(!reply.text.contains("SECRET"));
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{PN}/block_users"));
    assert_eq!(query(&request), pairs(&[("limit", "10")]));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);
    for limit in ["0", "101"] {
        let refused = h
            .call(get(
                format!("/v1/numbers/{PN}/blocked-users?limit={limit}"),
                &key,
            ))
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{limit}");
        assert_eq!(error(&refused)["field"], "limit");
    }
    assert_eq!(h.graph.requests().len(), 1);
}

/// block-users, "Block a user" and "Unblock a user" (their examples,
/// including the mixed answer with its `139100`): the exact bodies Meta
/// documents (`user` for a phone, `user_id` for a BSUID), person-by-person
/// outcomes with Meta's codes and never its text, a refusal of some still a
/// `200`. The unblock is Meta's `DELETE` with a body. A key replays.
/// Refused before any request: a number without its `+`, a parent BSUID, a
/// group, nobody, 1,001 people. Decisive: the body of `people`, the answer's
/// conversion.
#[tokio::test]
#[allow(clippy::too_many_lines)] // block, replay, unblock, then each refusal
async fn blocking_and_unblocking_are_metas_bodies_with_outcomes_by_person() {
    let (h, key) = connected().await;
    let block = |key: &str, idem: &str, body: &Value| {
        call(Method::POST, format!("/v1/numbers/{PN}/blocked-users"), key)
            .header("idempotency-key", idem)
            .json(body)
    };
    let body = json!({"users": [{"phone": BLOCKED}, {"user_id": BLOCKED_USER_ID},
                                {"phone": BLOCKED_2}]});
    h.graph.push_json(
        200,
        json!({
            "messaging_product": "whatsapp",
            "block_users": {
                "added_users": [
                    {"input": BLOCKED, "wa_id": BLOCKED_DIGITS},
                    {"input": BLOCKED_USER_ID, "user_id": BLOCKED_USER_ID}],
                "failed_users": [{
                    "input": BLOCKED_2, "wa_id": "14155559876",
                    "errors": [{"message": "Re-engagement required SENTINEL-META-TEXT", "code": 131_047,
                                "error_data": {"details": "User has not messaged in the last 24 hours"}}]}]
            },
            "error": {"message": "(#139100) Failed to block/unblock users SENTINEL-META-TEXT",
                      "type": "OAuthException", "code": 139_100,
                      "error_data": {"details": "Failed to block some users"},
                      "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}
        }),
    );
    let reply = h.call(block(&key, "block-1", &body)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({
            "done": [
                {"input": BLOCKED, "wa_id": BLOCKED_DIGITS, "user_id": null},
                {"input": BLOCKED_USER_ID, "wa_id": null, "user_id": BLOCKED_USER_ID}],
            "refused": [{"input": BLOCKED_2, "wa_id": "14155559876", "user_id": null,
                         "reasons": [{"code": 131_047, "kind": "customer_service_window_closed"}]}],
            "error_code": 139_100
        })
    );
    assert!(!reply.text.contains("SENTINEL-META-TEXT"), "{}", reply.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{PN}/block_users"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        request.json(),
        Some(json!({"messaging_product": "whatsapp", "block_users": [
            {"user": BLOCKED}, {"user_id": BLOCKED_USER_ID}, {"user": BLOCKED_2}]}))
    );
    let again = h.call(block(&key, "block-1", &body)).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(again.json(), reply.json());
    assert_eq!(h.graph.requests().len(), 1, "the key replayed it");

    // A phone with its BSUID: both are sent (Meta then uses the phone).
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "block_users": {"removed_users": [
            {"input": BLOCKED, "wa_id": BLOCKED_DIGITS, "user_id": BLOCKED_USER_ID}]}}),
    );
    let unblocked = h
        .call(
            call(
                Method::POST,
                format!("/v1/numbers/{PN}/blocked-users/unblock"),
                &key,
            )
            .json(&json!({"users": [{"phone": BLOCKED, "user_id": BLOCKED_USER_ID}]})),
        )
        .await;
    assert_eq!(unblocked.status, StatusCode::OK, "{}", unblocked.text);
    assert_eq!(
        unblocked.json(),
        json!({"done": [{"input": BLOCKED, "wa_id": BLOCKED_DIGITS, "user_id": BLOCKED_USER_ID}],
               "refused": [], "error_code": null})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::DELETE);
    assert_eq!(request.path(), format!("/v25.0/{PN}/block_users"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        request.json(),
        Some(json!({"messaging_product": "whatsapp", "block_users": [
            {"user": BLOCKED, "user_id": BLOCKED_USER_ID}]}))
    );
    assert_eq!(h.graph.remaining(), 0);

    // Meta answering `139100` with an error status: its code, never its text.
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#139100) Failed to block/unblock users SENTINEL-META-TEXT",
                         "type": "OAuthException", "code": 139_100,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let failed = h.call(block(&key, "block-2", &body)).await;
    assert_eq!(failed.status, StatusCode::BAD_GATEWAY, "{}", failed.text);
    assert!(
        !failed.text.contains("SENTINEL-META-TEXT"),
        "{}",
        failed.text
    );
    assert_eq!(error(&failed)["graph"]["code"], 139_100);

    let sent = h.graph.requests().len();
    let people = |n: usize| json!({"users": vec![json!({"phone": BLOCKED}); n]});
    for (body, field) in [
        (
            json!({"users": [{"phone": BLOCKED_DIGITS}]}),
            "users[0].phone",
        ),
        (
            json!({"users": [{"user_id": "US.ENT.11815799212886844830"}]}),
            "users[0].user_id",
        ),
        (json!({"users": [{"group_id": "120363"}]}), "body"),
        (json!({"users": [{}]}), "users[0]"),
        (json!({"users": []}), "users"),
        (people(1001), "users"),
        (json!({"block_users": [{"user": BLOCKED}]}), "body"),
    ] {
        for route in ["blocked-users", "blocked-users/unblock"] {
            let refused = h
                .call(call(Method::POST, format!("/v1/numbers/{PN}/{route}"), &key).json(&body))
                .await;
            assert_eq!(
                refused.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{route} {body}: {}",
                refused.text
            );
            assert_eq!(error(&refused)["field"], field, "{route}");
        }
    }
    assert_eq!(h.graph.requests().len(), sent, "refused before any request");
}

// ─── Scopes and tenants ──────────────────────────────────────────────────

/// Every route of the item takes its own scope, and none of the others
/// stands in for it (D140): a key with every other scope is `403 forbidden`
/// on each, with no request to Meta. Decisive: `Operation::scope`, and the
/// router's `tenant(…, Scope::…)`.
#[tokio::test]
async fn each_family_takes_its_own_scope() {
    let (h, _) = connected().await;
    let others = [
        Scope::Send,
        Scope::Numbers,
        Scope::Templates,
        Scope::Payments,
        Scope::Inbox,
    ];
    let routes: [(Scope, Method, String, Option<Value>); 4] = [
        (
            Scope::Commerce,
            Method::PATCH,
            format!("/v1/numbers/{PN}/commerce-settings"),
            Some(json!({"is_cart_enabled": true})),
        ),
        (
            Scope::QrCodes,
            Method::DELETE,
            format!("/v1/numbers/{PN}/qr-codes/{CODE}"),
            None,
        ),
        (
            Scope::Analytics,
            Method::GET,
            format!(
                "/v1/wabas/{WABA}/analytics/messaging?start={}&end={}&granularity=DAY",
                rfc(now(&h) - time::Duration::days(2)),
                rfc(now(&h) - time::Duration::days(1))
            ),
            None,
        ),
        (
            Scope::Contacts,
            Method::POST,
            format!("/v1/numbers/{PN}/blocked-users"),
            Some(json!({"users": [{"phone": BLOCKED}]})),
        ),
    ];
    for (scope, method, path, body) in routes {
        let mut scopes = others.to_vec();
        scopes.extend(
            [
                Scope::Commerce,
                Scope::QrCodes,
                Scope::Analytics,
                Scope::Contacts,
            ]
            .into_iter()
            .filter(|other| *other != scope),
        );
        let wrong = h.tenant_key(TENANT, &scopes).await;
        let mut call = call(method.clone(), path.clone(), &wrong);
        if let Some(body) = &body {
            call = call.json(body);
        }
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, error(&reply)["code"].clone()),
            (StatusCode::FORBIDDEN, json!("forbidden")),
            "{method} {path}: {}",
            reply.text
        );
    }
    assert_nothing_sent(&h);
}

/// Another tenant's objects: tenant B's key on A's catalog, QR code or
/// block list is `404`, with no request to Meta and the vault unread
/// (the design's M5.1, here by name; `tests/authz.rs` iterates them all).
#[tokio::test]
async fn another_tenants_catalog_code_and_block_list_are_not_found() {
    let (h, _) = connected().await;
    let b_key = h
        .tenant_key(
            OTHER_TENANT,
            &[
                Scope::Commerce,
                Scope::QrCodes,
                Scope::Analytics,
                Scope::Contacts,
            ],
        )
        .await;
    let reads = h.kv.vault_reads();
    for (method, path, body) in [
        (
            Method::GET,
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products"),
            None,
        ),
        (
            Method::GET,
            format!("/v1/numbers/{PN}/qr-codes/{CODE}"),
            None,
        ),
        (
            Method::PATCH,
            format!("/v1/numbers/{PN}/qr-codes/{CODE}"),
            Some(json!({"prefilled_message": "Mine now"})),
        ),
        (
            Method::DELETE,
            format!("/v1/numbers/{PN}/qr-codes/{CODE}"),
            None,
        ),
        (Method::GET, format!("/v1/numbers/{PN}/blocked-users"), None),
        (
            Method::POST,
            format!("/v1/numbers/{PN}/blocked-users/unblock"),
            Some(json!({"users": [{"phone": BLOCKED}]})),
        ),
    ] {
        let mut call = call(method.clone(), path.clone(), &b_key);
        if let Some(body) = &body {
            call = call.json(body);
        }
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, error(&reply)["code"].clone()),
            (StatusCode::NOT_FOUND, json!("not_found")),
            "{method} {path}: {}",
            reply.text
        );
    }
    assert_eq!(h.kv.vault_reads(), reads, "the vault was not read");
    assert_nothing_sent(&h);
}
