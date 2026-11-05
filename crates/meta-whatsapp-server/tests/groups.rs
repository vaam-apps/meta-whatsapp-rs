//! Roadmap M5f: a number's groups through the service (scope `groups`;
//! docs/design/server.md, section 4.2): create, list, read, change, delete,
//! invite links, participants, join requests and analytics.
//!
//! Each test sends Meta's documented request and answer (`groups/reference`,
//! `analytics`): the exact requests (method, path, query, the tenant's
//! token, the JSON body), `remaining() == 0`. The design's M5.2: **a group
//! id the number does not hold is `404` with zero vault reads**, decided by
//! the service's records of the number's groups before the token is opened
//! (D150). M5.1 (another tenant's number, zero vault reads) is
//! `tests/authz.rs`, which iterates every route of the document, these
//! included; the logs and the audit events are `tests/logs.rs`'s capture
//! (`tests/groups_logs.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::meta::{bytes, with_ids};
use common::{Call, Harness};
use meta_whatsapp_rs::core::testing::RecordedRequest;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const OTHER_TENANT: &str = "merchant-43";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const PN_2: &str = "106540352242923";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242924";
const TOKEN: &str = "EAAG-merchant-token";
const OTHER_TOKEN: &str = "EAAG-other-token";

/// `groups/webhooks`, "Group create succeed": its group.
const GROUP: &str = "Y2FwaV9ncm91cDoxOTUwNTU1MDA3OToxMjAzNjMzOTQzMjAdOTY0MTUZD";
const OTHER_GROUP: &str = "Y2FwaV9ncm91cDoxOTUwNTU1MDA3OToxMjAzNjMzOTQzMjA5OTk5OTkZD";

const CREATE_SUCCEED: &str = include_str!(
    "../../meta-whatsapp-webhooks/tests/fixtures/pages/groups.webhooks__group_create_succeed.json"
);
const CREATE_FAIL: &str = include_str!(
    "../../meta-whatsapp-webhooks/tests/fixtures/pages/groups.webhooks__group_create_fail.json"
);
const DELETE_SUCCEED: &str = include_str!(
    "../../meta-whatsapp-webhooks/tests/fixtures/pages/groups.webhooks__delete_group_succeed.json"
);
const DELETE_FAILS: &str = include_str!(
    "../../meta-whatsapp-webhooks/tests/fixtures/pages/groups.webhooks__delete_group_fails.json"
);

/// Two tenants with a WABA each (the first with two numbers), the first
/// tenant's key with the scope `groups`.
async fn connected() -> (Harness, String) {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN, PN_2], TOKEN).await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], OTHER_TOKEN)
        .await;
    let key = h.tenant_key(TENANT, &[Scope::Groups]).await;
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

/// Meta's webhook `fixture`, for `pn` of `waba`, with its group `group`.
fn event(fixture: &str, waba: &str, pn: &str, group: &str) -> Vec<u8> {
    let mut payload = with_ids(serde_json::from_str(fixture).unwrap(), waba, pn);
    for change in payload["entry"][0]["changes"].as_array_mut().unwrap() {
        for g in change["value"]["groups"].as_array_mut().unwrap() {
            g["group_id"] = json!(group);
        }
    }
    bytes(&payload)
}

/// Deliver Meta's creation event, which makes `group` the number's.
async fn hold(h: &Harness, pn: &str, group: &str) {
    let waba = if pn == OTHER_PN { OTHER_WABA } else { WABA };
    let reply = h.webhook(&event(CREATE_SUCCEED, waba, pn, group)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
}

fn on(method: Method, path: &str, key: &str) -> Call {
    Call::new(method, path).key(key)
}

fn graph_error(code: i64, details: &str) -> Value {
    json!({"error": {"message": details, "type": "OAuthException", "code": code,
                     "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn",
                     "error_data": {"messaging_product": "whatsapp", "details": details}}})
}

// ─── Creation, and the group's id arriving by webhook ────────────────────

/// groups/reference, "Create group": the request is Meta's body with its
/// `messaging_product`, the number's WABA token and no replay on a repeat
/// of the key. The answer is `202` with Meta's `request_id`; the group is
/// not the number's here until the `group_lifecycle_update` event that
/// reports it (a creation that failed, an event of another number and a
/// deletion that failed change nothing; a deletion that succeeded forgets
/// it). Refused before any request: a blank or long subject, a long
/// description, an unknown mode, a key the service does not know.
/// Decisive: `GroupRecords::on_event`, and the key of
/// `Fingerprint::create_group`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // Meta's example, each refusal, the event's life
async fn a_group_is_asked_for_and_becomes_the_numbers_when_meta_reports_it() {
    let (h, key) = connected().await;
    let create = |subject: &str| {
        on(Method::POST, &format!("/v1/numbers/{PN}/groups"), &key)
            .header("idempotency-key", "create-1")
            .json(&json!({"subject": subject, "description": "Jim would like to learn about new car purchase options.",
                          "join_approval_mode": "approval_required"}))
    };
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "request_id": "6f1c2d3e-4a5b-4c6d-8e7f-9a0b1c2d3e4f"}),
    );
    let reply = h.call(create("New Purchase Inquiry")).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"request_id": "6f1c2d3e-4a5b-4c6d-8e7f-9a0b1c2d3e4f"})
    );
    let requests = h.graph.requests();
    let [created] = &requests[..] else {
        panic!("one request: {requests:?}")
    };
    assert_eq!(created.method, Method::POST);
    assert_eq!(created.path(), format!("/v25.0/{PN}/groups"));
    assert_eq!(created.bearer(), Some(TOKEN));
    assert_eq!(
        created.json().unwrap(),
        json!({"messaging_product": "whatsapp", "subject": "New Purchase Inquiry",
               "description": "Jim would like to learn about new car purchase options.",
               "join_approval_mode": "approval_required"})
    );
    // The same key and body: Meta is not asked again.
    let replayed = h.call(create("New Purchase Inquiry")).await;
    assert_eq!(replayed.status, StatusCode::ACCEPTED, "{}", replayed.text);
    assert_eq!(replayed.json(), reply.json());
    assert_eq!(h.graph.requests().len(), 1, "no second group");
    // The same key with another body: refused.
    let reused = h.call(create("Another subject")).await;
    assert_eq!(reused.code(), "idempotency_key_reused", "{}", reused.text);

    // Not the number's yet: the vault is not read, Meta is not asked.
    let read = Call::get(format!("/v1/numbers/{PN}/groups/{GROUP}")).key(&key);
    let reads = h.kv.vault_reads();
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/groups/{GROUP}")).key(&key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found")
    );
    assert_eq!(h.kv.vault_reads(), reads, "decided before the vault");
    assert_eq!(h.graph.requests().len(), 1);

    // A creation that failed makes nothing the number's.
    let reply = h.webhook(&event(CREATE_FAIL, WABA, PN, GROUP)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/groups/{GROUP}")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    // An event of another tenant's number is that tenant's.
    let reply = h
        .webhook(&event(CREATE_SUCCEED, OTHER_WABA, OTHER_PN, GROUP))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/groups/{GROUP}")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    // The event that reports it: the number's now, and only this number's.
    hold(&h, PN, GROUP).await;
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "id": GROUP, "subject": "New Purchase Inquiry"}),
    );
    let reply = h.call(read).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_2}/groups/{GROUP}")).key(&key))
        .await;
    assert_eq!(
        reply.status,
        StatusCode::NOT_FOUND,
        "another number of the same tenant: {}",
        reply.text
    );
    // What is stored holds neither the group's id nor the number alone.
    for (stored, value) in h.kv.writes() {
        if stored.namespace() == meta_whatsapp_server_core::groups::NAMESPACE {
            assert!(!stored.key().contains("Y2Fw"), "{stored:?}");
            let value = String::from_utf8(value).unwrap();
            assert!(!value.contains("Y2Fw"), "{value}");
        }
    }
    // A deletion that failed leaves it; one that succeeded forgets it.
    let reply = h.webhook(&event(DELETE_FAILS, WABA, PN, GROUP)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    h.graph.push_json(200, json!({"id": GROUP}));
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/groups/{GROUP}")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let reply = h.webhook(&event(DELETE_SUCCEED, WABA, PN, GROUP)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/groups/{GROUP}")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);

    // Refused before any request.
    let before = h.graph.requests().len();
    for (body, field) in [
        (json!({"subject": "   "}), "subject"),
        (json!({"subject": "x".repeat(129)}), "subject"),
        (
            json!({"subject": "a", "description": "d".repeat(2049)}),
            "description",
        ),
        (
            json!({"subject": "a", "join_approval_mode": "manual"}),
            "join_approval_mode",
        ),
        (json!({"subject": "a", "picture": "x"}), "body"),
        (json!({}), "body"),
    ] {
        let reply = h
            .call(on(Method::POST, &format!("/v1/numbers/{PN}/groups"), &key).json(&body))
            .await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{body}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before, "nothing reached Meta");
    assert_eq!(h.graph.remaining(), 0);
}

// ─── M5.2: a group the number does not hold ──────────────────────────────

/// The design's M5.2 for this family, and the item's decisive test: a group
/// id the number does not hold is `404 not_found` **with zero vault reads**
/// and nothing asked of Meta, on every route that names one: another
/// tenant's group, another number's of the same tenant, an id never
/// recorded, an id that cannot be a group's. The same id on the number
/// that holds it is found. Decisive: `GroupRecords::owned` before
/// `Authorizer::owned_number` in the `OwnedGroup` extractor (put the vault
/// read first and every count below moves).
#[tokio::test]
#[allow(clippy::too_many_lines)] // each route, each kind of group id
async fn a_group_the_number_does_not_hold_is_not_found_with_zero_vault_reads() {
    let (h, key) = connected().await;
    // A group of the second tenant's, and one of this tenant's other
    // number, both reported by Meta's events.
    hold(&h, OTHER_PN, GROUP).await;
    hold(&h, PN_2, OTHER_GROUP).await;
    let routes = [
        (Method::GET, "", None),
        (Method::PATCH, "", Some(json!({"subject": "New"}))),
        (Method::DELETE, "", None),
        (Method::GET, "/invite-link", None),
        (Method::POST, "/invite-link", None),
        (Method::GET, "/participants", None),
        (
            Method::POST,
            "/participants/remove",
            Some(json!({"users": [{"phone": "+16505551234"}]})),
        ),
        (Method::GET, "/join-requests", None),
        (
            Method::POST,
            "/join-requests/approve",
            Some(json!({"join_request_ids": ["MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw"]})),
        ),
        (
            Method::POST,
            "/join-requests/reject",
            Some(json!({"join_request_ids": ["MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw"]})),
        ),
        (
            Method::GET,
            "/analytics?start=2020-01-01T00:00:00Z&end=2020-01-02T00:00:00Z&metrics=SENT",
            None,
        ),
    ];
    let mut checked = 0;
    // Ids: another tenant's, another number's, never held, unfit.
    for group in [GROUP, OTHER_GROUP, "NEVER-RECORDED", "a%20b", "%C3%A9"] {
        for (method, tail, body) in &routes {
            let mut call = on(
                method.clone(),
                &format!("/v1/numbers/{PN}/groups/{group}{tail}"),
                &key,
            );
            if let Some(body) = body {
                call = call.json(body);
            }
            let reads = h.kv.vault_reads();
            let reply = h.call(call).await;
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{method} {tail} {group}: {}",
                reply.text
            );
            assert_eq!(
                h.kv.vault_reads(),
                reads,
                "{method} {tail} {group} read the vault"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 5 * 11);
    assert!(h.graph.requests().is_empty(), "Meta was never asked");
    // The tenant's own group, on the number that holds it: found, and now
    // the vault is read, once.
    let reads = h.kv.vault_reads();
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "id": OTHER_GROUP, "subject": "Own"}),
    );
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_2}/groups/{OTHER_GROUP}")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert!(
        h.kv.vault_reads() > reads,
        "the token is opened after the group"
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// A listing records what Meta lists as the number's (the way to adopt a
/// group created before the service), and only for the tenant that listed
/// it: the other tenant's number still finds nothing. Meta's request is the
/// documented one. Decisive: `Groups::list`'s `records.record`.
#[tokio::test]
async fn listing_the_numbers_groups_makes_them_its_own() {
    let (h, key) = connected().await;
    h.graph.push_json(
        200,
        json!({"data": {"groups": [
            {"id": GROUP, "subject": "Artificial Intelligence Insights", "created_at": "1683731200"},
            {"id": OTHER_GROUP, "subject": "Watch Enthusiasts", "created_at": 1_683_731_300}]},
            "paging": {"cursors": {"after": "MTAxNTExOTQ1MjAwNzI5NDE=", "before": "NDMyNzQyODI3OTQw"},
                       "next": "https://graph.facebook.com/v25.0/106540352242922/groups?limit=2&after=MTAxNTExOTQ1MjAwNzI5NDE="}}),
    );
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/groups?limit=2&after=QUJD")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"data": [
            {"id": GROUP, "subject": "Artificial Intelligence Insights", "created_at": "2023-05-10T15:06:40Z"},
            {"id": OTHER_GROUP, "subject": "Watch Enthusiasts", "created_at": "2023-05-10T15:08:20Z"}],
            "next_after": "MTAxNTExOTQ1MjAwNzI5NDE=", "previous_before": "NDMyNzQyODI3OTQw"})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{PN}/groups"));
    assert_eq!(query(&request), pairs(&[("limit", "2"), ("after", "QUJD")]));
    assert_eq!(request.bearer(), Some(TOKEN));
    // Now found, on the number that listed it.
    h.graph.push_json(200, json!({"id": GROUP}));
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN}/groups/{GROUP}")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_2}/groups/{GROUP}")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    let other = h.tenant_key(OTHER_TENANT, &[Scope::Groups]).await;
    let reply = h
        .call(Call::get(format!("/v1/numbers/{OTHER_PN}/groups/{GROUP}")).key(&other))
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text);
    // Refused before any request: `limit` out of Meta's range, a cursor
    // that is not one.
    let before = h.graph.requests().len();
    for path in [
        format!("/v1/numbers/{PN}/groups?limit=0"),
        format!("/v1/numbers/{PN}/groups?limit=1025"),
        format!("/v1/numbers/{PN}/groups?after=a%20b"),
    ] {
        let reply = h.call(Call::get(path.clone()).key(&key)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── One group: read, change, delete ─────────────────────────────────────

/// groups/reference, "Get group info", "Update group settings" and
/// "Delete group": the documented requests, with Meta's field names. The
/// read asks the six scalar fields by default (the participants have a
/// route), or the ones named; the change takes a subject and/or a
/// description; a deletion is `204`. Meta refusing the group (it is gone)
/// is `404` with no Meta text; a refusal of a change keeps Meta's code and
/// kind and none of its text. Decisive: `Groups::get`'s field list,
/// `failed_on_group`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // three operations, each its refusals
async fn a_group_is_read_changed_and_deleted_as_metas_page_says() {
    let (h, key) = connected().await;
    hold(&h, PN, GROUP).await;
    let path = format!("/v1/numbers/{PN}/groups/{GROUP}");
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "id": GROUP, "subject": "Artificial Intelligence Insights",
               "creation_timestamp": "683731200", "suspended": false,
               "description": "Explore AI developments.", "total_participant_count": 6,
               "join_approval_mode": "auto_approve"}),
    );
    let reply = h.call(Call::get(&path).key(&key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"id": GROUP, "subject": "Artificial Intelligence Insights",
               "description": "Explore AI developments.", "join_approval_mode": "auto_approve",
               "suspended": false, "creation_timestamp": "1991-09-01T13:20:00Z",
               "total_participant_count": 6})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{GROUP}"));
    assert_eq!(
        query(&request),
        pairs(&[(
            "fields",
            "subject,description,join_approval_mode,suspended,creation_timestamp,total_participant_count"
        )])
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    // Named fields only.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "id": GROUP, "subject": "S"}),
    );
    let reply = h
        .call(Call::get(format!("{path}?fields=subject,suspended")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        query(&h.graph.last_request().unwrap()),
        pairs(&[("fields", "subject,suspended")])
    );
    assert_eq!(reply.json()["description"], Value::Null);
    // The participants are not a field of this route.
    let before = h.graph.requests().len();
    let reply = h
        .call(Call::get(format!("{path}?fields=participants")).key(&key))
        .await;
    assert_eq!(reply.json()["error"]["field"], "fields");
    let reply = h
        .call(Call::get(format!("{path}?unknown=1")).key(&key))
        .await;
    assert_eq!(reply.json()["error"]["field"], "query");
    assert_eq!(h.graph.requests().len(), before);

    // Update.
    h.graph.push_json(200, json!({"success": true}));
    let reply = h
        .call(
            on(Method::PATCH, &path, &key).json(
                &json!({"subject": "Watch Enthusiasts", "description": "Join our community."}),
            ),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{GROUP}"));
    assert_eq!(
        request.json().unwrap(),
        json!({"messaging_product": "whatsapp", "subject": "Watch Enthusiasts",
               "description": "Join our community."})
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    let before = h.graph.requests().len();
    for (body, field) in [
        (json!({}), "update"),
        (json!({"subject": ""}), "subject"),
        (json!({"description": "d".repeat(2049)}), "description"),
        (json!({"picture": "x"}), "body"),
    ] {
        let reply = h.call(on(Method::PATCH, &path, &key).json(&body)).await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{body}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before);
    // A refusal of the change: Meta's code and the service's kind, none of
    // its text (it may quote the caller's own subject, or a participant).
    h.graph.push_json(
        400,
        graph_error(131_209, "Group picture is not square: do not echo me"),
    );
    let reply = h
        .call(on(Method::PATCH, &path, &key).json(&json!({"subject": "x"})))
        .await;
    // 131209 is no code the library classifies: `unknown`, answered `502`
    // with Meta's code, and its text nowhere.
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY, "{}", reply.text);
    assert_eq!(reply.json()["error"]["graph"]["code"], 131_209);
    assert!(!reply.text.contains("do not echo me"), "{}", reply.text);

    // Delete.
    h.graph.push_json(200, json!({"success": true}));
    let reply = h.call(on(Method::DELETE, &path, &key)).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::DELETE);
    assert_eq!(request.path(), format!("/v25.0/{GROUP}"));
    assert_eq!(request.bearer(), Some(TOKEN));
    // Meta no longer has the group: not found, no Meta code or text. The
    // record stays until the deletion's event says so.
    h.graph
        .push_json(400, graph_error(100, "Unsupported get request"));
    let reply = h.call(Call::get(&path).key(&key)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        reply.text
    );
    assert!(reply.json()["error"]["graph"].is_null(), "{}", reply.text);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Invite links, participants, join requests ───────────────────────────

/// groups/reference, "Get and reset group invite link": `GET` and `POST
/// /{group}/invite_link` (the reset with `messaging_product`), each
/// answering the link; and "Get group info" for the participants.
#[tokio::test]
#[allow(clippy::too_many_lines)] // each call and its request
async fn invite_links_and_participants_are_read_and_the_link_reset() {
    let (h, key) = connected().await;
    hold(&h, PN, GROUP).await;
    let base = format!("/v1/numbers/{PN}/groups/{GROUP}");
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "invite_link": "https://chat.whatsapp.com/BjCg8D2zYrE7kS1uFp0xQa"}),
    );
    let reply = h
        .call(Call::get(format!("{base}/invite-link")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"invite_link": "https://chat.whatsapp.com/BjCg8D2zYrE7kS1uFp0xQa"})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{GROUP}/invite_link"));
    assert_eq!(request.bearer(), Some(TOKEN));
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "invite_link": "https://chat.whatsapp.com/NewCodeNewCode1234567"}),
    );
    let reply = h
        .call(on(Method::POST, &format!("{base}/invite-link"), &key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json()["invite_link"],
        "https://chat.whatsapp.com/NewCodeNewCode1234567"
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{GROUP}/invite_link"));
    assert_eq!(
        request.json().unwrap(),
        json!({"messaging_product": "whatsapp"})
    );

    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "id": GROUP, "total_participant_count": 3,
               "participants": [{"wa_id": "2228675309"}, {"wa_id": "7693349922"},
                                {"user_id": "US.13491208655302741918", "username": "@jim"}]}),
    );
    let reply = h
        .call(Call::get(format!("{base}/participants")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"participants": [
            {"wa_id": "2228675309", "user_id": null, "parent_user_id": null, "username": null},
            {"wa_id": "7693349922", "user_id": null, "parent_user_id": null, "username": null},
            {"wa_id": null, "user_id": "US.13491208655302741918", "parent_user_id": null, "username": "@jim"}],
            "total_participant_count": 3})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{GROUP}"));
    assert_eq!(
        query(&request),
        pairs(&[("fields", "participants,total_participant_count")])
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// groups/reference, "Remove group participants": `DELETE
/// /{group}/participants` with `participants` of `user` (a phone number or
/// a WhatsApp user id) or `user_id` (a BSUID), 1 to 8. A partial success
/// (Meta's `206`) is answered `partial`. Meta's refusal is its code and
/// kind with none of its text, which may quote the participant. Refused
/// before any request: none, more than 8, two ways at once, a bad number,
/// id or BSUID.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the request, the partial answer, each refusal
async fn participants_are_removed_by_phone_wa_id_or_bsuid() {
    let (h, key) = connected().await;
    hold(&h, PN, GROUP).await;
    let path = format!("/v1/numbers/{PN}/groups/{GROUP}/participants/remove");
    h.graph.push_json(200, json!({"success": true}));
    let reply = h
        .call(on(Method::POST, &path, &key).json(&json!({"users": [
            {"phone": "+17865347866"}, {"wa_id": "7693349922"},
            {"user_id": "US.13491208655302741918"}]})))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json(), json!({"partial": false}));
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::DELETE);
    assert_eq!(request.path(), format!("/v25.0/{GROUP}/participants"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        request.json().unwrap(),
        json!({"messaging_product": "whatsapp", "participants": [
            {"user": "+17865347866"}, {"user": "7693349922"},
            {"user_id": "US.13491208655302741918"}]})
    );
    // `groups/error-codes`: not all participant-level operations succeeded
    // is HTTP 206.
    h.graph.push_json(206, json!({"success": true}));
    let reply = h
        .call(on(Method::POST, &path, &key).json(&json!({"users": [{"phone": "+17865347866"}]})))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(reply.json(), json!({"partial": true}));
    // A refusal: Meta's code, its text nowhere.
    h.graph.push_json(
        400,
        graph_error(
            131_203,
            "Recipient +17865347866 has not accepted our new Terms",
        ),
    );
    let reply = h
        .call(on(Method::POST, &path, &key).json(&json!({"users": [{"phone": "+17865347866"}]})))
        .await;
    assert_eq!(
        reply.json()["error"]["graph"]["code"],
        131_203,
        "{}",
        reply.text
    );
    assert!(!reply.text.contains("17865347866"), "{}", reply.text);
    assert!(reply.json()["error"]["graph"]["details"].is_null());
    // Refused before any request.
    let before = h.graph.requests().len();
    let nine = vec![json!({"phone": "+16505551234"}); 9];
    for (body, field) in [
        (json!({"users": []}), "users"),
        (json!({"users": nine}), "users"),
        (json!({"users": [{}]}), "users[0]"),
        (
            json!({"users": [{"phone": "+16505551234", "user_id": "US.1"}]}),
            "users[0]",
        ),
        (
            json!({"users": [{"phone": "16505551234"}]}),
            "users[0].phone",
        ),
        (
            json!({"users": [{"wa_id": "+16505551234"}]}),
            "users[0].wa_id",
        ),
        (
            json!({"users": [{"user_id": "US.ENT.11815799212886844830"}]}),
            "users[0].user_id",
        ),
        (json!({"users": [{"name": "x"}]}), "body"),
    ] {
        let reply = h.call(on(Method::POST, &path, &key).json(&body)).await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{body}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before, "nothing reached Meta");
    assert_eq!(h.graph.remaining(), 0);
}

/// groups/reference, "Get join requests", "Approve join requests" and
/// "Reject join requests": the documented requests (the ids as
/// `join_requests`), the failures listed by id with Meta's codes and
/// kinds, never its texts. Refused before any request: none, more than
/// 100, an id that is not visible ASCII.
#[tokio::test]
#[allow(clippy::too_many_lines)] // three calls, each its example
async fn join_requests_are_listed_approved_and_rejected() {
    let (h, key) = connected().await;
    hold(&h, PN, GROUP).await;
    let base = format!("/v1/numbers/{PN}/groups/{GROUP}/join-requests");
    let id = "MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw";
    h.graph.push_json(
        200,
        json!({"data": [{"join_request_id": id, "wa_id": "16505551234", "creation_timestamp": 1_755_548_877},
                        {"join_request_id": "SECOND", "user_id": "US.13491208655302741918"}],
               "paging": {"cursors": {"before": "BEFORE", "after": "AFTER"}}}),
    );
    let reply = h
        .call(Call::get(format!("{base}?after=PAGE1")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"data": [
            {"join_request_id": id, "wa_id": "16505551234", "user_id": null, "parent_user_id": null,
             "username": null, "created_at": "2025-08-18T20:27:57Z"},
            {"join_request_id": "SECOND", "wa_id": null, "user_id": "US.13491208655302741918",
             "parent_user_id": null, "username": null, "created_at": null}],
            "next_after": null, "previous_before": "BEFORE"})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{GROUP}/join_requests"));
    assert_eq!(query(&request), pairs(&[("after", "PAGE1")]));
    assert_eq!(request.bearer(), Some(TOKEN));

    // Approve, with a failure listed.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "approved_join_requests": [id],
               "failed_join_requests": [{"join_request_id": "SECOND", "errors": [{
                   "code": 131_203, "message": "(#131203) Recipient +16505551234 has not accepted",
                   "title": "Unable to add participant to group",
                   "error_data": {"details": "Recipient has not accepted our new Terms of Service"}}]}],
               "errors": [{"code": 131_201, "message": "Not all", "title": "Partial"}]}),
    );
    let reply = h
        .call(
            on(Method::POST, &format!("{base}/approve"), &key)
                .json(&json!({"join_request_ids": [id, "SECOND"]})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let answered = reply.json();
    assert_eq!(answered["done"], json!([id]));
    assert_eq!(answered["failed"][0]["join_request_id"], "SECOND");
    assert_eq!(answered["failed"][0]["reasons"][0]["code"], 131_203);
    assert_eq!(answered["reasons"][0]["code"], 131_201);
    assert!(!reply.text.contains("has not accepted"), "{}", reply.text);
    assert!(!reply.text.contains("16505551234"), "{}", reply.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{GROUP}/join_requests"));
    assert_eq!(
        request.json().unwrap(),
        json!({"messaging_product": "whatsapp", "join_requests": [id, "SECOND"]})
    );
    // Reject.
    h.graph.push_json(
        200,
        json!({"messaging_product": "whatsapp", "rejected_join_requests": [id]}),
    );
    let reply = h
        .call(
            on(Method::POST, &format!("{base}/reject"), &key)
                .json(&json!({"join_request_ids": [id]})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"done": [id], "failed": [], "reasons": []})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::DELETE);
    assert_eq!(request.path(), format!("/v25.0/{GROUP}/join_requests"));
    assert_eq!(
        request.json().unwrap(),
        json!({"messaging_product": "whatsapp", "join_requests": [id]})
    );
    // Refused before any request.
    let before = h.graph.requests().len();
    let many = vec!["a"; 101];
    for body in [
        json!({"join_request_ids": []}),
        json!({"join_request_ids": many}),
        json!({"join_request_ids": ["a b"]}),
    ] {
        for verb in ["approve", "reject"] {
            let reply = h
                .call(on(Method::POST, &format!("{base}/{verb}"), &key).json(&body))
                .await;
            assert_eq!(
                reply.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{verb} {body}: {}",
                reply.text
            );
        }
    }
    assert_eq!(h.graph.requests().len(), before);
    assert_eq!(h.graph.remaining(), 0);
}

// ─── Analytics (parity row 81) ───────────────────────────────────────────

/// analytics, "Group analytics": the request is the documented one (the
/// number's WABA, `granularity=DAILY`, the one group, the metrics, Unix
/// times), the example response's days answered with RFC 3339 times. The
/// range reaches back 90 days and ends after it starts; the metrics are
/// Meta's five. Decisive: `analytics_query`'s bounds, the WABA of the
/// request.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the request, the answer, each refusal
async fn a_groups_analytics_ask_the_numbers_waba_for_that_one_group() {
    let (h, key) = connected().await;
    hold(&h, PN, GROUP).await;
    let now = time::OffsetDateTime::now_utc();
    let start = (now - time::Duration::days(3))
        .replace_nanosecond(0)
        .unwrap();
    let end = (now - time::Duration::days(1))
        .replace_nanosecond(0)
        .unwrap();
    let rfc = |at: time::OffsetDateTime| {
        at.format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    };
    h.graph.push_json(
        200,
        json!({"data": [{"granularity": "DAILY", "data_points": [
            {"group_id": GROUP, "start": 1_685_548_801, "end": 1_685_635_200,
             "sent": 100, "delivered": 250, "read": 200, "joined": 3, "left": 1},
            {"group_id": GROUP, "start": 1_685_635_201, "end": 1_685_721_600,
             "sent": 80, "delivered": 200, "read": 150, "joined": 1, "left": 0}]}],
               "paging": {"cursors": {"before": "MAZDZD", "after": "MjQZD"}}}),
    );
    let path = format!(
        "/v1/numbers/{PN}/groups/{GROUP}/analytics?start={}&end={}&metrics=SENT,DELIVERED,READ,PARTICIPANTS_JOINED,PARTICIPANTS_LEFT",
        rfc(start),
        rfc(end)
    );
    let reply = h.call(Call::get(&path).key(&key)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"data": [
            {"start": "2023-05-31T16:00:01Z", "end": "2023-06-01T16:00:00Z", "sent": 100,
             "delivered": 250, "read": 200, "joined": 3, "left": 1},
            {"start": "2023-06-01T16:00:01Z", "end": "2023-06-02T16:00:00Z", "sent": 80,
             "delivered": 200, "read": 150, "joined": 1, "left": 0}],
            "next_after": null})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::GET);
    assert_eq!(request.path(), format!("/v25.0/{WABA}/group_analytics"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        query(&request),
        pairs(&[
            ("start", &start.unix_timestamp().to_string()),
            ("end", &end.unix_timestamp().to_string()),
            ("granularity", "DAILY"),
            ("group_ids", &format!("[\"{GROUP}\"]")),
            (
                "metric_types",
                "[\"SENT\",\"DELIVERED\",\"READ\",\"PARTICIPANTS_JOINED\",\"PARTICIPANTS_LEFT\"]"
            ),
        ])
    );
    // Refused before any request.
    let before = h.graph.requests().len();
    let old = rfc(now - time::Duration::days(91));
    for (query, field) in [
        (
            format!("start={old}&end={}&metrics=SENT", rfc(end)),
            "start",
        ),
        (
            format!("start={}&end={}&metrics=SENT", rfc(end), rfc(start)),
            "end",
        ),
        (
            format!("start={}&end={}&metrics=SENT,BOGUS", rfc(start), rfc(end)),
            "metrics",
        ),
        (
            format!("start={}&end={}&metrics=", rfc(start), rfc(end)),
            "metrics",
        ),
        (
            format!("start=yesterday&end={}&metrics=SENT", rfc(end)),
            "start",
        ),
        (format!("end={}&metrics=SENT", rfc(end)), "query"),
    ] {
        let reply = h
            .call(Call::get(format!("/v1/numbers/{PN}/groups/{GROUP}/analytics?{query}")).key(&key))
            .await;
        assert_eq!(
            (reply.status, reply.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{query}: {}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), before, "nothing reached Meta");
    assert_eq!(h.graph.remaining(), 0);
}

// ─── The scope ───────────────────────────────────────────────────────────

/// The family has a scope of its own: a key with the others is `403`, and
/// the group routes, ownership aside, ask nothing of Meta. Decisive:
/// `Operation::scope` for the group operations.
#[tokio::test]
async fn the_groups_scope_is_its_own() {
    let (h, _) = connected().await;
    hold(&h, PN, GROUP).await;
    let others: Vec<Scope> = Scope::ALL
        .into_iter()
        .filter(|s| *s != Scope::Groups)
        .collect();
    let key = h.tenant_key(TENANT, &others).await;
    for call in [
        Call::get(format!("/v1/numbers/{PN}/groups")),
        Call::get(format!("/v1/numbers/{PN}/groups/{GROUP}")),
        Call::new(Method::DELETE, format!("/v1/numbers/{PN}/groups/{GROUP}")),
    ] {
        let reply = h.call(call.key(&key)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{}",
            reply.text
        );
    }
    assert!(h.graph.requests().is_empty());
}
