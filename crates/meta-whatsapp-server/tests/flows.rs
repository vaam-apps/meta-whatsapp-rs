//! Roadmap M5d: WhatsApp Flows' management through the service (scope
//! `flows`; docs/design/server.md, section 4.2): a Flow created,
//! listed, read, changed, filled with its Flow JSON, previewed, published,
//! deprecated and deleted.
//!
//! Each test sends Meta's documented request and answer
//! (`flows/guides/flowsapi`): the exact requests (method, path, query, the
//! tenant's token, the JSON body or the multipart form), `remaining() ==
//! 0`. The design's M5.2: a Flow id another WABA holds is `404`, and the
//! Flow's read (`GET /{flow_id}`) is the only request made for it; a
//! refusal sends nothing. M5.1 (another tenant's WABA, zero vault reads)
//! is `tests/authz.rs`, which iterates every route of the document, these
//! included. The data endpoint is `tests/flows_endpoint.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::testing::{RecordedBody, RecordedRequest};
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TENANT: &str = "merchant-43";
const OTHER_WABA: &str = "102290129340399";
const FLOW: &str = "1234567890123456";

/// `GET /{flow_id}`'s fields (the library asks for all but the preview).
const DETAIL_FIELDS: &str = "id,name,status,categories,validation_errors,json_version,\
data_api_version,endpoint_uri,whatsapp_business_account,application";

/// A tenant with one WABA, and another tenant with another.
async fn connected() -> (Harness, String) {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &["106540352242924"], "EAAG-other")
        .await;
    let key = h.tenant_key(TENANT, &[Scope::Flows]).await;
    (h, key)
}

fn call(method: Method, path: &str, key: &str) -> Call {
    Call::new(method, path.to_owned()).key(key)
}

fn flows_path(rest: &str) -> String {
    format!("/v1/wabas/{WABA}/flows{rest}")
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

/// `GET /{flow_id}`'s answer for a Flow of `waba`.
fn flow_of(waba: &str) -> Value {
    json!({"id": FLOW, "name": "My first flow", "status": "DRAFT",
           "categories": ["LEAD_GENERATION"], "validation_errors": [],
           "json_version": "3.0", "data_api_version": "3.0",
           "endpoint_uri": "https://example.com",
           "whatsapp_business_account": {"id": waba, "name": "Lucky Shrub"},
           "application": {"id": "1"}})
}

/// The read of a Flow by id, made with the tenant's token.
fn assert_read(read: &RecordedRequest) {
    assert_eq!(read.method, Method::GET);
    assert_eq!(read.path(), format!("/v25.0/{FLOW}"));
    assert_eq!(query(read), pairs(&[("fields", DETAIL_FIELDS)]));
    assert_eq!(read.bearer(), Some(TOKEN));
}

/// flows/guides/flowsapi, "Creating a Flow" and its sample response: the
/// request Meta receives is the documented one (`POST /{WABA-ID}/flows`
/// with `name`, `categories`, `flow_json` as text and `publish`), the
/// validation errors come back as Meta wrote them, `201`. Decisive:
/// `flows::new_flow` and the library's `Flows::create`.
#[tokio::test]
async fn a_flow_is_created_from_metas_example() {
    let (h, key) = connected().await;
    let flow_json = r#"{"version":"5.0","screens":[{"id":"WELCOME_SCREEN","layout":{"type":"SingleColumnLayout","children":[{"type":"TextHeading","text":"Hello World"}]}}]}"#;
    h.graph.push_json(
        200,
        json!({"id": FLOW, "success": true, "validation_errors": [{
            "error": "INVALID_PROPERTY_VALUE", "error_type": "FLOW_JSON_ERROR",
            "message": "Invalid value found for property 'type'.",
            "line_start": 10, "line_end": 10, "column_start": 21, "column_end": 34,
            "pointers": [{"line_start": 10, "line_end": 10, "column_start": 21,
                          "column_end": 34, "path": "screens [0]. layout.children [0].type"}]}]}),
    );
    let body = json!({"name": "My first flow", "categories": ["OTHER"],
                      "flow_json": flow_json, "publish": true});
    let reply = h
        .call(call(Method::POST, &flows_path(""), &key).json(&body))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"id": FLOW, "validation_errors": [{
            "error": "INVALID_PROPERTY_VALUE", "error_type": "FLOW_JSON_ERROR",
            "message": "Invalid value found for property 'type'.",
            "line_start": 10, "line_end": 10, "column_start": 21, "column_end": 34,
            "pointers": [{"line_start": 10, "line_end": 10, "column_start": 21,
                          "column_end": 34, "path": "screens [0]. layout.children [0].type"}]}]})
    );
    let requests = h.graph.requests();
    let [created] = &requests[..] else {
        panic!("one request: {requests:?}")
    };
    assert_eq!(created.method, Method::POST);
    assert_eq!(created.path(), format!("/v25.0/{WABA}/flows"));
    assert_eq!(created.bearer(), Some(TOKEN));
    assert_eq!(created.json(), Some(body));
    assert_eq!(h.graph.remaining(), 0);

    // The Flow JSON as the object itself is Meta's text: compact.
    h.graph.push_json(200, json!({"id": FLOW, "success": true}));
    let object: Value = serde_json::from_str(flow_json).unwrap();
    let reply = h
        .call(call(Method::POST, &flows_path(""), &key).json(
            &json!({"name": "Hello", "categories": ["SURVEY", "OTHER"], "flow_json": object,
                        "endpoint_uri": "https://example.com/flows"}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(reply.json(), json!({"id": FLOW, "validation_errors": []}));
    let sent = h.graph.last_request().unwrap().json().unwrap();
    assert_eq!(sent["flow_json"], json!(object.to_string()));
    assert_eq!(sent["categories"], json!(["SURVEY", "OTHER"]));
    assert_eq!(sent["endpoint_uri"], "https://example.com/flows");
    assert_eq!(h.graph.remaining(), 0);
}

/// A creation naming a Flow to copy reads it first and copies only the
/// WABA's own; another WABA's is `404` with nothing created (M5.2).
/// Decisive: `Flows::create`'s read of `clone_flow_id`.
#[tokio::test]
async fn a_flow_is_cloned_only_from_the_wabas_own() {
    let (h, key) = connected().await;
    let body = json!({"name": "Copy", "categories": ["OTHER"], "clone_flow_id": FLOW});
    h.graph.push_json(200, flow_of(WABA));
    h.graph
        .push_json(200, json!({"id": "9999", "success": true}));
    let reply = h
        .call(call(Method::POST, &flows_path(""), &key).json(&body))
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    let requests = h.graph.requests();
    let [read, created] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_read(read);
    assert_eq!(created.path(), format!("/v25.0/{WABA}/flows"));
    assert_eq!(created.json(), Some(body.clone()));
    assert_eq!(h.graph.remaining(), 0);

    let (h, key) = connected().await;
    h.graph.push_json(200, flow_of(OTHER_WABA));
    let reply = h
        .call(call(Method::POST, &flows_path(""), &key).json(&body))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 1, "only the read: {requests:?}");
    assert_read(&requests[0]);
    assert_eq!(h.graph.remaining(), 0);
}

/// flows/guides/flowsapi, "Retrieving a List of Flows" (its sample
/// response) and "Retrieving Flow details": the list takes Meta's cursor
/// (the service's opaque one) and answers Meta's default fields; a page
/// size is not Meta's parameter, so it is refused; the details are the
/// documented fields. Decisive: the cursor's round trip, and the details'
/// `fields`.
#[tokio::test]
async fn flows_are_listed_and_read() {
    let (h, key) = connected().await;
    h.graph.push_json(
        200,
        json!({"data": [
            {"id": "flow-1", "name": "flow 1", "status": "DRAFT", "categories": ["CONTACT_US"], "validation_errors": []},
            {"id": "flow-2", "name": "flow 2", "status": "PUBLISHED", "categories": ["SURVEY"], "validation_errors": []}],
            "paging": {"cursors": {"before": "QVFIbefore", "after": "QVFIafter"},
                       "next": "https://graph.facebook.com/v25.0/next"}}),
    );
    let reply = h.call(call(Method::GET, &flows_path(""), &key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let page = reply.json();
    assert_eq!(page["data"][0]["id"], "flow-1");
    assert_eq!(page["data"][1]["status"], "PUBLISHED");
    assert_eq!(page["data"][1]["categories"], json!(["SURVEY"]));
    let cursor = page["next_cursor"].as_str().unwrap().to_owned();
    let requests = h.graph.requests();
    assert_eq!(requests[0].method, Method::GET);
    assert_eq!(requests[0].path(), format!("/v25.0/{WABA}/flows"));
    assert_eq!(query(&requests[0]), Vec::<(String, String)>::new());
    assert_eq!(requests[0].bearer(), Some(TOKEN));

    h.graph.push_json(
        200,
        json!({"data": [], "paging": {"cursors": {"before": "a", "after": "b"}}}),
    );
    let reply = h
        .call(call(
            Method::GET,
            &flows_path(&format!("?cursor={cursor}")),
            &key,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        query(&h.graph.last_request().unwrap()),
        pairs(&[("after", "QVFIafter")])
    );
    assert_eq!(h.graph.remaining(), 0);

    for bad in ["?limit=5", "?cursor=zz", "?after=QVFI"] {
        let reply = h.call(call(Method::GET, &flows_path(bad), &key)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), 2, "nothing sent for a refusal");

    h.graph.push_json(200, flow_of(WABA));
    let reply = h
        .call(call(Method::GET, &flows_path(&format!("/{FLOW}")), &key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"id": FLOW, "name": "My first flow", "status": "DRAFT",
               "categories": ["LEAD_GENERATION"], "validation_errors": [],
               "json_version": "3.0", "data_api_version": "3.0",
               "endpoint_uri": "https://example.com"})
    );
    assert_read(&h.graph.last_request().unwrap());
    assert_eq!(h.graph.remaining(), 0);
}

/// flows/guides/flowsapi, "Updating Flow's metadata": the Flow is read for
/// its WABA first, then `POST /{FLOW-ID}` carries the documented fields;
/// `204`. Refused before any request: an empty change, a key Meta's page
/// does not list, an `application_id` that is not digits, a blank name, an
/// endpoint that is not `https`. Decisive: `Flows::update`'s read and
/// `flows::flow_change`'s checks.
#[tokio::test]
async fn a_flows_metadata_is_changed() {
    let (h, key) = connected().await;
    h.graph.push_json(200, flow_of(WABA));
    h.graph.push_json(200, json!({"success": true}));
    let body = json!({"name": "New flow name", "categories": ["SURVEY"],
                      "endpoint_uri": "https://business.com/v2"});
    let reply = h
        .call(call(Method::PATCH, &flows_path(&format!("/{FLOW}")), &key).json(&body))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let requests = h.graph.requests();
    let [read, updated] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_read(read);
    assert_eq!(updated.method, Method::POST);
    assert_eq!(updated.path(), format!("/v25.0/{FLOW}"));
    assert_eq!(updated.bearer(), Some(TOKEN));
    assert_eq!(updated.json(), Some(body));
    assert_eq!(h.graph.remaining(), 0);

    // Connecting the Meta app (roadmap M5d2): the page's `application_id`.
    h.graph.push_json(200, flow_of(WABA));
    h.graph.push_json(200, json!({"success": true}));
    let connect = json!({"application_id": "1234567890"});
    let reply = h
        .call(call(Method::PATCH, &flows_path(&format!("/{FLOW}")), &key).json(&connect))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    assert_eq!(h.graph.last_request().unwrap().json(), Some(connect));
    assert_eq!(h.graph.remaining(), 0);
    let sent = h.graph.requests().len();

    for (body, field) in [
        (json!({}), "body"),
        (json!({"application_ref": "1"}), "body"),
        (json!({"application_id": "app one"}), "application_id"),
        (json!({"application_id": ""}), "application_id"),
        (json!({"name": " "}), "name"),
        (json!({"categories": []}), "categories"),
        (
            json!({"endpoint_uri": "http://business.com"}),
            "endpoint_uri",
        ),
    ] {
        let reply = h
            .call(call(Method::PATCH, &flows_path(&format!("/{FLOW}")), &key).json(&body))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
    }
    assert_eq!(h.graph.requests().len(), sent, "nothing sent for a refusal");
}

/// flows/guides/flowsapi, "Updating a Flow's Flow JSON": the Flow is read,
/// then `POST /{FLOW_ID}/assets` carries the documented form (`file` =
/// `flow.json` as `application/json`, `name`, `asset_type`), the bytes as
/// the caller sent them; the sample response's validation errors are the
/// answer. A body that is not a JSON object is refused before any request;
/// one over 2 MiB is `413`. Decisive: `Flows::upload_json`, and
/// `flows::flow_json_upload` keeping the bytes.
#[tokio::test]
async fn a_flow_json_is_uploaded_as_sent() {
    let (h, key) = connected().await;
    let text = "{\n  \"version\": \"5.0\",\n  \"screens\": []\n}";
    h.graph.push_json(200, flow_of(WABA));
    h.graph.push_json(
        200,
        json!({"success": true, "validation_errors": [{
            "error": "INVALID_PROPERTY_VALUE", "error_type": "FLOW_JSON_ERROR",
            "message": "Invalid value found for property 'type'.",
            "line_start": 10, "line_end": 10, "column_start": 21, "column_end": 34,
            "pointers": [{"line_start": 10, "line_end": 10, "column_start": 21,
                          "column_end": 34, "path": "screens [0]. layout.children [0].type"}]}]}),
    );
    let put = |body: Body| {
        call(Method::PUT, &flows_path(&format!("/{FLOW}/json")), &key)
            .header("content-type", "application/json")
            .body(body)
    };
    let reply = h.call(put(Body::from(text.to_owned()))).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let answer = reply.json();
    assert_eq!(answer["success"], true);
    assert_eq!(
        answer["validation_errors"][0]["error"],
        "INVALID_PROPERTY_VALUE"
    );
    assert_eq!(
        answer["validation_errors"][0]["pointers"][0]["column_end"],
        34
    );
    let requests = h.graph.requests();
    let [read, uploaded] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_read(read);
    assert_eq!(uploaded.method, Method::POST);
    assert_eq!(uploaded.path(), format!("/v25.0/{FLOW}/assets"));
    assert_eq!(uploaded.bearer(), Some(TOKEN));
    let RecordedBody::Multipart(parts) = &uploaded.body else {
        panic!("a multipart form: {:?}", uploaded.body)
    };
    let names: Vec<&str> = parts.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(names, ["file", "name", "asset_type"]);
    let (filename, content_type, data) = uploaded.multipart_field("file").unwrap();
    assert_eq!(
        (filename, content_type),
        (Some("flow.json"), Some("application/json"))
    );
    assert_eq!(&data[..], text.as_bytes());
    assert_eq!(
        &uploaded.multipart_field("name").unwrap().2[..],
        b"flow.json"
    );
    assert_eq!(
        &uploaded.multipart_field("asset_type").unwrap().2[..],
        b"FLOW_JSON"
    );
    assert_eq!(h.graph.remaining(), 0);

    for bad in ["", "[]", "\"x\"", "{", "null"] {
        let reply = h.call(put(Body::from(bad.to_owned()))).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{bad:?}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], "body", "{bad:?}");
    }
    let big = format!("{{\"x\": \"{}\"}}", "a".repeat(2 * 1024 * 1024));
    let reply = h.call(put(Body::from(big))).await;
    assert_eq!(
        reply.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        reply.text
    );
    assert_eq!(h.graph.requests().len(), 2, "nothing sent for a refusal");
}

/// flows/guides/flowsapi, "Visualizing and interacting with your Flow using
/// the Web Preview": the Flow is read, then `GET /{FLOW-ID}?fields=
/// preview.invalidate(false)` (or `true` when asked), the sample response's
/// link and expiry answered. Decisive: `Flows::preview`'s `invalidate`.
#[tokio::test]
async fn a_flows_preview_is_minted_or_revoked() {
    let (h, key) = connected().await;
    let preview = json!({"preview": {
        "preview_url": "https://business.facebook.com/wa/manage/flows/550.../preview/?token=b9d6....",
        "expires_at": "2023-05-21T11:18:09+0000"}, "id": "flow-1"});
    for (body, invalidate) in [(json!({}), "false"), (json!({"invalidate": true}), "true")] {
        h.graph.push_json(200, flow_of(WABA));
        h.graph.push_json(200, preview.clone());
        let reply = h
            .call(call(Method::POST, &flows_path(&format!("/{FLOW}/preview")), &key).json(&body))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        assert_eq!(
            reply.json(),
            json!({"preview_url": "https://business.facebook.com/wa/manage/flows/550.../preview/?token=b9d6....",
                   "expires_at": "2023-05-21T11:18:09+0000"})
        );
        let requests = h.graph.requests();
        let previewed = requests.last().unwrap();
        assert_eq!(previewed.method, Method::GET);
        assert_eq!(previewed.path(), format!("/v25.0/{FLOW}"));
        assert_eq!(
            query(previewed),
            pairs(&[("fields", &format!("preview.invalidate({invalidate})"))])
        );
        assert_eq!(previewed.bearer(), Some(TOKEN));
    }
    let reply = h
        .call(
            call(Method::POST, &flows_path(&format!("/{FLOW}/preview")), &key)
                .json(&json!({"invalidate": "yes"})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(h.graph.requests().len(), 4, "nothing sent for a refusal");
    assert_eq!(h.graph.remaining(), 0);
}

/// flows/guides/flowsapi, "Publishing a Flow", "Deprecating a Flow" and
/// "Deleting a Flow": each reads the Flow for its WABA, then makes the
/// documented request with the tenant's token; `204`. Decisive: each
/// operation's read, and its own Graph call.
#[tokio::test]
async fn a_flow_is_published_deprecated_and_deleted() {
    let (h, key) = connected().await;
    for (method, rest, graph_method, graph_path) in [
        (
            Method::POST,
            "/publish",
            Method::POST,
            format!("/v25.0/{FLOW}/publish"),
        ),
        (
            Method::POST,
            "/deprecate",
            Method::POST,
            format!("/v25.0/{FLOW}/deprecate"),
        ),
        (Method::DELETE, "", Method::DELETE, format!("/v25.0/{FLOW}")),
    ] {
        h.graph.push_json(200, flow_of(WABA));
        h.graph.push_json(200, json!({"success": true}));
        let reply = h
            .call(call(method, &flows_path(&format!("/{FLOW}{rest}")), &key))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::NO_CONTENT,
            "{rest}: {}",
            reply.text
        );
        let requests = h.graph.requests();
        let acted = requests.last().unwrap();
        assert_read(&requests[requests.len() - 2]);
        assert_eq!(acted.method, graph_method);
        assert_eq!(acted.path(), graph_path);
        assert_eq!(acted.bearer(), Some(TOKEN));
        assert_eq!(acted.body, RecordedBody::Empty);
    }
    assert_eq!(h.graph.requests().len(), 6);
    assert_eq!(h.graph.remaining(), 0);
}

/// The design's M5.2: a Flow id is the WABA's, or it does not exist.
/// Another WABA's Flow, a read that answers another id, a node with no
/// WABA, and an id Meta refuses are all `404 not_found` with nothing of
/// Meta's code or text, and the read is the only request made: nothing is
/// changed, uploaded, previewed, published, deprecated or deleted, on
/// every route that names a Flow. Decisive: `Flows::owned_flow`'s check of
/// the id and of the WABA.
#[tokio::test]
#[allow(clippy::too_many_lines)] // every route, every kind of other
async fn another_wabas_flow_is_not_found_and_nothing_is_acted_on() {
    let (h, key) = connected().await;
    let routes: Vec<(Method, String, Option<Value>)> = vec![
        (Method::GET, flows_path(&format!("/{FLOW}")), None),
        (
            Method::PATCH,
            flows_path(&format!("/{FLOW}")),
            Some(json!({"name": "x"})),
        ),
        (
            Method::PUT,
            flows_path(&format!("/{FLOW}/json")),
            Some(json!({"version": "5.0"})),
        ),
        (
            Method::POST,
            flows_path(&format!("/{FLOW}/preview")),
            Some(json!({})),
        ),
        (Method::POST, flows_path(&format!("/{FLOW}/publish")), None),
        (
            Method::POST,
            flows_path(&format!("/{FLOW}/deprecate")),
            None,
        ),
        (Method::DELETE, flows_path(&format!("/{FLOW}")), None),
    ];
    let others: Vec<(&str, Value, u16)> = vec![
        ("another WABA's", flow_of(OTHER_WABA), 200),
        (
            "another id",
            json!({"id": "9999", "whatsapp_business_account": {"id": WABA}}),
            200,
        ),
        (
            "no WABA",
            json!({"id": FLOW, "name": "a page, not a Flow"}),
            200,
        ),
        (
            "a WABA without an id",
            json!({"id": FLOW, "whatsapp_business_account": {"name": "x"}}),
            200,
        ),
        (
            "an id Meta refuses",
            json!({"error": {"message": "Unsupported get request. Object with ID '1234567890123456' does not exist",
                             "type": "GraphMethodException", "code": 100, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
            400,
        ),
    ];
    let mut checked = 0;
    for (method, path, body) in &routes {
        for (kind, answer, status) in &others {
            h.graph.push_json(*status, answer.clone());
            let mut request = call(method.clone(), path, &key);
            if let Some(body) = body {
                request = request.json(body);
            }
            let reply = h.call(request).await;
            let label = format!("{method} {path} ({kind})");
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{label}: {}",
                reply.text
            );
            assert!(
                reply.json()["error"]["graph"].is_null(),
                "{label}: {}",
                reply.text
            );
            // One request for each answer so far: the read, and no other.
            let requests = h.graph.requests();
            assert_eq!(
                requests.len(),
                checked + 1,
                "{label}: only the read: {requests:?}"
            );
            assert_read(requests.last().unwrap());
            assert_eq!(h.graph.remaining(), 0, "{label}");
            checked += 1;
        }
    }
    assert_eq!(checked, 35);
}

/// A Flow id that is not a Meta id (digits) names nothing: `422` on
/// `flow_id` before any request, on every route that names one.
/// Decisive: `flow_id`'s check, before the read.
#[tokio::test]
async fn a_flow_id_that_is_not_a_meta_id_is_refused_before_any_request() {
    let (h, key) = connected().await;
    for id in [
        "abc",
        "0123",
        "12%2F34",
        "1234567890123456789012345678901234567890123456789012345678901234567890",
    ] {
        for (method, rest, body) in [
            (Method::GET, "", None),
            (Method::PATCH, "", Some(json!({"name": "x"}))),
            (Method::PUT, "/json", Some(json!({"version": "5.0"}))),
            (Method::POST, "/preview", Some(json!({}))),
            (Method::POST, "/publish", None),
            (Method::POST, "/deprecate", None),
            (Method::DELETE, "", None),
        ] {
            let mut request = call(method.clone(), &flows_path(&format!("/{id}{rest}")), &key);
            if let Some(body) = body {
                request = request.json(&body);
            }
            let reply = h.call(request).await;
            assert_eq!(
                (reply.status, reply.json()["error"]["field"].as_str()),
                (StatusCode::UNPROCESSABLE_ENTITY, Some("flow_id")),
                "{method} {id}{rest}: {}",
                reply.text
            );
        }
    }
    assert!(h.graph.requests().is_empty());
}

/// Every refusal of a creation, before any request: Meta's object with no
/// other key, a name, 1 to 8 categories of Meta's spelling, a Meta id to
/// clone, an `https` endpoint, a Flow JSON that is an object or text of
/// at most 10 MiB. Decisive: each check of `flows::new_flow`.
#[tokio::test]
async fn a_creation_is_checked_before_any_request() {
    let (h, key) = connected().await;
    let good = json!({"name": "My first flow", "categories": ["OTHER"]});
    let with = |key: &str, value: Value| {
        let mut body = good.clone();
        body[key] = value;
        body
    };
    for (body, field) in [
        (json!("x"), "body"),
        (json!({"name": "x"}), "body"),
        (with("flow", json!(true)), "body"),
        (with("name", json!("")), "name"),
        (with("categories", json!([])), "categories"),
        (with("categories", json!(["other"])), "categories[0]"),
        (with("clone_flow_id", json!("flow-1")), "clone_flow_id"),
        (
            with("endpoint_uri", json!("http://example.com")),
            "endpoint_uri",
        ),
        (
            with("endpoint_uri", json!("https://user:secret@example.com")),
            "endpoint_uri",
        ),
        (with("flow_json", json!(42)), "flow_json"),
        (with("flow_json", json!("")), "flow_json"),
    ] {
        let reply = h
            .call(call(Method::POST, &flows_path(""), &key).json(&body))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{body}");
        assert!(!reply.text.contains("secret"), "{}", reply.text);
    }
    assert!(h.graph.requests().is_empty(), "nothing sent");
}

/// A creation takes an `Idempotency-Key` (design D135): the same key and
/// body replay the first answer with no second request; the same key with
/// another body is `422 idempotency_key_reused`, nothing sent. Decisive:
/// `Fingerprint::create_flow` and the route's `state.idempotent`.
#[tokio::test]
async fn a_creation_honours_its_idempotency_key() {
    let (h, key) = connected().await;
    let body = json!({"name": "My first flow", "categories": ["OTHER"]});
    let create = |body: &Value| {
        call(Method::POST, &flows_path(""), &key)
            .header("idempotency-key", "create-flow-1")
            .json(body)
    };
    h.graph.push_json(200, json!({"id": FLOW, "success": true}));
    let first = h.call(create(&body)).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    let replayed = h.call(create(&body)).await;
    assert_eq!(replayed.status, StatusCode::CREATED, "{}", replayed.text);
    assert_eq!(replayed.headers["idempotent-replayed"], "true");
    assert_eq!(replayed.json(), first.json());
    assert_eq!(h.graph.requests().len(), 1, "no second creation");
    let other = h
        .call(create(&json!({"name": "Another", "categories": ["OTHER"]})))
        .await;
    assert_eq!(
        other.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        other.text
    );
    assert_eq!(other.code(), "idempotency_key_reused");
    assert_eq!(h.graph.requests().len(), 1);
    assert_eq!(h.graph.remaining(), 0);
}

/// Meta's refusals keep the service's error model (section 5): a Flow JSON
/// with errors is an answer, not an error; a publication Meta refuses (the
/// publishing checks) is `422 invalid_parameter` with Meta's `details`; a
/// timeout of a change `may_have_been_sent`. Decisive: `Flows::failed`.
#[tokio::test]
async fn metas_refusals_keep_the_error_model() {
    let (h, key) = connected().await;
    h.graph.push_json(200, flow_of(WABA));
    h.graph.push_json(
        400,
        json!({"error": {"message": "Invalid parameter", "type": "OAuthException", "code": 100,
                         "error_data": {"details": "endpoint_uri: You need to set the endpoint URI before you can send or publish a flow."},
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let reply = h
        .call(call(
            Method::POST,
            &flows_path(&format!("/{FLOW}/publish")),
            &key,
        ))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.text
    );
    assert_eq!(reply.code(), "invalid_parameter");
    assert!(
        reply.json()["error"]["graph"]["details"]
            .as_str()
            .unwrap()
            .contains("endpoint_uri")
    );
    assert_eq!(reply.json()["error"]["may_have_been_sent"], false);
    assert_eq!(h.graph.remaining(), 0);
}

/// The scope (design D132): a key without `flows` reaches none of the
/// routes (`403 forbidden`) and nothing is sent, however many other
/// scopes it holds, `templates` included: a key that edits templates does
/// not manage Flows. And a key with `flows` alone is not a templates key.
/// Decisive: `Operation::scope`.
#[tokio::test]
async fn a_key_without_the_scope_is_forbidden() {
    let (h, _) = connected().await;
    let others: Vec<Scope> = Scope::ALL
        .into_iter()
        .filter(|scope| *scope != Scope::Flows)
        .collect();
    let key = h.tenant_key(TENANT, &others).await;
    for (method, path) in [
        (Method::GET, flows_path("")),
        (Method::GET, flows_path(&format!("/{FLOW}"))),
        (Method::DELETE, flows_path(&format!("/{FLOW}"))),
        (Method::POST, flows_path(&format!("/{FLOW}/publish"))),
    ] {
        let reply = h.call(call(method, &path, &key)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{path}"
        );
    }
    let flows_only = h.tenant_key(TENANT, &[Scope::Flows]).await;
    let templates = h
        .call(call(
            Method::GET,
            &format!("/v1/wabas/{WABA}/templates"),
            &flows_only,
        ))
        .await;
    assert_eq!(
        templates.status,
        StatusCode::FORBIDDEN,
        "{}",
        templates.text
    );
    assert!(h.graph.requests().is_empty());
}

/// A tenant's Flow may not name this service's data endpoint address for
/// another WABA as its `endpoint_uri` (design D136): Meta's request names
/// no tenant, so the address routes it, and that Flow's users' answers
/// would reach another tenant's app. Refused `422` on `endpoint_uri`, on
/// a creation and on a change, before any request; the WABA's own address
/// and any other URL are accepted. Decisive: `endpoint_names_waba` in
/// `Flows::create` and `Flows::update`.
#[tokio::test]
async fn a_flows_endpoint_may_not_be_another_wabas_data_endpoint_address() {
    let (h, key) = connected().await;
    let other = format!("https://flows.example.com/webhooks/flows/{OTHER_WABA}");
    let create = call(Method::POST, &flows_path(""), &key)
        .json(&json!({"name": "n", "categories": ["OTHER"], "endpoint_uri": other}));
    let reply = h.call(create).await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.text
    );
    assert_eq!(reply.json()["error"]["field"], "endpoint_uri");
    let patch = call(Method::PATCH, &flows_path(&format!("/{FLOW}")), &key)
        .json(&json!({ "endpoint_uri": other }));
    let reply = h.call(patch).await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.text
    );
    assert_eq!(reply.json()["error"]["field"], "endpoint_uri");
    assert!(h.graph.requests().is_empty(), "nothing was asked of Meta");

    // The WABA's own address is the point of the route.
    h.graph.push_json(200, json!({"id": FLOW, "success": true}));
    let own = format!("https://flows.example.com/webhooks/flows/{WABA}");
    let reply = h
        .call(
            call(Method::POST, &flows_path(""), &key)
                .json(&json!({"name": "n", "categories": ["OTHER"], "endpoint_uri": own})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
}
