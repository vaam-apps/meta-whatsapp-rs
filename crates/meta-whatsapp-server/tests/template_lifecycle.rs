//! Roadmap M5b: a template's life after its creation, through the service
//! (scope `templates`; docs/design/server.md, section 4.2): an edit, an
//! unpause, a comparison, archiving and unarchiving, the Template Library,
//! a migration, template groups, and each kind of parity row 99 created
//! from Meta's example with no key dropped.
//!
//! Each test sends Meta's documented request and answer: the exact
//! requests (method, path, query, the tenant's token, the JSON body),
//! `remaining() == 0`. The design's M5.2: an id another WABA holds is `404`
//! with nothing acted on, and a refusal sends nothing. M5.1 (another
//! tenant's WABA, zero vault reads) is `tests/authz.rs`, which iterates
//! every route of the document, these included.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness};
use meta_whatsapp_rs::core::testing::RecordedRequest;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const WABA_2: &str = "104996122399160";
const PN_2: &str = "106540352242923";
const TOKEN: &str = "EAAG-merchant-token";
const TOKEN_2: &str = "EAAG-second-waba-token";
const OTHER_TENANT: &str = "merchant-43";
const OTHER_WABA: &str = "102290129340399";

/// A tenant with two WABAs (the second the migration's source), and
/// another tenant with one.
async fn connected() -> (Harness, String) {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.connect(TENANT, WABA_2, &[PN_2], TOKEN_2).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &["106540352242924"], "EAAG-other")
        .await;
    let key = h.tenant_key(TENANT, &[Scope::Templates]).await;
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

/// The lookup of a template id by its name: `GET /{id}?fields=name`, then
/// the WABA's templates of that name, holding `found` (the ids answered).
fn script_lookup(h: &Harness, id: &str, found: &[&str]) {
    h.graph
        .push_json(200, json!({"name": "spring_sale", "id": id}));
    let data: Vec<Value> = found
        .iter()
        .map(|id| json!({"name": "spring_sale", "id": id}))
        .collect();
    h.graph.push_json(200, json!({ "data": data }));
}

/// The lookup's two requests, made with `token` for the WABA `waba`.
fn assert_lookup(named: &RecordedRequest, listed: &RecordedRequest, id: &str, waba: &str) {
    assert_eq!(named.method, Method::GET);
    assert_eq!(named.path(), format!("/v25.0/{id}"));
    assert_eq!(query(named), pairs(&[("fields", "name")]));
    assert_eq!(named.bearer(), Some(TOKEN));
    assert_eq!(listed.method, Method::GET);
    assert_eq!(listed.path(), format!("/v25.0/{waba}/message_templates"));
    assert_eq!(
        query(listed),
        pairs(&[
            ("fields", "name"),
            ("limit", "100"),
            ("name", "spring_sale")
        ])
    );
    assert_eq!(listed.bearer(), Some(TOKEN));
}

fn graph_error(code: i64) -> Value {
    json!({"error": {"message": "Invalid parameter", "type": "OAuthException", "code": code,
                     "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}})
}

// ─── Edit ────────────────────────────────────────────────────────────────

/// templates/template-management, "Edit template components" (verbatim):
/// the template is looked for in the WABA first, then edited with the
/// WABA's token, the components as the library writes them (enums upper
/// case already in the page). `204`, and the WABA's cache dropped.
/// Refused before any request: a misspelled key, a name (not editable), a
/// zero-tap acceptance, an empty edit. Another WABA's template (not among
/// the WABA's of its name) is `404`, nothing edited; Meta refusing the id
/// is `404` without Meta's code. Decisive: the lookup before the edit
/// (`Templates::owned_template`), and `templates::edit`'s checks.
#[tokio::test]
#[allow(clippy::too_many_lines)] // Meta's example, then each refusal
async fn an_edit_reaches_only_the_wabas_own_template() {
    let (h, key) = connected().await;
    let id = "564750795574598";
    let example = json!({"components": [
        {"type": "HEADER", "format": "TEXT", "text": "Our {{1}} is on!",
         "example": {"header_text": ["Spring Sale"]}},
        {"type": "BODY",
         "text": "Shop now through {{1}} and use code {{2}} to get {{3}} off of all merchandise.",
         "example": {"body_text": [["the end of April", "25OFF", "25%"]]}},
        {"type": "FOOTER", "text": "Use the buttons below to manage your marketing subscriptions"},
        {"type": "BUTTONS", "buttons": [
            {"type": "QUICK_REPLY", "text": "Unsubscribe from Promos"},
            {"type": "QUICK_REPLY", "text": "Unsubscribe from All"}]}]});
    let edit = |id: &str, body: &Value| {
        Call::new(Method::PATCH, format!("/v1/wabas/{WABA}/templates/{id}"))
            .key(&key)
            .json(body)
    };
    script_lookup(&h, id, &["1", id]);
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(edit(id, &example)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let requests = h.graph.requests();
    let [named, listed, edited] = &requests[..] else {
        panic!("three requests: {requests:?}")
    };
    assert_lookup(named, listed, id, WABA);
    assert_eq!(edited.method, Method::POST);
    assert_eq!(edited.path(), format!("/v25.0/{id}"));
    assert_eq!(query(edited), pairs(&[]));
    assert_eq!(edited.bearer(), Some(TOKEN));
    assert_eq!(edited.json().unwrap(), example);
    // "Edit template category", verbatim.
    script_lookup(&h, "1252715608684590", &["1252715608684590"]);
    h.graph.push_json(200, json!({"success": true}));
    let reply = h
        .call(edit("1252715608684590", &json!({"category": "MARKETING"})))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    assert_eq!(
        h.graph.last_request().unwrap().json().unwrap(),
        json!({"category": "MARKETING"})
    );
    // Refused before any request, each on its path.
    let before = h.graph.requests().len();
    let mut misspelled = example.clone();
    misspelled["components"][3]["buttons"][0]["txt"] = json!("Stop");
    let mut zero_tap = json!({"components": [
        {"type": "BODY", "add_security_recommendation": true},
        {"type": "BUTTONS", "buttons": [{"type": "OTP", "otp_type": "ZERO_TAP", "text": "Copy Code",
            "autofill_text": "Autofill", "zero_tap_terms_accepted": true,
            "supported_apps": [{"package_name": "com.example.luckyshrub", "signature_hash": "K8a/AINcGX7"}]}]}],
        "category": "AUTHENTICATION"});
    for (body, field) in [
        (misspelled, "components[3].buttons[0].txt"),
        (json!({"name": "spring_sale_2"}), "name"),
        (json!({}), "edit"),
        (json!("an edit"), "body"),
        (
            zero_tap.clone(),
            "components[1].buttons[0].zero_tap_terms_accepted",
        ),
    ] {
        let reply = h.call(edit(id, &body)).await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{body}: {}",
            reply.text
        );
    }
    zero_tap["components"][1]["buttons"][0]["zero_tap_terms_accepted"] = json!(false);
    let reply = h.call(edit(id, &zero_tap)).await;
    assert_eq!(reply.code(), "invalid_request", "the library's own refusal");
    let reply = h.call(edit("abc", &example)).await;
    assert_eq!(reply.json()["error"]["field"], "id");
    assert_eq!(h.graph.requests().len(), before, "nothing reached Meta");
    // Another WABA's template: its name's templates in this WABA do not
    // hold it. Nothing is edited.
    script_lookup(&h, id, &["1"]);
    let reply = h.call(edit(id, &example)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    assert_eq!(h.graph.requests().len(), before + 2, "no edit");
    // Meta refusing the id: not found, no Meta code.
    h.graph.push_json(400, graph_error(100));
    let reply = h.call(edit(id, &example)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    assert!(reply.json()["error"]["graph"].is_null(), "{}", reply.text);
    // Meta's refusal of the edit itself keeps its code.
    script_lookup(&h, id, &[id]);
    h.graph.push_json(
        400,
        json!({"error": {"message": "x", "type": "OAuthException", "code": 2388040,
                         "error_data": {"details": "Meta's reason"}}}),
    );
    let reply = h.call(edit(id, &example)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "template_rejected")
    );
    assert_eq!(reply.json()["error"]["graph"]["details"], "Meta's reason");
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Unpause ─────────────────────────────────────────────────────────────

/// templates/template-pausing: `POST /{id}/unpause`, on the WABA's own
/// template only. Decisive: the lookup before the unpause.
#[tokio::test]
async fn an_unpause_reaches_only_the_wabas_own_template() {
    let (h, key) = connected().await;
    let id = "1407680676729941";
    let unpause = |id: &str| {
        Call::new(
            Method::POST,
            format!("/v1/wabas/{WABA}/templates/{id}/unpause"),
        )
        .key(&key)
    };
    script_lookup(&h, id, &[id]);
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(unpause(id)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let requests = h.graph.requests();
    let [named, listed, unpaused] = &requests[..] else {
        panic!("three requests: {requests:?}")
    };
    assert_lookup(named, listed, id, WABA);
    assert_eq!(unpaused.method, Method::POST);
    assert_eq!(unpaused.path(), format!("/v25.0/{id}/unpause"));
    assert_eq!(unpaused.bearer(), Some(TOKEN));
    script_lookup(&h, id, &[]);
    let reply = h.call(unpause(id)).await;
    assert_eq!(reply.code(), "not_found");
    assert_eq!(h.graph.requests().len(), 5, "no unpause");
    let reply = h.call(unpause("0123")).await;
    assert_eq!(reply.json()["error"]["field"], "id");
    assert_eq!(h.graph.requests().len(), 5);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Compare ─────────────────────────────────────────────────────────────

/// templates/template-comparison, "Example request" and "Example
/// response" (verbatim): both ids looked for in the WABA, then the
/// comparison as Meta writes it (`template_ids=[…]`), the answer under
/// Meta's names. The same id twice is `422`; the other template of
/// another WABA is `404`, and Meta is not asked to compare. Decisive: the
/// lookup of the second id.
#[tokio::test]
async fn a_comparison_names_two_of_the_wabas_templates() {
    let (h, key) = connected().await;
    let (id, other) = ("5289179717853347", "1533406637136032");
    let example = json!({"data": [
        {"metric": "BLOCK_RATE", "type": "RELATIVE",
         "order_by_relative_metric": ["1533406637136032", "5289179717853347"]},
        {"metric": "MESSAGE_SENDS", "type": "NUMBER_VALUES",
         "number_values": [{"key": "5289179717853347", "value": 1273},
                           {"key": "1533406637136032", "value": 1042}]},
        {"metric": "TOP_BLOCK_REASON", "type": "STRING_VALUES",
         "string_values": [{"key": "5289179717853347", "value": "UNKNOWN_BLOCK_REASON"},
                           {"key": "1533406637136032", "value": "UNKNOWN_BLOCK_REASON"}]}]});
    let compare = |other: &str| {
        Call::get(format!(
            "/v1/wabas/{WABA}/templates/{id}/compare?template_ids={other}&start=1674844791182&end=1674845395982"
        ))
        .key(&key)
    };
    script_lookup(&h, id, &[id]);
    script_lookup(&h, other, &[other]);
    h.graph.push_json(200, example.clone());
    let reply = h.call(compare(other)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let mut expected = example.clone();
    expected["data"][0]["number_values"] = Value::Null;
    expected["data"][0]["string_values"] = Value::Null;
    expected["data"][1]["order_by_relative_metric"] = Value::Null;
    expected["data"][1]["string_values"] = Value::Null;
    expected["data"][2]["order_by_relative_metric"] = Value::Null;
    expected["data"][2]["number_values"] = Value::Null;
    assert_eq!(reply.json(), expected);
    let requests = h.graph.requests();
    let [named, listed, other_named, other_listed, compared] = &requests[..] else {
        panic!("five requests: {requests:?}")
    };
    assert_lookup(named, listed, id, WABA);
    assert_lookup(other_named, other_listed, other, WABA);
    assert_eq!(compared.method, Method::GET);
    assert_eq!(compared.path(), format!("/v25.0/{id}/compare"));
    assert_eq!(
        query(compared),
        pairs(&[
            ("template_ids", "[1533406637136032]"),
            ("start", "1674844791182"),
            ("end", "1674845395982"),
        ])
    );
    assert_eq!(compared.bearer(), Some(TOKEN));
    for bad in [id, "x1"] {
        let reply = h.call(compare(bad)).await;
        assert_eq!(reply.json()["error"]["field"], "template_ids", "{bad}");
    }
    assert_eq!(h.graph.requests().len(), 5, "refused before any request");
    // The other template is not the WABA's: nothing compared.
    script_lookup(&h, id, &[id]);
    script_lookup(&h, other, &["1"]);
    let reply = h.call(compare(other)).await;
    assert_eq!(reply.code(), "not_found");
    assert_eq!(h.graph.requests().len(), 9, "no comparison");
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Archive and unarchive ───────────────────────────────────────────────

/// templates/template-archival, its request syntax and example answers:
/// every id is found in one walk of the WABA's templates (`fields=id`,
/// 100 a page; an unarchive walks its archived ones), across pages, then
/// archived with the WABA's token, the ids as JSON strings; Meta's partial
/// failure is answered as it is. An id the walk does not find is `404`,
/// nothing archived; more than 100 ids, or one not a Meta id, is `422`
/// before any request. Decisive: the walk (`Templates::all_in_waba`).
#[tokio::test]
#[allow(clippy::too_many_lines)] // both routes, then each refusal
async fn archive_and_unarchive_act_only_on_the_wabas_templates() {
    let (h, key) = connected().await;
    let (first, second) = ("1387372356726668", "1304694804498707");
    let call = |route: &str, ids: &Value| {
        Call::new(Method::POST, format!("/v1/wabas/{WABA}/templates/{route}"))
            .key(&key)
            .json(&json!({ "hsm_ids": ids }))
    };
    for (route, status, answer, answered) in [
        (
            "archive",
            None,
            json!({"archived_templates": [first],
                   "failed_templates": {second: "Only approved or rejected templates can be archived"}}),
            "archived_templates",
        ),
        (
            "unarchive",
            Some("ARCHIVED"),
            json!({"unarchived_templates": [first],
                   "failed_templates": {second: "Template is not archived"}}),
            "unarchived_templates",
        ),
    ] {
        let before = h.graph.requests().len();
        h.graph.push_json(
            200,
            json!({"data": [{"id": "1"}, {"id": first}],
                   "paging": {"cursors": {"after": "QVFIUafter"}, "next": "https://graph.facebook.com/v25.0/x"}}),
        );
        h.graph.push_json(200, json!({"data": [{"id": second}]}));
        h.graph.push_json(200, answer.clone());
        let reply = h.call(call(route, &json!([first, second]))).await;
        assert_eq!(reply.status, StatusCode::OK, "{route}: {}", reply.text);
        assert_eq!(reply.json(), answer, "{route}");
        assert_eq!(reply.json()[answered], json!([first]));
        let requests = h.graph.requests();
        let [page_1, page_2, done] = &requests[before..] else {
            panic!("three requests: {requests:?}")
        };
        let mut walk = pairs(&[("fields", "id"), ("limit", "100")]);
        if let Some(status) = status {
            walk.push(("status".to_owned(), status.to_owned()));
        }
        for page in [page_1, page_2] {
            assert_eq!(page.method, Method::GET, "{route}");
            assert_eq!(page.path(), format!("/v25.0/{WABA}/message_templates"));
            assert_eq!(page.bearer(), Some(TOKEN));
        }
        assert_eq!(query(page_1), walk, "{route}");
        walk.push(("after".to_owned(), "QVFIUafter".to_owned()));
        assert_eq!(query(page_2), walk, "{route}");
        assert_eq!(done.method, Method::POST);
        assert_eq!(
            done.path(),
            format!("/v25.0/{WABA}/message_templates/{route}")
        );
        assert_eq!(done.bearer(), Some(TOKEN));
        assert_eq!(done.json().unwrap(), json!({"hsm_ids": [first, second]}));
        // An id of another WABA: the walk ends without it, nothing done.
        h.graph.push_json(200, json!({"data": [{"id": first}]}));
        let reply = h
            .call(call(route, &json!([first, "1627019861106475"])))
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{route}"
        );
        assert_eq!(
            h.graph.requests().len(),
            before + 4,
            "{route}: nothing done"
        );
        // Refused before any request.
        let many: Vec<String> = (0..101).map(|i| format!("1{i:015}")).collect();
        for (ids, field) in [
            (json!([]), "hsm_ids"),
            (json!(many), "hsm_ids"),
            (json!([first, "12a"]), "hsm_ids[1]"),
            (json!([first, "0123"]), "hsm_ids[1]"),
        ] {
            let reply = h.call(call(route, &ids)).await;
            assert_eq!(
                reply.json()["error"]["field"],
                field,
                "{route} {ids}: {}",
                reply.text
            );
        }
        let reply = h
            .call(
                Call::new(Method::POST, format!("/v1/wabas/{WABA}/templates/{route}"))
                    .key(&key)
                    .json(&json!({"template_ids": [first]})),
            )
            .await;
        assert_eq!(reply.json()["error"]["field"], "body");
        assert_eq!(h.graph.requests().len(), before + 4, "{route}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// The walk is bounded: a WABA past its 10 pages of 100 answers `404` for
/// an id beyond them, and Meta is asked 10 times, never more (design
/// D116). Decisive: `WALK_PAGES`.
#[tokio::test]
async fn the_walk_stops_after_its_pages() {
    let (h, key) = connected().await;
    for _ in 0..meta_whatsapp_server_core::templates::WALK_PAGES {
        h.graph.push_json(
            200,
            json!({"data": [{"id": "1"}],
                   "paging": {"cursors": {"after": "QVFIUafter"}, "next": "https://graph.facebook.com/v25.0/x"}}),
        );
    }
    let reply = h
        .call(
            Call::new(Method::POST, format!("/v1/wabas/{WABA}/templates/archive"))
                .key(&key)
                .json(&json!({"hsm_ids": ["1387372356726668"]})),
        )
        .await;
    assert_eq!(reply.code(), "not_found", "{}", reply.text);
    assert_eq!(h.graph.requests().len(), 10);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── The Template Library ────────────────────────────────────────────────

/// templates/template-library: its filters reach Meta as its parameters
/// (Meta's enums upper-cased), with the WABA's token, and its example
/// entry (with the `FLOW` button of its forms example) is answered under
/// Meta's names. A creation from the library is Meta's example body, the
/// button inputs an array of objects, posted as given; the page's
/// single-quoted string is refused (`invalid_request`, nothing sent), as
/// is a zero-tap acceptance in a button input. Decisive:
/// `templates::library_request`'s checks.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the browse, the creation, the refusals
async fn the_library_is_browsed_and_created_from() {
    let (h, key) = connected().await;
    let entry = json!({
      "name": "low_balance_warning_1",
      "language": "en_US",
      "category": "UTILITY",
      "topic": "PAYMENTS",
      "usecase": "LOW_BALANCE_WARNING",
      "industry": ["FINANCIAL_SERVICES"],
      "header": "Your account balance is low",
      "body": "Hi {{1}},\nThis is to notify you that your {{2}} in your {{3}} account, ending in {{4}} is below your pre-set {{5}} of {{6}}.\nClick the button to deposit more {{7}}.\n{{8}}",
      "body_params": ["Jim", "available funds", "CS Mutual checking plus", "1234", "limit", "$75.00", "funds", "CS Mutual"],
      "buttons": [
        {"type": "URL", "text": "Make a deposit", "url": "https://www.example.com/"},
        {"type": "PHONE_NUMBER", "text": "Call us", "phone_number": "+18005551234"}
      ],
      "id": "7147013345418927"
    });
    let form = json!({"name": "delivery_failed_2_form", "language": "en_US", "category": "UTILITY",
        "topic": "ORDER_MANAGEMENT", "usecase": "DELIVERY_FAILED", "industry": ["E_COMMERCE"],
        "body": "We were unable to deliver order {{1}} today.\n\nPlease {{2}} to schedule another delivery attempt.",
        "body_params": ["#12345", "try a redelivery"], "body_param_types": ["TEXT", "TEXT"],
        "buttons": [{"type": "FLOW", "text": "Reschedule"}], "id": "7138055039625658"});
    h.graph
        .push_json(200, json!({"data": [entry.clone(), form.clone()]}));
    let reply = h
        .call(
            Call::get(format!(
                "/v1/wabas/{WABA}/templates/library?search=payments&topic=payments&usecase=low_balance_warning&industry=FINANCIAL_SERVICES&language=en_US&name=low_balance_warning_1"
            ))
            .key(&key),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let mut answered = entry.clone();
    answered["body_param_types"] = json!([]);
    let mut answered_form = form.clone();
    answered_form["header"] = Value::Null;
    assert_eq!(reply.json(), json!({"data": [answered, answered_form]}));
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), "/v25.0/message_template_library");
    assert_eq!(
        query(&request),
        pairs(&[
            ("search", "payments"),
            ("topic", "PAYMENTS"),
            ("usecase", "LOW_BALANCE_WARNING"),
            ("industry", "FINANCIAL_SERVICES"),
            ("language", "en_US"),
            ("name", "low_balance_warning_1"),
        ])
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    for bad in [
        "?topic=a%26b",
        "?search=%20",
        "?language=en-US",
        "?name=Low%20Balance",
    ] {
        let reply = h
            .call(Call::get(format!("/v1/wabas/{WABA}/templates/library{bad}")).key(&key))
            .await;
        assert_eq!(reply.code(), "invalid_request", "{bad}");
    }
    assert_eq!(h.graph.requests().len(), 1);
    // The creation: Meta's example body, as given.
    let create = |body: &Value| {
        Call::new(Method::POST, format!("/v1/wabas/{WABA}/templates/library"))
            .key(&key)
            .json(body)
    };
    h.graph.push_json(
        200,
        json!({"id": "1627019861106480", "status": "APPROVED", "category": "UTILITY"}),
    );
    let example = common::sample_library_template();
    let reply = h.call(create(&example)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"id": "1627019861106480", "status": "APPROVED", "category": "UTILITY"})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(request.json().unwrap(), example);
    // Refused before any request.
    let before = h.graph.requests().len();
    let mut as_string = example.clone();
    as_string["library_template_button_inputs"] = json!(
        "[{'type': 'URL', 'url': {'base_url' : 'https://www.example.com/{{1}}', 'url_suffix_example' : 'https://www.example.com/order_update'}}]"
    );
    let mut misspelled = example.clone();
    misspelled["library_template_button_inputs"][0]["url"]["base_ur"] = json!("x");
    let mut zero_tap = example.clone();
    zero_tap["category"] = json!("AUTHENTICATION");
    zero_tap["library_template_button_inputs"] = json!([{"type": "OTP", "otp_type": "ZERO_TAP",
        "zero_tap_terms_accepted": true,
        "supported_apps": [{"package_name": "com.example.luckyshrub", "signature_hash": "K8a/AINcGX7"}]}]);
    let mut blank = example.clone();
    blank["library_template_name"] = json!(" ");
    let mut bad_name = example.clone();
    bad_name["name"] = json!("My Delivery Update");
    for (body, field) in [
        (as_string, "body"),
        (misspelled, "library_template_button_inputs[0].url.base_ur"),
        (
            zero_tap,
            "library_template_button_inputs[0].zero_tap_terms_accepted",
        ),
        (blank, "library_template_name"),
        (bad_name, "name"),
    ] {
        let reply = h.call(create(&body)).await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{body}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before, "nothing reached Meta");
    // An `Idempotency-Key` never creates twice.
    let keyed = || create(&example).header("idempotency-key", "library:1");
    h.graph.push_json(
        200,
        json!({"id": "1627019861106481", "status": "APPROVED", "category": "UTILITY"}),
    );
    let first = h.call(keyed()).await;
    let again = h.call(keyed()).await;
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, first.text);
    assert_eq!(h.graph.requests().len(), before + 1);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Migrate ─────────────────────────────────────────────────────────────

/// templates/template-migration, its example (into `104996122399160`
/// from `102290129340398`, here the tenant's two WABAs) with template ids:
/// the ids are found in the source's templates with the **source's**
/// token, then the migration is posted to the destination with **its**
/// token, Meta's parameters as a JSON body; Meta's example answer is
/// answered under its names. A source of another tenant is `404` with
/// nothing read from the vault for it and nothing asked of Meta; an id not
/// the source's is `404`, nothing migrated. Decisive: the source opened as
/// the caller's (design D117), and the walk with its token.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the migration, then each refusal
async fn a_migration_comes_only_from_another_of_the_tenants_wabas() {
    let (h, key) = connected().await;
    let migrate = |body: &Value| {
        Call::new(
            Method::POST,
            format!("/v1/wabas/{WABA_2}/templates/migrations"),
        )
        .key(&key)
        .json(body)
    };
    let answer = json!({
      "migrated_templates": ["1473688840035974", "6162904357082268", "6147830171896170"],
      "failed_templates": {
        "1019496902803242": "Incorrect category",
        "259672276895259": "Formatting error - dangling parameter",
        "572279198452421": "Incorrect category"
      }
    });
    let body = json!({"source_waba_id": WABA, "page_number": 0, "count": 200,
                      "template_ids": ["1473688840035974", "1019496902803242"]});
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1473688840035974"}, {"id": "1019496902803242"}]}),
    );
    h.graph.push_json(200, answer.clone());
    let reply = h.call(migrate(&body)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json(), answer);
    let requests = h.graph.requests();
    let [walk, migrated] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_eq!(walk.method, Method::GET);
    assert_eq!(walk.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(query(walk), pairs(&[("fields", "id"), ("limit", "100")]));
    assert_eq!(walk.bearer(), Some(TOKEN), "the source's token");
    assert_eq!(migrated.method, Method::POST);
    assert_eq!(
        migrated.path(),
        format!("/v25.0/{WABA_2}/migrate_message_templates")
    );
    assert_eq!(query(migrated), pairs(&[]));
    assert_eq!(migrated.bearer(), Some(TOKEN_2), "the destination's token");
    assert_eq!(migrated.json().unwrap(), body);
    // Without ids: no walk.
    h.graph.push_json(200, answer.clone());
    let reply = h
        .call(migrate(&json!({"source_waba_id": WABA, "page_number": 1})))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(h.graph.requests().len(), 3);
    assert_eq!(
        h.graph.last_request().unwrap().json().unwrap(),
        json!({"source_waba_id": WABA, "page_number": 1})
    );
    // Another tenant's WABA as the source: not found, its token never read.
    let reads = h.kv.vault_reads();
    let reply = h
        .call(migrate(&json!({"source_waba_id": OTHER_WABA})))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        reply.text
    );
    assert_eq!(
        h.kv.vault_reads(),
        reads + 1,
        "the destination's token only"
    );
    assert_eq!(h.graph.requests().len(), 3, "nothing asked of Meta");
    // An id not the source's: nothing migrated.
    h.graph
        .push_json(200, json!({"data": [{"id": "1473688840035974"}]}));
    let reply = h.call(migrate(&body)).await;
    assert_eq!(reply.code(), "not_found");
    assert_eq!(h.graph.requests().len(), 4, "no migration");
    // Refused before any request.
    let many: Vec<String> = (0..501).map(|i| format!("1{i:015}")).collect();
    for (body, field) in [
        (json!({"source_waba_id": "WABA"}), "source_waba_id"),
        (json!({"source_waba_id": WABA, "count": 0}), "count"),
        (json!({"source_waba_id": WABA, "count": 501}), "count"),
        (
            json!({"source_waba_id": WABA, "template_ids": many}),
            "template_ids",
        ),
        (
            json!({"source_waba_id": WABA, "template_ids": ["1", "x"]}),
            "template_ids[1]",
        ),
        (json!({"source_waba_id": WABA, "page": 0}), "body"),
    ] {
        let reply = h.call(migrate(&body)).await;
        assert_eq!(
            reply.json()["error"]["field"],
            field,
            "{body}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), 4);
    // An `Idempotency-Key` never migrates twice.
    let keyed =
        || migrate(&json!({"source_waba_id": WABA})).header("idempotency-key", "migration:1");
    h.graph.push_json(200, answer);
    let first = h.call(keyed()).await;
    let again = h.call(keyed()).await;
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, first.text);
    assert_eq!(h.graph.requests().len(), 5);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Template groups ─────────────────────────────────────────────────────

/// templates/template-groups, "Get a template group", "Example response"
/// (one template of its truncated list), for the WABA `waba`.
fn group_example(waba: &str) -> Value {
    json!({
      "name": "Black Friday 2024",
      "description": "US-based Black Friday sale 2024 templates.",
      "creation_time": "2025-01-06T23:05:12+0000",
      "update_time": "2025-01-06T23:05:12+0000",
      "whatsapp_business_account": {
        "id": waba, "name": "Lucky Shrub", "currency": "USD", "timezone_id": "1",
        "message_template_namespace": "ba30dd89_2ebd_41e4_b805_f2c05ae04cc9"
      },
      "whatsapp_business_templates": {
        "data": [{
          "name": "black_friday_2024_carousel_media_header_cards",
          "parameter_format": "POSITIONAL",
          "components": [{"type": "BODY", "text": "Rare black succulents for sale! {{1}}, add...",
                          "example": {"body_text": [["Pablo"]]}}],
          "language": "en_US", "status": "APPROVED", "category": "MARKETING",
          "id": "1945418102598215"
        }],
        "paging": {"cursors": {"before": "QVFIU...", "after": "QVFIU..."}}
      },
      "id": "9020555671393375"
    })
}

/// The page's examples, each route: a creation walks the WABA's templates
/// for its ids then posts Meta's body (ids as JSON numbers); a read, an
/// update and a deletion read the group first, and act only when its
/// `whatsapp_business_account` is the path's WABA. A group of another
/// WABA is `404` with nothing changed or deleted; Meta refusing the id is
/// `404` without its code. Decisive: `Templates::owned_group`'s WABA
/// comparison.
#[tokio::test]
#[allow(clippy::too_many_lines)] // four routes, then each refusal
async fn template_groups_are_the_wabas_own() {
    let (h, key) = connected().await;
    let group = "9020555671393375";
    let at = |method: Method, path: &str| Call::new(method, path).key(&key);
    let groups = format!("/v1/wabas/{WABA}/template-groups");
    let one = format!("{groups}/{group}");
    // Create: the page's example request.
    let create = json!({"name": "Black Friday 2024",
        "description": "US-based Black Friday sale 2024 templates.",
        "whatsapp_business_templates": [278_077_987_957_091_u64, 1_945_418_102_598_215_u64, 1_035_843_174_854_974_u64]});
    h.graph.push_json(
        200,
        json!({"data": [{"id": "278077987957091"}, {"id": "1945418102598215"}, {"id": "1035843174854974"}]}),
    );
    h.graph.push_json(200, json!({"id": group}));
    let reply = h.call(at(Method::POST, &groups).json(&create)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(reply.json(), json!({"id": group}));
    let requests = h.graph.requests();
    let [walk, created] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_eq!(walk.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(query(walk), pairs(&[("fields", "id"), ("limit", "100")]));
    assert_eq!(created.method, Method::POST);
    assert_eq!(created.path(), format!("/v25.0/{WABA}/template_groups"));
    assert_eq!(created.bearer(), Some(TOKEN));
    assert_eq!(created.json().unwrap(), create);
    // The ids as strings reach Meta as its integers.
    let mut as_strings = create.clone();
    as_strings["whatsapp_business_templates"] =
        json!(["278077987957091", "1945418102598215", "1035843174854974"]);
    h.graph.push_json(
        200,
        json!({"data": [{"id": "278077987957091"}, {"id": "1945418102598215"}, {"id": "1035843174854974"}]}),
    );
    h.graph.push_json(200, json!({"id": group}));
    let reply = h.call(at(Method::POST, &groups).json(&as_strings)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(h.graph.last_request().unwrap().json().unwrap(), create);
    // Read: the page's example answer.
    h.graph.push_json(200, group_example(WABA));
    let reply = h.call(at(Method::GET, &one)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"id": group, "name": "Black Friday 2024",
               "description": "US-based Black Friday sale 2024 templates.",
               "creation_time": "2025-01-06T23:05:12+0000",
               "update_time": "2025-01-06T23:05:12+0000",
               "templates": [{"id": "1945418102598215",
                   "name": "black_friday_2024_carousel_media_header_cards",
                   "language": "en_US", "status": "APPROVED", "category": "MARKETING",
                   "components": group_example(WABA)["whatsapp_business_templates"]["data"][0]["components"]}]})
    );
    let read = h.graph.last_request().unwrap();
    assert_eq!(read.method, Method::GET);
    assert_eq!(read.path(), format!("/v25.0/{group}"));
    assert_eq!(query(&read), pairs(&[]));
    assert_eq!(read.bearer(), Some(TOKEN));
    // Update: the page's example request.
    let update = json!({"add_templates": [844_006_344_365_270_u64, 1_188_657_612_184_382_u64],
                        "remove_templates": [1_035_843_174_854_974_u64]});
    h.graph.push_json(200, group_example(WABA));
    h.graph.push_json(
        200,
        json!({"data": [{"id": "844006344365270"}, {"id": "1188657612184382"}]}),
    );
    h.graph.push_json(200, json!({"success": true}));
    let before = h.graph.requests().len();
    let reply = h.call(at(Method::PATCH, &one).json(&update)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let requests = h.graph.requests();
    let [read, walk, updated] = &requests[before..] else {
        panic!("three requests: {requests:?}")
    };
    assert_eq!(read.path(), format!("/v25.0/{group}"));
    assert_eq!(walk.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(updated.method, Method::POST);
    assert_eq!(updated.path(), format!("/v25.0/{group}"));
    assert_eq!(updated.bearer(), Some(TOKEN));
    assert_eq!(updated.json().unwrap(), update);
    // Delete.
    h.graph.push_json(200, group_example(WABA));
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(at(Method::DELETE, &one)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let deleted = h.graph.last_request().unwrap();
    assert_eq!(deleted.method, Method::DELETE);
    assert_eq!(deleted.path(), format!("/v25.0/{group}"));
    assert_eq!(deleted.bearer(), Some(TOKEN));
    // Another WABA's group, or a node naming no WABA: read, never acted on.
    let before = h.graph.requests().len();
    for other in [
        group_example(OTHER_WABA),
        group_example(WABA_2),
        json!({"id": group, "name": "Lucky Shrub"}),
    ] {
        for (method, body) in [
            (Method::GET, None),
            (Method::PATCH, Some(json!({"name": "Mine now"}))),
            (Method::DELETE, None),
        ] {
            h.graph.push_json(200, other.clone());
            let mut call = at(method.clone(), &one);
            if let Some(body) = &body {
                call = call.json(body);
            }
            let reply = h.call(call).await;
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{method} {other}"
            );
        }
    }
    assert_eq!(
        h.graph.requests().len(),
        before + 9,
        "only the reads reached Meta"
    );
    // Meta refusing the id.
    h.graph.push_json(400, graph_error(100));
    let reply = h.call(at(Method::DELETE, &one)).await;
    assert_eq!(reply.code(), "not_found");
    assert!(reply.json()["error"]["graph"].is_null());
    // A template of another WABA: nothing created, nothing added.
    h.graph
        .push_json(200, json!({"data": [{"id": "278077987957091"}]}));
    let reply = h.call(at(Method::POST, &groups).json(&create)).await;
    assert_eq!(reply.code(), "not_found");
    h.graph.push_json(200, group_example(WABA));
    h.graph.push_json(200, json!({"data": []}));
    let reply = h.call(at(Method::PATCH, &one).json(&update)).await;
    assert_eq!(reply.code(), "not_found");
    assert_eq!(h.graph.requests().len(), before + 13, "nothing acted");
    // Refused before any request.
    for (method, path, body, field) in [
        (Method::POST, &groups, json!({"name": "x"}), "body"),
        (
            Method::POST,
            &groups,
            json!({"name": "x", "whatsapp_business_templates": ["1", "1a"]}),
            "whatsapp_business_templates[1]",
        ),
        (
            Method::POST,
            &groups,
            json!({"name": "x".repeat(257), "whatsapp_business_templates": ["1"]}),
            "name",
        ),
        (
            Method::POST,
            &groups,
            json!({"name": "x", "whatsapp_business_templates": ["1"], "descripton": "y"}),
            "descripton",
        ),
        (Method::PATCH, &one, json!({}), "update"),
        (
            Method::PATCH,
            &one,
            json!({"add_templates": ["x"]}),
            "add_templates[0]",
        ),
        (
            Method::PATCH,
            &format!("{groups}/90205a"),
            json!({"name": "y"}),
            "group_id",
        ),
    ] {
        let reply = h.call(at(method.clone(), path).json(&body)).await;
        assert_eq!(
            reply.json()["error"]["field"],
            field,
            "{method} {path} {body}: {}",
            reply.text
        );
    }
    for method in [Method::GET, Method::DELETE] {
        let reply = h.call(at(method, &format!("{groups}/..%2Fx"))).await;
        assert_eq!(reply.json()["error"]["field"], "group_id");
    }
    assert_eq!(h.graph.requests().len(), before + 13);
    // A creation's `Idempotency-Key` never creates twice.
    let keyed = || {
        at(Method::POST, &groups)
            .json(&create)
            .header("idempotency-key", "group:1")
    };
    h.graph.push_json(
        200,
        json!({"data": [{"id": "278077987957091"}, {"id": "1945418102598215"}, {"id": "1035843174854974"}]}),
    );
    h.graph.push_json(200, json!({"id": group}));
    let first = h.call(keyed()).await;
    let again = h.call(keyed()).await;
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, first.text);
    assert_eq!(h.graph.requests().len(), before + 15);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Parity row 99: each kind, created ───────────────────────────────────

/// Definition enums appear in either case on Meta's pages (Meta accepts
/// both, and the library writes upper case): compared upper-cased, as the
/// library's tests compare them.
fn normalize(mut v: Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(map) => {
                for (k, val) in map.iter_mut() {
                    match (k.as_str(), &mut *val) {
                        (
                            "type" | "format" | "category" | "parameter_format" | "otp_type",
                            Value::String(s),
                        ) => *s = s.to_uppercase(),
                        _ => walk(val),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut v);
    v
}

/// The creation examples of parity row 99's pages, verbatim: coupon,
/// limited-time offer, media-card carousel, product-card carousel (a
/// product header), location, call permission request, catalog, MPM, SPM
/// (a product header).
#[allow(clippy::too_many_lines)] // nine of Meta's examples, verbatim
fn row_99_examples() -> Vec<(&'static str, Value)> {
    let media_card = |example: &str| {
        json!({"components": [
            {"type": "header", "format": "image", "example": {"header_handle": ["4::an..."]}},
            {"type": "buttons", "buttons": [
                {"type": "quick_reply", "text": "Send me more like this!"},
                {"type": "url", "text": "Shop", "url": "https://www.luckyshrub.com/rare-succulents/{{1}}",
                 "example": [example]}]}]})
    };
    let product_card = json!({"components": [
        {"type": "header", "format": "product"},
        {"type": "buttons", "buttons": [{"type": "spm", "text": "View"}]}]});
    vec![
        (
            "templates/marketing-templates/coupon-templates",
            json!({"name": "winter_sale_coupon", "language": "en_US", "category": "MARKETING",
                "parameter_format": "named", "components": [
                {"type": "HEADER", "format": "TEXT", "text": "Our Winter Sale is on!"},
                {"type": "BODY", "text": "Shop now through the end of December and use the one-time use code {{coupon_code}} to get {{discount}} off of your entire order!",
                 "example": {"body_text_named_params": [
                     {"param_name": "coupon_code", "example": "WINTER25"},
                     {"param_name": "discount", "example": "30%"}]}},
                {"type": "BUTTONS", "buttons": [
                    {"type": "QUICK_REPLY", "text": "Unsubscribe"},
                    {"type": "COPY_CODE", "example": "WINTER25"}]}]}),
        ),
        (
            "templates/marketing-templates/limited-time-offer-templates",
            json!({"name": "limited_time_offer_caribbean_pkg_2023", "language": "en_US",
                "category": "marketing", "components": [
                {"type": "header", "format": "image", "example": {"header_handle": ["4::aW..."]}},
                {"type": "limited_time_offer", "limited_time_offer": {"text": "Expiring offer!", "has_expiration": true}},
                {"type": "body", "text": "Good news, {{1}}! Use code {{2}} to get 25% off all Caribbean Destination packages!",
                 "example": {"body_text": [["Pablo", "CARIBE25"]]}},
                {"type": "buttons", "buttons": [
                    {"type": "copy_code", "example": "CARIBE25"},
                    {"type": "url", "text": "Book now!", "url": "https://awesomedestinations.com/offers?code={{1}}",
                     "example": ["https://awesomedestinations.com/offers?ref=n3mtql"]}]}]}),
        ),
        (
            "templates/marketing-templates/media-card-carousel-templates",
            json!({"name": "carousel_template_media_cards_v1", "language": "en_US",
                "category": "marketing", "components": [
                {"type": "body", "text": "Rare succulents for sale! {{1}}, add these unique plants to your collection. Each of these rare succulents are {{2}} if you checkout using code {{3}}. Shop now and add some unique and beautiful plants to your collection!",
                 "example": {"body_text": [["Pablo", "30%", "30OFF"]]}},
                {"type": "carousel", "cards": [
                    media_card("BLUE_ELF"), media_card("BUDDHA"), media_card("BLACK_PRINCE")]}]}),
        ),
        (
            "catalogs/product-card-carousel-template-messages",
            json!({"name": "carousel_template_product_cards_v1", "language": "en_US",
                "category": "marketing", "components": [
                {"type": "body", "text": "Rare succulents for sale! {{1}}, add these unique plants to your collection. All three of these rare succulents are available for purchase on our website, and they come with a 100% satisfaction guarantee. Whether you're a seasoned succulent enthusiast or just starting your plant collection, these rare succulents are sure to impress. Shop now and add some unique and beautiful plants to your collection!",
                 "example": {"body_text": "Pablo"}},
                {"type": "carousel", "cards": [product_card.clone(), product_card]}]}),
        ),
        (
            "templates/utility-templates/location-templates",
            json!({"name": "order_delivery_update", "language": "en_US", "category": "UTILITY",
                "parameter_format": "named", "components": [
                {"type": "HEADER", "format": "LOCATION"},
                {"type": "BODY", "text": "Good news {{customer_name}}! Your order #{{order_number}} is on its way to the location above. Thank you for your order!",
                 "example": {"body_text_named_params": [
                     {"param_name": "customer_name", "example": "Mark"},
                     {"param_name": "order_number", "example": "566701"}]}},
                {"type": "FOOTER", "text": "To stop receiving delivery updates, tap the button below."},
                {"type": "BUTTONS", "buttons": [{"type": "QUICK_REPLY", "text": "Stop Delivery Updates"}]}]}),
        ),
        (
            "templates/marketing-templates/call-permission-request-message-template",
            json!({"name": "vip_early_access_call", "language": "en_US", "category": "MARKETING",
                "parameter_format": "named", "components": [
                {"type": "body", "text": "Hi {{first_name}}, as a Lucky Shrub VIP, get a first look at our rare new succulents before anyone else. Can we give you a quick call?",
                 "example": {"body_text_named_params": [{"param_name": "first_name", "example": "Pablo"}]}},
                {"type": "call_permission_request"}]}),
        ),
        (
            "catalogs/catalog-template-messages",
            json!({"name": "intro_catalog_offer", "language": "en_US", "category": "MARKETING",
                "components": [
                {"type": "BODY", "text": "Now shop for your favorite products right here on WhatsApp! Get Rs {{1}} off on all orders above {{2}}Rs! Valid for your first {{3}} orders placed on WhatsApp!",
                 "example": {"body_text": [["100", "400", "3"]]}},
                {"type": "FOOTER", "text": "Best grocery deals on WhatsApp!"},
                {"type": "BUTTONS", "buttons": [{"type": "CATALOG", "text": "View catalog"}]}]}),
        ),
        (
            "catalogs/mpm-template-messages",
            json!({"name": "abandoned_cart", "language": "en_US", "category": "MARKETING",
                "components": [
                {"type": "HEADER", "format": "TEXT", "text": "Forget something, {{1}}?",
                 "example": {"header_text": ["Pablo"]}},
                {"type": "BODY", "text": "Looks like you left these items in your cart, still interested? Use code {{1}} to get 10% off!",
                 "example": {"body_text": [["10OFF"]]}},
                {"type": "BUTTONS", "buttons": [{"type": "MPM", "text": "View items"}]}]}),
        ),
        (
            "catalogs/spm-template-messages",
            json!({"name": "spm_template_named_params", "language": "en_US", "category": "marketing",
                "parameter_format": "named", "components": [
                {"type": "header", "format": "product"},
                {"type": "body", "text": "Use code {{code}} to get {{percent}} off our newest succulent!",
                 "example": {"body_text_named_params": [
                     {"param_name": "code", "example": "15OFF"},
                     {"param_name": "percent", "example": "15%"}]}},
                {"type": "footer", "text": "Offer ends September 22, 2024"},
                {"type": "buttons", "buttons": [{"type": "spm", "text": "View"}]}]}),
        ),
    ]
}

/// Parity row 99, the roadmap's decisive test for M5b: each kind's
/// creation example, verbatim, is created through the service with no key
/// dropped: Meta receives the example itself, but for the library's
/// spelling (enums upper-cased; a single `body_text` example written as
/// the reference's `[["…"]]`). Then, in each, a key the library does not
/// carry, at the deepest object of its last component, is refused on its
/// path with nothing sent (roadmap M5a's comparison, P2's
/// `request::dropped_key`). Decisive: the comparison in
/// `templates::definition`, and each kind's fields in the library.
#[tokio::test]
async fn each_kind_of_row_99_is_created_from_metas_example_with_no_key_dropped() {
    let (h, key) = connected().await;
    let create = |body: &Value| {
        Call::new(Method::POST, format!("/v1/wabas/{WABA}/templates"))
            .key(&key)
            .json(body)
    };
    let examples = row_99_examples();
    assert_eq!(examples.len(), 9);
    for (page, example) in &examples {
        h.graph.push_json(
            200,
            json!({"id": "1627019861106475", "status": "PENDING", "category": "MARKETING"}),
        );
        let reply = h.call(create(example)).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{page}: {}", reply.text);
        let request = h.graph.last_request().unwrap();
        assert_eq!(request.method, Method::POST, "{page}");
        assert_eq!(request.path(), format!("/v25.0/{WABA}/message_templates"));
        assert_eq!(request.bearer(), Some(TOKEN), "{page}");
        let mut expected = normalize(example.clone());
        if let Some(body_text) = expected
            .pointer_mut("/components/0/example/body_text")
            .filter(|v| v.is_string())
        {
            *body_text = json!([[body_text.clone()]]);
        }
        assert_eq!(normalize(request.json().unwrap()), expected, "{page}");
    }
    // A key the library would drop, in each: refused on its path.
    let before = h.graph.requests().len();
    for (page, example) in &examples {
        let mut extra = example.clone();
        let components = extra["components"].as_array_mut().unwrap();
        let last = components.len() - 1;
        let target = &mut components[last];
        let (path, object) = match target.get_mut("buttons").and_then(|b| b.get_mut(0)) {
            Some(button) => (format!("components[{last}].buttons[0].not_a_key"), button),
            None => match target.get_mut("cards").and_then(|c| c.get_mut(0)) {
                Some(card) => (format!("components[{last}].cards[0].not_a_key"), card),
                None => (format!("components[{last}].not_a_key"), target),
            },
        };
        object["not_a_key"] = json!("kept?");
        let reply = h.call(create(&extra)).await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(path.as_str())),
            "{page}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before, "nothing reached Meta");
    assert_eq!(h.graph.remaining(), 0);
}

/// Parity row 100. templates/time-to-live's example (verbatim) sets 120
/// seconds on a `MARKETING` template, which the page's own table forbids
/// (12 hours to 30 days): refused on `message_send_ttl_seconds`, nothing
/// sent; at 43,200 seconds it is created and the key reaches Meta.
/// templates/tap-target-url-title-override's send example: its
/// `tap_target_configuration` component reaches Meta as the page writes
/// it. Decisive: the library's TTL range, and its `TapTarget` component.
#[tokio::test]
async fn a_ttl_and_a_tap_target_reach_meta() {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Templates, Scope::Send]).await;
    let mut ttl = json!({"name": "test_template", "language": "en_US", "category": "MARKETING",
        "message_send_ttl_seconds": 120, "components": [
        {"type": "BODY", "text": "Shop now through {{1}} and use code {{2}} to get {{3}} off of all merchandise.",
         "example": {"body_text": [["the end of August", "25OFF", "25%"]]}},
        {"type": "FOOTER", "text": "Use the buttons below to manage your marketing subscriptions"}]});
    let create = |body: &Value| {
        Call::new(Method::POST, format!("/v1/wabas/{WABA}/templates"))
            .key(&key)
            .json(body)
    };
    let reply = h.call(create(&ttl)).await;
    assert_eq!(
        reply.json()["error"]["field"],
        "message_send_ttl_seconds",
        "{}",
        reply.text
    );
    assert!(h.graph.requests().is_empty());
    ttl["message_send_ttl_seconds"] = json!(43_200);
    h.graph.push_json(
        200,
        json!({"id": "1627019861106475", "status": "PENDING", "category": "MARKETING"}),
    );
    let reply = h.call(create(&ttl)).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(h.graph.last_request().unwrap().json().unwrap(), ttl);
    // The tap target, sent.
    let template = json!({"name": "august_promotion", "language": {"code": "en"}, "components": [
        {"type": "header", "parameters": [{"type": "image", "image": {"link": "https://www.luckyshrubs.com"}}]},
        {"type": "body", "parameters": [{"type": "text", "text": "Hello Andy..."}]},
        {"type": "tap_target_configuration", "parameters": [{"type": "tap_target_configuration",
            "tap_target_configuration": [{"url": "https://www.luckyshrubs.com", "title": "Offer Details!"}]}]}]});
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp",
               "contacts": [{"input": "+16505551234", "wa_id": "16505551234"}],
               "messages": [{"id": "wamid.HBgLMTY1MDUwNzY1MjAVAgARGBI5QTNDQTVCM0Q0Q0Q2RTY3RTcA"}]}),
    );
    let reply = h
        .call(
            Call::new(Method::POST, format!("/v1/numbers/{PN}/messages"))
                .key(&key)
                .json(&json!({"to": {"phone": "+16505551234"}, "type": "template", "template": template})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    let sent = h.graph.last_request().unwrap().json().unwrap();
    assert_eq!(sent["template"]["components"][2], template["components"][2]);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── The catalog a template's products name ──────────────────────────────

/// catalogs/product-card-carousel-template-messages, the send example
/// (its three cards, catalog `194836987003835`), and
/// catalogs/spm-template-messages' header: a template naming a catalog is
/// sent only when that catalog is the number's WABA's (`GET
/// /{waba_id}/product_catalogs`, with the number's token, then the send),
/// as M5a checks a product message's (design D102; M5a left the
/// template's to M5b). Another WABA's catalog, a WABA with none, a second
/// catalog on one card, or a catalog in a parameter the library does not
/// type, is `404` with nothing sent. Decisive: the template's catalogs in
/// `messages::named_object`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // Meta's example, then each refusal
async fn a_templates_catalog_is_the_numbers_wabas() {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Send]).await;
    let card = |i: u32, product: &str, catalog: &str| {
        json!({"card_index": i, "components": [{"type": "header", "parameters": [
            {"type": "product", "product": {"product_retailer_id": product, "catalog_id": catalog}}]}]})
    };
    let carousel = |catalogs: [&str; 3]| {
        json!({"name": "carousel_template_product_cards_v1", "language": {"code": "en_US"},
               "components": [
                   {"type": "body", "parameters": [{"type": "text", "text": "Pablo"}]},
                   {"type": "carousel", "cards": [
                       card(0, "vrpj01fvwp", catalogs[0]),
                       card(1, "va2l5ioeat", catalogs[1]),
                       card(2, "sqpjv0mgde", catalogs[2])]}]})
    };
    let send = |template: &Value| {
        Call::new(Method::POST, format!("/v1/numbers/{PN}/messages"))
            .key(&key)
            .json(
                &json!({"to": {"phone": "+16505551234"}, "type": "template", "template": template}),
            )
    };
    let accepted = json!({"messaging_product": "whatsapp",
        "contacts": [{"input": "+16505551234", "wa_id": "16505551234"}],
        "messages": [{"id": "wamid.HBgLMTY1MDUwNzY1MjAVAgARGBI5QTNDQTVCM0Q0Q0Q2RTY3RTcA"}]});
    let catalog = "194836987003835";
    let catalogs_with = |id: &str| json!({"data": [{"id": id, "name": "Lucky Shrub"}]});
    let example = carousel([catalog; 3]);
    h.graph.push_json(200, catalogs_with(catalog));
    h.graph.push_json(200, accepted.clone());
    let reply = h.call(send(&example)).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    let requests = h.graph.requests();
    let [checked, posted] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_eq!(checked.method, Method::GET);
    assert_eq!(checked.path(), format!("/v25.0/{WABA}/product_catalogs"));
    assert_eq!(checked.bearer(), Some(TOKEN));
    assert_eq!(posted.method, Method::POST);
    assert_eq!(posted.path(), format!("/v25.0/{PN}/messages"));
    assert_eq!(posted.json().unwrap()["template"], example);
    // A single-product template's header (catalogs/spm-template-messages).
    let spm = json!({"name": "spm_template_named_params", "language": {"code": "en_US"},
        "components": [{"type": "header", "parameters": [{"type": "product", "product":
            {"product_retailer_id": "2lc20305pt", "catalog_id": catalog}}]}]});
    h.graph.push_json(200, catalogs_with(catalog));
    h.graph.push_json(200, accepted);
    let reply = h.call(send(&spm)).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    assert_eq!(h.graph.requests().len(), 4);
    // Refused, nothing sent: another WABA's catalog; a WABA with none; a
    // second catalog on one card; a catalog in a parameter the library
    // does not type (its `type` spelled otherwise).
    let mut hidden = example.clone();
    hidden["components"][1]["cards"][2]["components"][0]["parameters"][0]["type"] =
        json!("Product");
    hidden["components"][1]["cards"][2]["components"][0]["parameters"][0]["product"]["catalog_id"] =
        json!("987654321");
    for (template, answer) in [
        (carousel(["987654321"; 3]), catalogs_with(catalog)),
        (example.clone(), json!({"data": []})),
        (
            carousel([catalog, catalog, "987654321"]),
            catalogs_with(catalog),
        ),
        (hidden, catalogs_with(catalog)),
    ] {
        h.graph.push_json(200, answer);
        let reply = h.call(send(&template)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{template}: {}",
            reply.text
        );
    }
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 8);
    assert!(
        requests[4..].iter().all(|r| r.method == Method::GET),
        "nothing sent"
    );
    // A template naming no catalog asks nothing more.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "messages": [{"id": "wamid.X"}]}),
    );
    let reply = h
        .call(send(
            &json!({"name": "hello_world", "language": {"code": "en_US"}}),
        ))
        .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    assert_eq!(h.graph.requests().len(), 9);
    assert_eq!(h.graph.remaining(), 0);
}
