//! Roadmap M5g2: what M5g did not route (docs/design/server.md, D280 to
//! D286): a catalog's product sets and one product by id, the products
//! edge's `filter`, template group analytics, and the two analytics
//! switches (template insights, button click tracking).
//!
//! Each test sends the requests the pages document and checks the exact
//! requests that reach Meta (method, path, query, the tenant's token, the
//! JSON body) and `remaining() == 0`. The design's M5.2: a set or a
//! product is its catalog's, a template group its WABA's, a template its
//! WABA's, and a refusal sends no request that acts. M5.1 (another
//! tenant's WABA, zero vault reads) is `tests/authz.rs`, which iterates
//! every route of the document, these included.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::clock::Clock as _;
use meta_whatsapp_rs::core::testing::RecordedRequest;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TENANT: &str = "merchant-43";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242924";
const CATALOG: &str = "194836987003835";
const OTHER_CATALOG: &str = "194836987003999";
const SET: &str = "5000000000000101";
const PRODUCT: &str = "5000000000000001";
const GROUP: &str = "1044106240855852";
const TEMPLATE: &str = "245435364965041";
const SENTINEL: &str = "SENTINEL-META-TEXT";

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
    let key = h
        .tenant_key(TENANT, &[Scope::Commerce, Scope::Analytics])
        .await;
    (h, key)
}

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

fn get(path: String, key: &str) -> Call {
    Call::get(path).key(key)
}

fn call(method: Method, path: String, key: &str) -> Call {
    Call::new(method, path).key(key)
}

fn error(reply: &common::Reply) -> Value {
    reply.json()["error"].clone()
}

fn enc(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn rfc(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap()
}

fn catalog_answer() -> Value {
    json!({"data": [{"id": CATALOG, "name": "Lucky Shrub", "product_count": 2, "vertical": "commerce"}]})
}

const SET_FIELDS: &str = "id,name,filter,retailer_id,product_count";
const PRODUCT_FIELDS: &str = "id,retailer_id,name,description,availability,condition,price,sale_price,currency,image_url,url,brand,review_status,visibility";

// ─── Product sets ────────────────────────────────────────────────────────

/// A catalog's product sets: the WABA's catalog list, then the catalog's
/// own `product_sets` edge with the fields of the product set page, paged
/// with Meta's cursor. Another WABA's catalog is `404` with the list as
/// the only request. Decisive: `Catalogs::get` before `product_sets`.
#[tokio::test]
async fn product_sets_are_listed_through_the_wabas_own_catalog() {
    let (h, key) = connected(false).await;
    h.graph.push_json(200, catalog_answer());
    h.graph.push_json(
        200,
        json!({"data": [{"id": SET, "name": "Shirts", "filter": "{\"product_type\":{\"i_contains\":\"shirt\"}}",
                         "retailer_id": "set-1", "product_count": 2}],
               "paging": {"cursors": {"before": "QVFI", "after": "MjQZD"},
                          "next": "https://graph.facebook.com/v25.0/x?after=MjQZD&access_token=SECRET"}}),
    );
    let reply = h
        .call(get(
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/product-sets?limit=5"),
            &key,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"data": [{"id": SET, "name": "Shirts",
                         "filter": "{\"product_type\":{\"i_contains\":\"shirt\"}}",
                         "retailer_id": "set-1", "product_count": 2}],
               "next_cursor": hex::encode("MjQZD")})
    );
    assert!(!reply.text.contains("SECRET"));
    let requests = h.graph.requests();
    let [list, sets] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(list.path(), format!("/v25.0/{WABA}/product_catalogs"));
    assert_eq!(sets.method, Method::GET);
    assert_eq!(sets.path(), format!("/v25.0/{CATALOG}/product_sets"));
    assert_eq!(
        query(sets),
        pairs(&[("fields", SET_FIELDS), ("limit", "5")])
    );
    assert_eq!(sets.bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);

    // The next page carries Meta's cursor back.
    h.graph.push_json(200, catalog_answer());
    h.graph.push_json(200, json!({"data": []}));
    let next = h
        .call(get(
            format!(
                "/v1/wabas/{WABA}/catalogs/{CATALOG}/product-sets?cursor={}",
                hex::encode("MjQZD")
            ),
            &key,
        ))
        .await;
    assert_eq!(next.status, StatusCode::OK, "{}", next.text);
    assert_eq!(
        h.graph.last_request().unwrap().query("after").as_deref(),
        Some("MjQZD")
    );

    // Another WABA's catalog: the list only.
    let before = h.graph.requests().len();
    h.graph.push_json(200, catalog_answer());
    let other = h
        .call(get(
            format!("/v1/wabas/{WABA}/catalogs/{OTHER_CATALOG}/product-sets"),
            &key,
        ))
        .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.text);
    assert_eq!(h.graph.requests().len(), before + 1);
    assert_eq!(h.graph.remaining(), 0);

    // Refused before any request.
    for path in [
        format!("/v1/wabas/{WABA}/catalogs/12a/product-sets"),
        format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/product-sets?limit=0"),
        format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/product-sets?limit=101"),
        format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/product-sets?cursor=zz"),
    ] {
        let refused = h.call(get(path.clone(), &key)).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
    }
    assert_eq!(h.graph.requests().len(), before + 1);
}

/// One product set: read for its catalog alone first
/// (`fields=product_catalog{id}`), in full only when that is the catalog
/// of the path, and the catalog the WABA's. A set of another catalog, a
/// set naming none, another node and a set Meta refuses are `404` with
/// **only the check's requests** sent. Decisive: the owner comparison and
/// the order in `Catalogs::product_set`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the owned set, then each way to not be one
async fn a_product_set_is_its_catalogs_or_it_does_not_exist() {
    let (h, key) = connected(false).await;
    let path = format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/product-sets/{SET}");
    h.graph.push_json(200, catalog_answer());
    h.graph
        .push_json(200, json!({"id": SET, "product_catalog": {"id": CATALOG}}));
    h.graph.push_json(
        200,
        json!({"id": SET, "name": "Shirts", "filter": "{}", "retailer_id": "set-1", "product_count": 2}),
    );
    let reply = h.call(get(path.clone(), &key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"id": SET, "name": "Shirts", "filter": "{}", "retailer_id": "set-1", "product_count": 2})
    );
    let requests = h.graph.requests();
    let [list, owner, read] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(list.path(), format!("/v25.0/{WABA}/product_catalogs"));
    assert_eq!(owner.method, Method::GET);
    assert_eq!(owner.path(), format!("/v25.0/{SET}"));
    assert_eq!(query(owner), pairs(&[("fields", "product_catalog{id}")]));
    assert_eq!(read.path(), format!("/v25.0/{SET}"));
    assert_eq!(query(read), pairs(&[("fields", SET_FIELDS)]));
    for request in &requests {
        assert_eq!(request.bearer(), Some(TOKEN));
    }
    assert_eq!(h.graph.remaining(), 0);

    // Each way not to be the catalog's: two requests, the check's.
    let not_owned = [
        json!({"id": SET, "product_catalog": {"id": OTHER_CATALOG}}),
        json!({"id": SET}),
        json!({"id": "5000000000000999", "product_catalog": {"id": CATALOG}}),
    ];
    for answer in not_owned {
        let before = h.graph.requests().len();
        h.graph.push_json(200, catalog_answer());
        h.graph.push_json(200, answer.clone());
        let reply = h.call(get(path.clone(), &key)).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{answer}: {}",
            reply.text
        );
        assert_eq!(error(&reply)["code"], "not_found");
        assert_eq!(h.graph.requests().len(), before + 2, "{answer}");
    }
    // The full read answers another node than the one asked for: `404`,
    // after the three requests. Decisive: the id comparison on the full
    // read in `Catalogs::product_set`.
    let before = h.graph.requests().len();
    h.graph.push_json(200, catalog_answer());
    h.graph
        .push_json(200, json!({"id": SET, "product_catalog": {"id": CATALOG}}));
    h.graph.push_json(
        200,
        json!({"id": "5000000000000999", "name": "Other", "filter": "{}"}),
    );
    let swapped = h.call(get(path.clone(), &key)).await;
    assert_eq!(swapped.status, StatusCode::NOT_FOUND, "{}", swapped.text);
    assert_eq!(error(&swapped)["code"], "not_found");
    assert_eq!(h.graph.requests().len(), before + 3);
    // Meta refusing the set's id: not found, no text of Meta's.
    let before = h.graph.requests().len();
    h.graph.push_json(200, catalog_answer());
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) {SENTINEL}"), "type": "GraphMethodException",
                         "code": 100, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h.call(get(path.clone(), &key)).await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND, "{}", refused.text);
    assert!(!refused.text.contains(SENTINEL));
    assert_eq!(h.graph.requests().len(), before + 2);
    // A catalog that is not the WABA's: the list only.
    let before = h.graph.requests().len();
    h.graph.push_json(200, catalog_answer());
    let other = h
        .call(get(
            format!("/v1/wabas/{WABA}/catalogs/{OTHER_CATALOG}/product-sets/{SET}"),
            &key,
        ))
        .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.text);
    assert_eq!(h.graph.requests().len(), before + 1);
    assert_eq!(h.graph.remaining(), 0);
    // Not Meta ids: nothing sent.
    let before = h.graph.requests().len();
    for path in [
        format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/product-sets/12a"),
        format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/product-sets/0123"),
    ] {
        let refused = h.call(get(path.clone(), &key)).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
        assert_eq!(error(&refused)["field"], "set_id");
    }
    assert_eq!(h.graph.requests().len(), before);
}

// ─── One product ─────────────────────────────────────────────────────────

/// One product by id: through the WABA's catalog's own products edge,
/// filtered on `product_item_id`, the product found among the answer with
/// that very id. A product the edge does not hold, and an answer that
/// ignores the filter, are `404`; another WABA's catalog is `404` with the
/// list only. Decisive: the id comparison in `Catalogs::product`, and the
/// order (catalog first).
#[tokio::test]
async fn a_product_is_found_only_through_its_catalogs_own_edge() {
    let (h, key) = connected(false).await;
    let path = format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products/{PRODUCT}");
    h.graph.push_json(200, catalog_answer());
    h.graph.push_json(
        200,
        json!({"data": [{"id": PRODUCT, "retailer_id": "sku-1", "name": "Mug", "price": "10.00 USD",
                         "availability": "in stock"}]}),
    );
    let reply = h.call(get(path.clone(), &key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json()["id"], PRODUCT);
    assert_eq!(reply.json()["retailer_id"], "sku-1");
    let requests = h.graph.requests();
    let [list, edge] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(list.path(), format!("/v25.0/{WABA}/product_catalogs"));
    assert_eq!(edge.method, Method::GET);
    assert_eq!(edge.path(), format!("/v25.0/{CATALOG}/products"));
    assert_eq!(
        query(edge),
        pairs(&[
            ("fields", PRODUCT_FIELDS),
            (
                "filter",
                &format!("{{\"product_item_id\":{{\"eq\":{PRODUCT}}}}}")
            ),
        ])
    );
    assert_eq!(edge.bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);

    // The edge ignores the filter and answers another product, or none.
    for data in [
        json!([{"id": "5000000000000002", "name": "Cap"}]),
        json!([]),
    ] {
        h.graph.push_json(200, catalog_answer());
        h.graph.push_json(200, json!({ "data": data }));
        let reply = h.call(get(path.clone(), &key)).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{data}: {}",
            reply.text
        );
        assert_eq!(error(&reply)["code"], "not_found");
    }
    // Another WABA's catalog: the list only.
    let before = h.graph.requests().len();
    h.graph.push_json(200, catalog_answer());
    let other = h
        .call(get(
            format!("/v1/wabas/{WABA}/catalogs/{OTHER_CATALOG}/products/{PRODUCT}"),
            &key,
        ))
        .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.text);
    assert_eq!(h.graph.requests().len(), before + 1);
    assert_eq!(h.graph.remaining(), 0);
    // Not a Meta id: nothing sent.
    let refused = h
        .call(get(
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products/sku-1"),
            &key,
        ))
        .await;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error(&refused)["field"], "product_id");
    assert_eq!(h.graph.requests().len(), before + 1);
}

// ─── The products edge's filter ──────────────────────────────────────────

/// The `filter` is Meta's documented JSON: parsed, then written from the
/// parsed value (its whitespace and a key given twice gone), and every
/// malformed or out-of-bounds one is `422` on `filter` with **zero
/// requests**. Decisive: `ProductFilter::parse`, and the products route
/// writing its value and not the query's text.
#[tokio::test]
async fn the_products_filter_is_parsed_and_bounded_before_any_request() {
    let (h, key) = connected(false).await;
    let base = format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products");
    h.graph.push_json(200, catalog_answer());
    h.graph.push_json(200, json!({"data": []}));
    let filter = enc(
        r#"{ "product_type" : {"i_contains":"shirt"}, "product_type": {"i_contains": "mug"} }"#,
    );
    let reply = h
        .call(get(format!("{base}?filter={filter}&limit=3"), &key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let edge = h.graph.last_request().unwrap();
    assert_eq!(
        query(&edge),
        pairs(&[
            ("fields", PRODUCT_FIELDS),
            ("filter", r#"{"product_type":{"i_contains":"mug"}}"#),
            ("limit", "3"),
        ])
    );
    assert_eq!(h.graph.remaining(), 0);

    // The value is URL-encoded once: a `&`, an `=`, a `%` and non-ASCII text
    // in a value reach Meta as one `filter` pair, decoded to the compact
    // JSON, and no other pair (no parameter can be added through it).
    h.graph.push_json(200, catalog_answer());
    h.graph.push_json(200, json!({"data": []}));
    let tricky = enc(r#"{"name": {"eq": "a&limit=100&access_token=x=%41 é中"}}"#);
    let reply = h
        .call(get(format!("{base}?filter={tricky}&limit=3"), &key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let edge = h.graph.last_request().unwrap();
    assert_eq!(
        query(&edge),
        pairs(&[
            ("fields", PRODUCT_FIELDS),
            (
                "filter",
                "{\"name\":{\"eq\":\"a&limit=100&access_token=x=%41 \u{e9}\u{4e2d}\"}}"
            ),
            ("limit", "3"),
        ])
    );
    let raw = edge.url.query().unwrap();
    assert!(raw.contains("%2541") && !raw.contains("%252541"), "{raw}");
    assert_eq!(h.graph.remaining(), 0);

    let sent = h.graph.requests().len();
    let deep = format!(
        r#"{{"name":{{"eq":{}1{}}}}}"#,
        "[".repeat(300),
        "]".repeat(300)
    );
    let long = format!(r#"{{"name":{{"eq":"{}"}}}}"#, "a".repeat(2048));
    for bad in [
        "x",
        "[]",
        "{}",
        r#"{"name":"shirt"}"#,
        r#"{"unknown":{"eq":"a"}}"#,
        r#"{"name":{"matches":"a"}}"#,
        r#"{"name":{"eq":null}}"#,
        r#"{"name":{"i_lt":1}}"#,
        r#"{"price":{"lt":1e999}}"#,
        r#"{"name":{"eq":[["a"]]}}"#,
        r#"{"name":{"eq":"a"}} {"x":1}"#,
        "\u{feff}{\"name\":{\"eq\":1}}",
        deep.as_str(),
        long.as_str(),
    ] {
        let refused = h
            .call(get(format!("{base}?filter={}", enc(bad)), &key))
            .await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            refused.text
        );
        assert_eq!(error(&refused)["field"], "filter", "{bad}");
    }
    assert_eq!(h.graph.requests().len(), sent, "refused before any request");
}

// ─── Template group analytics ────────────────────────────────────────────

fn group_answer(waba: &str) -> Value {
    json!({"id": GROUP, "name": "Seasonal", "whatsapp_business_account": {"id": waba}})
}

fn groups_get(path_query: &str, key: &str) -> Call {
    get(
        format!("/v1/wabas/{WABA}/analytics/template-groups?{path_query}"),
        key,
    )
}

/// analytics, "Template group analytics" (its example request and
/// response): every group id is read first and must name this WABA (D118),
/// then the analytics are asked for with the page's parameters. A group of
/// another WABA, among others, is `404` with the group reads as the only
/// requests. Decisive: `all_groups_in_waba` before the analytics.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the example, a foreign group, then the days
async fn template_group_analytics_ask_only_for_the_wabas_own_groups() {
    let (h, key) = connected(false).await;
    let end = h.clock.now().replace_nanosecond(0).unwrap();
    let start = end - time::Duration::days(3);
    let day = start.unix_timestamp();
    let q = format!(
        "start={}&end={}&group_ids={GROUP}&metric_types=sent,delivered,read",
        rfc(start),
        rfc(end)
    );
    h.graph.push_json(200, group_answer(WABA));
    h.graph.push_json(
        200,
        json!({"data": [{"granularity": "DAILY", "data_points": [
            {"template_group_id": GROUP, "start": day, "end": day + 86400,
             "sent": 1460, "delivered": 1460, "read": 1399}]}],
            "paging": {"cursors": {"before": "MAZDZD", "after": "MjQZD"},
                       "next": "https://graph.facebook.com/v25.0/x?after=MjQZD"}}),
    );
    let reply = h.call(groups_get(&q, &key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let body = reply.json();
    let point = &body["data"][0]["data_points"][0];
    assert_eq!(point["template_group_id"], GROUP);
    assert_eq!(point["start"], rfc(start));
    assert_eq!(point["sent"], 1460);
    assert_eq!(point["clicked"], json!([]));
    assert_eq!(body["next_cursor"], hex::encode("MjQZD"));
    let requests = h.graph.requests();
    let [group, analytics] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(group.method, Method::GET);
    assert_eq!(group.path(), format!("/v25.0/{GROUP}"));
    assert_eq!(group.bearer(), Some(TOKEN));
    assert_eq!(
        analytics.path(),
        format!("/v25.0/{WABA}/template_group_analytics")
    );
    assert_eq!(
        query(analytics),
        pairs(&[
            ("granularity", "daily"),
            ("start", &start.unix_timestamp().to_string()),
            ("end", &end.unix_timestamp().to_string()),
            ("metric_types", "sent,delivered,read"),
            ("template_group_ids", &format!("[{GROUP}]")),
        ])
    );
    assert_eq!(analytics.bearer(), Some(TOKEN));
    assert_eq!(h.graph.remaining(), 0);

    // Another WABA's group: its read, then no analytics.
    let before = h.graph.requests().len();
    h.graph.push_json(200, group_answer(OTHER_WABA));
    let other = h.call(groups_get(&q, &key)).await;
    assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.text);
    assert_eq!(error(&other)["code"], "not_found");
    assert_eq!(h.graph.requests().len(), before + 1);
    // The second of two is another's: two reads, no analytics.
    let two = format!(
        "start={}&end={}&group_ids={GROUP},9020555671393375",
        rfc(start),
        rfc(end)
    );
    h.graph.push_json(200, group_answer(WABA));
    h.graph.push_json(
        200,
        json!({"id": "9020555671393375", "whatsapp_business_account": {"id": OTHER_WABA}}),
    );
    let mixed = h.call(groups_get(&two, &key)).await;
    assert_eq!(mixed.status, StatusCode::NOT_FOUND, "{}", mixed.text);
    let requests = h.graph.requests();
    assert_eq!(requests.len(), before + 3);
    assert!(
        requests[before..]
            .iter()
            .all(|r| !r.path().contains("template_group_analytics"))
    );
    // A node that names no WABA, and one with another id: not groups.
    for answer in [
        json!({"id": GROUP}),
        json!({"id": "1", "whatsapp_business_account": {"id": WABA}}),
    ] {
        let before = h.graph.requests().len();
        h.graph.push_json(200, answer);
        let reply = h.call(groups_get(&q, &key)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
        assert_eq!(h.graph.requests().len(), before + 1);
    }
    assert_eq!(h.graph.remaining(), 0);

    // Days with the WABA's timezone, as the page writes them.
    let (d0, d1) = (start.date(), end.date());
    h.graph.push_json(200, group_answer(WABA));
    h.graph.push_json(200, json!({"data": []}));
    let dated = h
        .call(groups_get(
            &format!(
                "start={d0}&end={d1}&group_ids={GROUP}&use_waba_timezone=true&cursor={}",
                hex::encode("MjQZD")
            ),
            &key,
        ))
        .await;
    assert_eq!(dated.status, StatusCode::OK, "{}", dated.text);
    let last = h.graph.last_request().unwrap();
    assert_eq!(
        last.query("start").as_deref(),
        Some(d0.to_string().as_str())
    );
    assert_eq!(last.query("use_waba_timezone").as_deref(), Some("true"));
    assert_eq!(last.query("after").as_deref(), Some("MjQZD"));
    assert_eq!(h.graph.remaining(), 0);
}

/// The range is Meta's 90-day look-back (D145's rule): every refusal is
/// `422` on the field with **no request** (no group read either).
/// Decisive: each bound of `template_group_query`.
#[tokio::test]
async fn template_group_ranges_and_lists_are_bounded_before_any_request() {
    let (h, key) = connected(false).await;
    let now = h.clock.now().replace_nanosecond(0).unwrap();
    let at = |days: i64| rfc(now - time::Duration::days(days));
    let ids = ["1"; 11].join(",");
    let cases = [
        (
            format!("start={}&end={}&group_ids=1", at(91), at(0)),
            "start",
        ),
        (format!("start={}&end={}&group_ids=1", at(2), at(2)), "end"),
        (
            format!("start={}&end={}&group_ids=1", at(89), at(-2)),
            "end",
        ),
        (
            format!("start={}&end={}&group_ids={ids}", at(2), at(1)),
            "group_ids",
        ),
        (
            format!("start={}&end={}&group_ids=1,01", at(2), at(1)),
            "group_ids[1]",
        ),
        (
            format!(
                "start={}&end={}&group_ids=1&metric_types=SENT",
                at(2),
                at(1)
            ),
            "metric_types[0]",
        ),
        (
            format!(
                "start={}&end={}&group_ids=1&use_waba_timezone=true",
                at(2),
                at(1)
            ),
            "start",
        ),
        (
            format!("start={}&end={}&group_ids=1&x=1", at(2), at(1)),
            "query",
        ),
    ];
    for (q, field) in cases {
        let refused = h.call(groups_get(&q, &key)).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{q}: {}",
            refused.text
        );
        assert_eq!(error(&refused)["field"], field, "{q}");
    }
    assert!(h.graph.requests().is_empty(), "{:?}", h.graph.requests());
}

// ─── The two switches ────────────────────────────────────────────────────

fn insights(key: &str) -> Call {
    call(
        Method::POST,
        format!("/v1/wabas/{WABA}/analytics/template-insights"),
        key,
    )
}

/// analytics, "Confirming template analytics": a legal act (Meta: the
/// business directs Meta to add link tracking and to collect and
/// anonymize chat data; it cannot be disabled). With the legal acts off
/// it is `403 feature_not_available` and **nothing is asked of Meta**,
/// with a key or without. Decisive: the arm of `is_legal_act` and the
/// gate in `Analytics::enable_template_insights`.
#[tokio::test]
async fn with_the_legal_acts_off_the_insights_are_refused_without_a_request_to_meta() {
    let (h, key) = connected(false).await;
    for call in [
        insights(&key),
        insights(&key).header("idempotency-key", "ti-1"),
    ] {
        let reply = h.call(call).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
        assert_eq!(reply.code(), "feature_not_available");
    }
    assert!(h.graph.requests().is_empty(), "{:?}", h.graph.requests());
    assert_eq!(h.graph.remaining(), 0);
}

/// With the legal acts on: one request, the page's, with the tenant's
/// token, answering `204` and nothing of Meta's; a repeat under the same
/// key is replayed without a request; Meta's refusal answers its code and
/// no text; a key without the `analytics` scope is `403 forbidden` first.
/// Decisive: the idempotency wrapper, `failed_withholding_texts`, and the
/// scope of the operation.
#[tokio::test]
async fn with_the_legal_acts_on_the_insights_are_one_request_under_a_key() {
    let (h, key) = connected(true).await;
    h.graph
        .push_json(200, json!({"id": 102_290_129_340_398_u64}));
    let first = h
        .call(insights(&key).header("idempotency-key", "ti-1"))
        .await;
    assert_eq!(first.status, StatusCode::NO_CONTENT, "{}", first.text);
    assert!(first.text.is_empty());
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{WABA}"));
    assert_eq!(
        query(&request),
        pairs(&[("is_enabled_for_insights", "true")])
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    let again = h
        .call(insights(&key).header("idempotency-key", "ti-1"))
        .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);
    assert_eq!(h.graph.requests().len(), 1, "replayed, not sent twice");

    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) {SENTINEL}"), "type": "OAuthException", "code": 100,
                         "error_data": {"details": SENTINEL}, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h.call(insights(&key)).await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    assert!(!refused.text.contains(SENTINEL), "{}", refused.text);
    assert_eq!(error(&refused)["graph"]["code"], 100);
    assert!(error(&refused)["graph"]["details"].is_null());

    let wrong = h.tenant_key(TENANT, &[Scope::Commerce, Scope::Send]).await;
    let sent = h.graph.requests().len();
    let denied = h.call(insights(&wrong)).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.code(), "forbidden");
    assert_eq!(h.graph.requests().len(), sent);
    assert_eq!(h.graph.remaining(), 0);
}

fn tracking(template: &str, key: &str, opted_out: &Value) -> Call {
    call(
        Method::PUT,
        format!("/v1/wabas/{WABA}/analytics/templates/{template}/click-tracking"),
        key,
    )
    .json(&json!({ "opted_out": opted_out }))
}

fn script_template(h: &Harness, id: &str, in_waba: bool) {
    h.graph
        .push_json(200, json!({"name": "seasonal_sale_promo", "id": id}));
    let data = if in_waba {
        json!([{"id": id, "category": "MARKETING"}])
    } else {
        json!([])
    };
    h.graph.push_json(200, json!({ "data": data }));
}

/// analytics, "Disabling button click analytics": the template is found
/// among the WABA's own (its name, then the WABA's templates of that
/// name), its current category read and sent back exactly as Meta
/// returned it, and the opt-out set with the page's parameters; `204`, a
/// repeat under the same key replayed. A template of another WABA is `404`
/// with the two lookups only. Refused before any request: a body that is
/// not `{"opted_out": bool}`, an id that is no Meta id. Meta's refusal
/// answers its code and no text. Decisive: `Analytics::set_template_click_tracking`'s
/// lookup, and the category it sends.
#[tokio::test]
#[allow(clippy::too_many_lines)] // set, replayed, foreign, refused, withheld
async fn click_tracking_is_set_only_on_the_wabas_own_template_with_its_category() {
    let (h, key) = connected(false).await;
    script_template(&h, TEMPLATE, true);
    h.graph.push_json(200, json!({"success": true}));
    let set = h
        .call(tracking(TEMPLATE, &key, &json!(true)).header("idempotency-key", "ct-1"))
        .await;
    assert_eq!(set.status, StatusCode::NO_CONTENT, "{}", set.text);
    assert!(set.text.is_empty());
    let requests = h.graph.requests();
    let [name, list, post] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(name.path(), format!("/v25.0/{TEMPLATE}"));
    assert_eq!(list.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(list.query("fields").as_deref(), Some("category"));
    assert_eq!(list.query("name").as_deref(), Some("seasonal_sale_promo"));
    assert_eq!(post.method, Method::POST);
    assert_eq!(post.path(), format!("/v25.0/{TEMPLATE}"));
    assert_eq!(
        query(post),
        pairs(&[
            ("cta_url_link_tracking_opted_out", "true"),
            ("category", "MARKETING")
        ])
    );
    assert_eq!(post.bearer(), Some(TOKEN));
    let again = h
        .call(tracking(TEMPLATE, &key, &json!(true)).header("idempotency-key", "ct-1"))
        .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);
    assert_eq!(h.graph.requests().len(), 3, "replayed, not sent twice");

    // Opting back in, on a template of another category: the category the
    // lookup returned goes back as it came (no change of case, no fixed
    // value), so that a comparison at Meta never moves the template to
    // review. Decisive: the category argument of `set_button_click_tracking`.
    h.graph
        .push_json(200, json!({"name": "seasonal_sale_promo", "id": TEMPLATE}));
    h.graph.push_json(
        200,
        json!({"data": [{"id": TEMPLATE, "category": "UTILITY"}]}),
    );
    h.graph.push_json(200, json!({"success": true}));
    let back = h.call(tracking(TEMPLATE, &key, &json!(false))).await;
    assert_eq!(back.status, StatusCode::NO_CONTENT, "{}", back.text);
    let post = h.graph.last_request().unwrap();
    assert_eq!(post.method, Method::POST);
    assert_eq!(
        query(&post),
        pairs(&[
            ("cta_url_link_tracking_opted_out", "false"),
            ("category", "UTILITY")
        ])
    );

    // Another WABA's template: the two lookups, no change.
    let before = h.graph.requests().len();
    script_template(&h, TEMPLATE, false);
    let other = h.call(tracking(TEMPLATE, &key, &json!(true))).await;
    assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.text);
    assert_eq!(error(&other)["code"], "not_found");
    let requests = h.graph.requests();
    assert_eq!(requests.len(), before + 2);
    assert!(requests[before..].iter().all(|r| r.method == Method::GET));
    assert_eq!(h.graph.remaining(), 0);

    // Refused before any request.
    let before = h.graph.requests().len();
    for (call, field) in [
        (tracking(TEMPLATE, &key, &json!("yes")), "body"),
        (tracking("12a", &key, &json!(true)), "template_id"),
        (
            call(
                Method::PUT,
                format!("/v1/wabas/{WABA}/analytics/templates/{TEMPLATE}/click-tracking"),
                &key,
            )
            .json(&json!({"opted_out": true, "category": "UTILITY"})),
            "body",
        ),
    ] {
        let refused = h.call(call).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            refused.text
        );
        assert_eq!(error(&refused)["field"], field);
    }
    assert_eq!(h.graph.requests().len(), before);

    // Meta's refusal of the change: its code, never its text.
    script_template(&h, TEMPLATE, true);
    h.graph.push_json(
        400,
        json!({"error": {"message": format!("(#100) {SENTINEL}"), "type": "OAuthException", "code": 100,
                         "error_data": {"details": SENTINEL}, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = h.call(tracking(TEMPLATE, &key, &json!(true))).await;
    assert!(refused.status.is_client_error(), "{}", refused.text);
    assert!(!refused.text.contains(SENTINEL), "{}", refused.text);
    assert_eq!(error(&refused)["graph"]["code"], 100);
    assert!(error(&refused)["graph"]["details"].is_null());
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Scopes ──────────────────────────────────────────────────────────────

/// The commerce routes take `commerce` and the analytics ones `analytics`,
/// and a key with every other scope is `403 forbidden` on each, with
/// nothing sent. Decisive: `Operation::scope` of each new operation.
#[tokio::test]
async fn each_new_route_takes_its_own_scope() {
    let (h, _) = connected(true).await;
    let now = h.clock.now();
    let q = format!(
        "start={}&end={}&group_ids={GROUP}",
        rfc(now - time::Duration::days(2)),
        rfc(now - time::Duration::days(1))
    );
    let commerce: Vec<(Method, String, Option<Value>)> = vec![
        (
            Method::GET,
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/product-sets"),
            None,
        ),
        (
            Method::GET,
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/product-sets/{SET}"),
            None,
        ),
        (
            Method::GET,
            format!("/v1/wabas/{WABA}/catalogs/{CATALOG}/products/{PRODUCT}"),
            None,
        ),
    ];
    let analytics: Vec<(Method, String, Option<Value>)> = vec![
        (
            Method::GET,
            format!("/v1/wabas/{WABA}/analytics/template-groups?{q}"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/wabas/{WABA}/analytics/template-insights"),
            None,
        ),
        (
            Method::PUT,
            format!("/v1/wabas/{WABA}/analytics/templates/{TEMPLATE}/click-tracking"),
            Some(json!({"opted_out": true})),
        ),
    ];
    for (own, routes) in [(Scope::Commerce, commerce), (Scope::Analytics, analytics)] {
        let scopes: Vec<Scope> = Scope::ALL.into_iter().filter(|s| *s != own).collect();
        let wrong = h.tenant_key(TENANT, &scopes).await;
        for (method, path, body) in routes {
            let mut c = call(method.clone(), path.clone(), &wrong);
            if let Some(body) = &body {
                c = c.json(body);
            }
            let reply = h.call(c).await;
            assert_eq!(
                (reply.status, reply.code()),
                (StatusCode::FORBIDDEN, "forbidden".to_owned()),
                "{method} {path}: {}",
                reply.text
            );
        }
    }
    assert!(h.graph.requests().is_empty());
}

// ─── No other route confirms the insights ────────────────────────────────

/// **No other route confirms template insights**, with the legal acts on:
/// every route of the document that names a number or a WABA, but the
/// insights route itself, is called with a key of every scope, Meta
/// answering each request it gets, and no request ever carries
/// `is_enabled_for_insights`. Decisive: any other operation calling the
/// library's `enable_template_insights`.
#[tokio::test]
async fn no_other_route_ever_confirms_template_insights() {
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
        if operation.admin()
            || !numbered
            || operation.template.contains("stream")
            || operation.template.ends_with("/analytics/template-insights")
        {
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
    let asked = h.graph.requests();
    assert!(asked.len() > 50, "Meta was asked {} times", asked.len());
    assert!(
        asked
            .iter()
            .all(|r| r.query("is_enabled_for_insights").is_none()),
        "{:?}",
        asked
            .iter()
            .filter(|r| r.query("is_enabled_for_insights").is_some())
            .map(meta_whatsapp_rs::core::testing::RecordedRequest::path)
            .collect::<Vec<_>>()
    );
}

/// The confirmation's call is made once by the core's `analytics` (the
/// library's) and once by the insights route's handler (the core's), and
/// by no other source of the service's crates. Decisive: a second caller.
#[test]
fn only_the_analytics_core_names_the_insights_confirmation() {
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
                // Once the service's `Analytics` call in the core (the
                // library's), once the handler's (the service's).
                for _ in 0..code.matches(".enable_template_insights(").count() {
                    found.push(path.strip_prefix(crates).unwrap().display().to_string());
                }
            }
        }
    }
    found.sort();
    assert_eq!(
        found,
        [
            "meta-whatsapp-server-api-axum/src/api/analytics.rs".to_owned(),
            "meta-whatsapp-server-core/src/analytics.rs".to_owned(),
        ]
    );
}
