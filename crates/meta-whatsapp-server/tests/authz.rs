//! The authorization order (docs/design/server.md, section 3.3) and
//! acceptance test M1.3: every `{pn}` and `{waba_id}` route of the
//! committed OpenAPI document answers tenant B's key on tenant A's number
//! or WABA with `404`, and the vault is never read.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use common::{ALL_SCOPES, Call, Harness};
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_rs::webhooks::axum::body::{Body, Bytes};
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::{
    AllowedTenants, KeyScope, NumberStatus, Scope, TenantId, TenantStatus,
};
use serde_json::{Value, json};

const A: &str = "tenant-a";
const B: &str = "tenant-b";
const WABA_A: &str = "102290129340398";
const PN_A: &str = "106540352242922";
const WABA_B: &str = "102290129340399";
const PN_B: &str = "106540352242923";

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

/// Two tenants, each with a WABA, a number and a token in the vault.
async fn two_tenants() -> Harness {
    // The Meta app resumable uploads open their sessions under (roadmap
    // M5c1): without one, the route asks Meta nothing.
    let h = Harness::with_settings(meta_whatsapp_server::state::Settings {
        app_id: Some(meta_whatsapp_rs::core::ids::AppId::new("634974688087057").unwrap()),
        ..common::test_settings()
    });
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA_A, &[PN_A], "TOKEN-OF-A").await;
    h.connect(B, WABA_B, &[PN_B], "TOKEN-OF-B").await;
    h
}

/// Every operation in the committed document whose path names a number
/// or a WABA.
fn owned_routes() -> Vec<common::Operation> {
    common::spec_operations()
        .into_iter()
        .filter(|o| o.template.contains("{pn}") || o.template.contains("{waba_id}"))
        .collect()
}

/// The values of A's number and WABA.
fn on_a() -> common::Sample {
    common::Sample {
        tenant: A.to_owned(),
        waba: WABA_A.to_owned(),
        pn: PN_A.to_owned(),
        key_id: "placeholder".to_owned(),
    }
}

/// A call of `operation` on A's number or WABA, with a body and query it
/// would accept (so a refusal cannot be their fault).
fn call(operation: &common::Operation, key: &str, tenant: Option<&str>) -> Call {
    let mut call = common::sample_call(operation, &on_a(), Some(key));
    if let Some(tenant) = tenant {
        call = call.tenant(tenant);
    }
    call
}

/// M1.3. Decisive: step 4 (ownership) of the authorization order. Without
/// it, B's call reads A's token from the vault.
#[tokio::test]
#[allow(clippy::too_many_lines)] // the routes it must find, then every call
async fn another_tenants_number_or_waba_is_not_found_and_the_vault_is_never_read() {
    let h = two_tenants().await;
    let b_key = h.tenant_key(B, &ALL_SCOPES).await;
    let platform = h
        .platform_key(AllowedTenants::Only(vec![tenant(B)]), &ALL_SCOPES)
        .await;
    let routes = owned_routes();
    // The table is the document: every such route is covered, and these
    // must be among them.
    for known in [
        (Method::GET, "/v1/numbers/{pn}"),
        (Method::GET, "/v1/numbers/{pn}/profile"),
        (Method::PATCH, "/v1/numbers/{pn}/profile"),
        // Roadmap M5c2 (the design's M5.1): the display name.
        (Method::POST, "/v1/numbers/{pn}/display-name"),
        (Method::GET, "/v1/numbers/{pn}/display-name"),
        // Roadmap M5c3 (the design's M5.1): a number's settings.
        (Method::GET, "/v1/numbers/{pn}/settings"),
        (Method::PATCH, "/v1/numbers/{pn}/settings"),
        (Method::PUT, "/v1/numbers/{pn}/settings/payload-encryption"),
        (
            Method::DELETE,
            "/v1/numbers/{pn}/settings/payload-encryption",
        ),
        (Method::GET, "/v1/numbers/{pn}/username"),
        (Method::PUT, "/v1/numbers/{pn}/username"),
        (Method::DELETE, "/v1/numbers/{pn}/username"),
        (Method::GET, "/v1/numbers/{pn}/username/suggestions"),
        (Method::GET, "/v1/numbers/{pn}/official-business-account"),
        (Method::POST, "/v1/numbers/{pn}/official-business-account"),
        (Method::GET, "/v1/numbers/{pn}/compliance-info"),
        (Method::PUT, "/v1/numbers/{pn}/compliance-info"),
        (Method::GET, "/v1/numbers/{pn}/health-status"),
        (Method::GET, "/v1/numbers/{pn}/conversational-components"),
        (Method::PUT, "/v1/numbers/{pn}/conversational-components"),
        (Method::DELETE, "/v1/numbers/{pn}/contact-book/{bsuid}"),
        (Method::DELETE, "/v1/wabas/{waba_id}"),
        (Method::DELETE, "/v1/admin/wabas/{waba_id}/binding"),
        (Method::POST, "/v1/numbers/{pn}/messages"),
        (Method::POST, "/v1/numbers/{pn}/messages/{message_id}/read"),
        (Method::POST, "/v1/numbers/{pn}/media"),
        (Method::GET, "/v1/numbers/{pn}/media/{media_id}"),
        (Method::DELETE, "/v1/numbers/{pn}/media/{media_id}"),
        (Method::GET, "/v1/numbers/{pn}/conversations"),
        (
            Method::GET,
            "/v1/numbers/{pn}/conversations/{contact}/messages",
        ),
        (
            Method::POST,
            "/v1/numbers/{pn}/conversations/{contact}/messages",
        ),
        (
            Method::GET,
            "/v1/numbers/{pn}/conversations/{contact}/window",
        ),
        (
            Method::POST,
            "/v1/numbers/{pn}/conversations/{contact}/read",
        ),
        (Method::GET, "/v1/numbers/{pn}/messages/{message_id}"),
        (Method::GET, "/v1/wabas/{waba_id}/templates"),
        (Method::POST, "/v1/wabas/{waba_id}/templates"),
        (Method::DELETE, "/v1/wabas/{waba_id}/templates"),
        (Method::GET, "/v1/wabas/{waba_id}/templates/{id}"),
        // Roadmap M5b (the design's M5.1): a template's life after its
        // creation, and its groups.
        (Method::PATCH, "/v1/wabas/{waba_id}/templates/{id}"),
        (Method::POST, "/v1/wabas/{waba_id}/templates/{id}/unpause"),
        (Method::GET, "/v1/wabas/{waba_id}/templates/{id}/compare"),
        (Method::POST, "/v1/wabas/{waba_id}/templates/archive"),
        (Method::POST, "/v1/wabas/{waba_id}/templates/unarchive"),
        (Method::GET, "/v1/wabas/{waba_id}/templates/library"),
        (Method::POST, "/v1/wabas/{waba_id}/templates/library"),
        (Method::POST, "/v1/wabas/{waba_id}/templates/migrations"),
        (Method::POST, "/v1/wabas/{waba_id}/template-groups"),
        (
            Method::GET,
            "/v1/wabas/{waba_id}/template-groups/{group_id}",
        ),
        (
            Method::PATCH,
            "/v1/wabas/{waba_id}/template-groups/{group_id}",
        ),
        (
            Method::DELETE,
            "/v1/wabas/{waba_id}/template-groups/{group_id}",
        ),
        // Roadmap P2: a payment configuration is its WABA's; a payment and
        // a refund their number's.
        (Method::GET, "/v1/wabas/{waba_id}/payment-configurations"),
        (Method::POST, "/v1/wabas/{waba_id}/payment-configurations"),
        (
            Method::GET,
            "/v1/wabas/{waba_id}/payment-configurations/{configuration_name}",
        ),
        (
            Method::DELETE,
            "/v1/wabas/{waba_id}/payment-configurations/{configuration_name}",
        ),
        (
            Method::POST,
            "/v1/wabas/{waba_id}/payment-configurations/{configuration_name}/oauth-link",
        ),
        (
            Method::GET,
            "/v1/numbers/{pn}/payments/{configuration_name}/{reference_id}",
        ),
        // Roadmap M5d (the design's M5.1): a WABA's Flows.
        (Method::POST, "/v1/wabas/{waba_id}/flows"),
        (Method::GET, "/v1/wabas/{waba_id}/flows"),
        (Method::GET, "/v1/wabas/{waba_id}/flows/{flow_id}"),
        (Method::PATCH, "/v1/wabas/{waba_id}/flows/{flow_id}"),
        (Method::PUT, "/v1/wabas/{waba_id}/flows/{flow_id}/json"),
        (Method::POST, "/v1/wabas/{waba_id}/flows/{flow_id}/preview"),
        (Method::POST, "/v1/wabas/{waba_id}/flows/{flow_id}/publish"),
        (
            Method::POST,
            "/v1/wabas/{waba_id}/flows/{flow_id}/deprecate",
        ),
        (Method::DELETE, "/v1/wabas/{waba_id}/flows/{flow_id}"),
        (Method::POST, "/v1/numbers/{pn}/payments/refunds"),
        // Roadmap M5g (the design's M5.1): commerce, QR codes, analytics
        // and the block list. A catalog is its WABA's, a code its number's.
        (Method::GET, "/v1/numbers/{pn}/commerce-settings"),
        (Method::PATCH, "/v1/numbers/{pn}/commerce-settings"),
        (Method::GET, "/v1/wabas/{waba_id}/catalogs"),
        (Method::GET, "/v1/wabas/{waba_id}/catalogs/{catalog_id}"),
        (
            Method::GET,
            "/v1/wabas/{waba_id}/catalogs/{catalog_id}/products",
        ),
        (Method::POST, "/v1/numbers/{pn}/qr-codes"),
        (Method::GET, "/v1/numbers/{pn}/qr-codes"),
        (Method::GET, "/v1/numbers/{pn}/qr-codes/{code}"),
        (Method::PATCH, "/v1/numbers/{pn}/qr-codes/{code}"),
        (Method::DELETE, "/v1/numbers/{pn}/qr-codes/{code}"),
        (Method::GET, "/v1/wabas/{waba_id}/analytics/messaging"),
        (Method::GET, "/v1/wabas/{waba_id}/analytics/conversations"),
        (Method::GET, "/v1/wabas/{waba_id}/analytics/pricing"),
        (Method::GET, "/v1/wabas/{waba_id}/analytics/templates"),
        (Method::GET, "/v1/numbers/{pn}/blocked-users"),
        (Method::POST, "/v1/numbers/{pn}/blocked-users"),
        (Method::POST, "/v1/numbers/{pn}/blocked-users/unblock"),
        // Roadmap M5l (the design's M5.1): conversation routing.
        (Method::GET, "/v1/numbers/{pn}/routing-roles"),
        (Method::POST, "/v1/numbers/{pn}/thread-control/pass"),
        (Method::POST, "/v1/numbers/{pn}/thread-control/take"),
        (Method::POST, "/v1/numbers/{pn}/thread-control/release"),
        // Roadmap M5f (the design's M5.1): a number's groups.
        (Method::POST, "/v1/numbers/{pn}/groups"),
        (Method::GET, "/v1/numbers/{pn}/groups"),
        (Method::GET, "/v1/numbers/{pn}/groups/{group_id}"),
        (Method::PATCH, "/v1/numbers/{pn}/groups/{group_id}"),
        (Method::DELETE, "/v1/numbers/{pn}/groups/{group_id}"),
        (
            Method::GET,
            "/v1/numbers/{pn}/groups/{group_id}/invite-link",
        ),
        (
            Method::POST,
            "/v1/numbers/{pn}/groups/{group_id}/invite-link",
        ),
        (
            Method::GET,
            "/v1/numbers/{pn}/groups/{group_id}/participants",
        ),
        (
            Method::POST,
            "/v1/numbers/{pn}/groups/{group_id}/participants/remove",
        ),
        (
            Method::GET,
            "/v1/numbers/{pn}/groups/{group_id}/join-requests",
        ),
        (
            Method::POST,
            "/v1/numbers/{pn}/groups/{group_id}/join-requests/approve",
        ),
        (
            Method::POST,
            "/v1/numbers/{pn}/groups/{group_id}/join-requests/reject",
        ),
        (Method::GET, "/v1/numbers/{pn}/groups/{group_id}/analytics"),
        // Roadmap M5h (the design's M5.1): marketing sends, max price and
        // welcome message sequences.
        (Method::POST, "/v1/numbers/{pn}/marketing-messages"),
        (Method::GET, "/v1/wabas/{waba_id}/marketing/onboarding"),
        (
            Method::GET,
            "/v1/wabas/{waba_id}/marketing/cloud-api-marketing",
        ),
        (
            Method::PUT,
            "/v1/wabas/{waba_id}/marketing/cloud-api-marketing",
        ),
        (
            Method::POST,
            "/v1/wabas/{waba_id}/marketing/template-duplicates",
        ),
        (Method::GET, "/v1/wabas/{waba_id}/marketing/reach-estimates"),
        (
            Method::GET,
            "/v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price",
        ),
        (
            Method::PUT,
            "/v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price",
        ),
        (
            Method::DELETE,
            "/v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price",
        ),
        (Method::POST, "/v1/wabas/{waba_id}/welcome-sequences"),
        (Method::GET, "/v1/wabas/{waba_id}/welcome-sequences"),
        (
            Method::GET,
            "/v1/wabas/{waba_id}/welcome-sequences/{sequence_id}",
        ),
        (
            Method::PATCH,
            "/v1/wabas/{waba_id}/welcome-sequences/{sequence_id}",
        ),
        (
            Method::DELETE,
            "/v1/wabas/{waba_id}/welcome-sequences/{sequence_id}",
        ),
        // Roadmap M5e (the design's M5.1): a number's calling.
        (Method::GET, "/v1/numbers/{pn}/call-settings"),
        (Method::PATCH, "/v1/numbers/{pn}/call-settings"),
        (Method::GET, "/v1/numbers/{pn}/call-permissions"),
        (Method::POST, "/v1/numbers/{pn}/calls"),
        (Method::POST, "/v1/numbers/{pn}/calls/{call_id}/pre-accept"),
        (Method::POST, "/v1/numbers/{pn}/calls/{call_id}/accept"),
        (Method::POST, "/v1/numbers/{pn}/calls/{call_id}/reject"),
        (Method::POST, "/v1/numbers/{pn}/calls/{call_id}/terminate"),
        (Method::GET, "/v1/wabas/{waba_id}/analytics/calls"),
    ] {
        assert!(
            routes
                .iter()
                .any(|o| o.method == known.0 && o.template == known.1),
            "{known:?} is not in the document"
        );
    }

    let mut checked = 0;
    for operation in &routes {
        // Admin routes refuse any non-admin key before anything else.
        let expected = if operation.admin() {
            (StatusCode::FORBIDDEN, "forbidden")
        } else {
            (StatusCode::NOT_FOUND, "not_found")
        };
        for (key, named) in [(&b_key, None), (&platform, Some(B))] {
            let before = h.kv.vault_reads();
            let reply = h.call(call(operation, key, named)).await;
            let label = operation.label();
            assert_eq!(
                (reply.status, reply.code().as_str()),
                expected,
                "{label} with B's key: {}",
                reply.text
            );
            assert_eq!(
                h.kv.vault_reads(),
                before,
                "{label} read the vault for tenant B"
            );
            checked += 1;
        }
    }
    assert!(checked >= 42 + 2 * 13 + 2 * 9, "only {checked} calls");
    assert!(h.graph.requests().is_empty(), "no call reached Meta");
    assert_eq!(h.graph.remaining(), 0);
    // Nothing of A's changed.
    let waba = h
        .store
        .waba(&WabaId::new(WABA_A).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(waba.tenant_id, tenant(A));
    let number = h
        .store
        .number(&PhoneNumberId::new(PN_A).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(number.status, NumberStatus::Connected);
    assert!(
        h.vault
            .get(&WabaId::new(WABA_A).unwrap())
            .await
            .unwrap()
            .is_some()
    );
}

/// The design's M5.2 for every route of the document that names a group of
/// a number (roadmap M5f), each with A's own key on A's own number: the
/// sample's group is none the number holds (nobody reported it), so it is
/// `404 not_found` with **zero vault reads** and nothing asked of Meta;
/// once Meta's creation event makes it the number's, the same call reads
/// the vault and asks Meta. A group route that opened the token before
/// the group fails the first half; one that skipped the group fails
/// neither (the document's own M5.1 table is the other tenants').
/// Decisive: `GroupRecords::owned` before `owned_number` in the
/// `OwnedGroup` extractor.
#[tokio::test]
async fn a_group_the_number_does_not_hold_is_not_found_before_the_vault_on_every_group_route() {
    let h = two_tenants().await;
    let a_key = h.tenant_key(A, &ALL_SCOPES).await;
    let routes: Vec<_> = owned_routes()
        .into_iter()
        .filter(|o| o.template.contains("{pn}") && o.template.contains("{group_id}"))
        .collect();
    assert!(routes.len() >= 11, "{} group routes", routes.len());
    for operation in &routes {
        let label = operation.label();
        let before = h.kv.vault_reads();
        let reply = h.call(call(operation, &a_key, None)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{label}: {}",
            reply.text
        );
        assert_eq!(h.kv.vault_reads(), before, "{label} read the vault");
    }
    assert!(h.graph.requests().is_empty(), "Meta was never asked");
    // Another tenant's number holds that group; it is still not A's.
    let held = h
        .webhook(&common::meta::group_created(
            WABA_B,
            PN_B,
            common::SAMPLE_TEMPLATE_GROUP_ID,
        ))
        .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.text);
    for operation in &routes {
        let label = operation.label();
        let before = h.kv.vault_reads();
        let reply = h.call(call(operation, &a_key, None)).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{label}: {}",
            reply.text
        );
        assert_eq!(h.kv.vault_reads(), before, "{label} read the vault");
    }
    assert!(h.graph.requests().is_empty());
    // A's own number reported it: now the token is opened and Meta asked.
    let held = h
        .webhook(&common::meta::group_created(
            WABA_A,
            PN_A,
            common::SAMPLE_TEMPLATE_GROUP_ID,
        ))
        .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.text);
    for operation in &routes {
        let label = operation.label();
        let before = h.kv.vault_reads();
        h.graph.push_json(
            500,
            json!({"error": {"message": "x", "type": "OAuthException", "code": 2}}),
        );
        let asked = h.graph.requests().len();
        let _ = h.call(call(operation, &a_key, None)).await;
        assert!(
            h.kv.vault_reads() > before,
            "{label}: the vault was not read"
        );
        assert_eq!(h.graph.requests().len(), asked + 1, "{label}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// The design's M5.2 for every route of the document that names a call of
/// a number (roadmap M5e), each with A's own key on A's own number: the
/// sample's call is none the number holds (no event reported it), so it is
/// `404 not_found` with **zero vault reads** and nothing asked of Meta;
/// once Meta's call event makes it the number's, the same call reads the
/// vault and asks Meta. A call route that opened the token before the call
/// fails the first half; one that skipped the call fails neither (the
/// document's own M5.1 table is the other tenants'). Decisive:
/// `CallRecords::owned` before `owned_number` in the `OwnedCall`
/// extractor.
#[tokio::test]
async fn a_call_the_number_does_not_hold_is_not_found_before_the_vault_on_every_call_route() {
    let h = two_tenants().await;
    let a_key = h.tenant_key(A, &ALL_SCOPES).await;
    let routes: Vec<_> = owned_routes()
        .into_iter()
        .filter(|o| o.template.contains("{pn}") && o.template.contains("{call_id}"))
        .collect();
    assert_eq!(routes.len(), 4, "the call routes: {routes:?}");
    for operation in &routes {
        let label = operation.label();
        let before = h.kv.vault_reads();
        let reply = h.call(call(operation, &a_key, None)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{label}: {}",
            reply.text
        );
        assert_eq!(h.kv.vault_reads(), before, "{label} read the vault");
    }
    assert!(h.graph.requests().is_empty(), "Meta was never asked");
    // Another tenant's number holds that call; it is still not A's.
    let held = h
        .webhook(&common::meta::call_connected(
            WABA_B,
            PN_B,
            common::SAMPLE_CALL_ID,
        ))
        .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.text);
    for operation in &routes {
        let label = operation.label();
        let before = h.kv.vault_reads();
        let reply = h.call(call(operation, &a_key, None)).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{label}: {}",
            reply.text
        );
        assert_eq!(h.kv.vault_reads(), before, "{label} read the vault");
    }
    assert!(h.graph.requests().is_empty());
    // A's own number reported it (a status event: the connect event of the
    // same call is B's, and Meta's retries are deduplicated by call and
    // event): now the token is opened and Meta asked.
    let held = h
        .webhook(&common::meta::call_status(
            WABA_A,
            PN_A,
            common::SAMPLE_CALL_ID,
        ))
        .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.text);
    for operation in &routes {
        let label = operation.label();
        let before = h.kv.vault_reads();
        h.graph.push_json(
            500,
            json!({"error": {"message": "x", "type": "OAuthException", "code": 2}}),
        );
        let asked = h.graph.requests().len();
        let _ = h.call(call(operation, &a_key, None)).await;
        assert!(
            h.kv.vault_reads() > before,
            "{label}: the vault was not read"
        );
        assert_eq!(h.graph.requests().len(), asked + 1, "{label}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// Roadmap L20c: a `{pn}` or `{waba_id}` that is not a numeric id names
/// nothing a tenant can hold, so it is `404 not_found`, the answer for an
/// unbound one (and every route's answer before ids were checked), with
/// the vault and Meta untouched, on the owner's own key and the admin's.
/// Decisive: answering the refused id with anything else (a `422`) fails
/// here, on every route.
#[tokio::test]
async fn a_path_id_that_is_not_numeric_is_not_found() {
    let h = two_tenants().await;
    let a_key = h.tenant_key(A, &ALL_SCOPES).await;
    let admin = h.admin_key().await;
    let too_long = "1".repeat(65);
    let routes = owned_routes();
    let mut checked = 0;
    // Percent-encoded where a path needs it: `1/2`, `+1`, ` 1`, `١٢`
    // (Arabic-Indic digits); then 65 digits, one past the cap.
    for bad in [
        "WABA_ID",
        "1%2F2",
        "%2B1",
        "%201",
        "%D9%A1%D9%A2",
        too_long.as_str(),
    ] {
        let sample = common::Sample {
            tenant: A.to_owned(),
            waba: bad.to_owned(),
            pn: bad.to_owned(),
            key_id: "placeholder".to_owned(),
        };
        for operation in &routes {
            let key = if operation.admin() { &admin } else { &a_key };
            let before = h.kv.vault_reads();
            let reply = h
                .call(common::sample_call(operation, &sample, Some(key)))
                .await;
            let label = operation.label();
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{label} on {bad:?}: {}",
                reply.text
            );
            assert_eq!(
                h.kv.vault_reads(),
                before,
                "{label} on {bad:?} read the vault"
            );
            checked += 1;
        }
    }
    assert!(checked >= 6 * 14, "only {checked} calls");
    assert!(h.graph.requests().is_empty(), "no call reached Meta");
}

/// The owned routes that ask Meta nothing, and what A's own sample call
/// answers: the inbox's reads (roadmaps M2a and M2g), which read the
/// service's own store, and marking a conversation read without
/// `notify_customer` (the sample's empty body). They still take the whole
/// authorization order, the vault included, as every owned route does. A
/// message by id A's inbox does not hold is `404`, after the vault. An
/// erasure (roadmap M2f) too: it asks Meta nothing.
const ASK_META_NOTHING: [(Method, &str, StatusCode); 9] = [
    // A resumable upload's chunk and status name an upload the sample
    // does not hold: refused after the vault, before Meta (roadmap M5c1).
    (
        Method::POST,
        "/v1/numbers/{pn}/media-uploads/{upload_id}",
        StatusCode::UNPROCESSABLE_ENTITY,
    ),
    (
        Method::GET,
        "/v1/numbers/{pn}/media-uploads/{upload_id}",
        StatusCode::UNPROCESSABLE_ENTITY,
    ),
    (
        Method::GET,
        "/v1/numbers/{pn}/conversations",
        StatusCode::OK,
    ),
    (
        Method::GET,
        "/v1/numbers/{pn}/conversations/{contact}/messages",
        StatusCode::OK,
    ),
    (
        Method::GET,
        "/v1/numbers/{pn}/conversations/{contact}/window",
        StatusCode::OK,
    ),
    (
        Method::POST,
        "/v1/numbers/{pn}/conversations/{contact}/read",
        StatusCode::NO_CONTENT,
    ),
    (
        Method::GET,
        "/v1/numbers/{pn}/messages/{message_id}",
        StatusCode::NOT_FOUND,
    ),
    (Method::POST, "/v1/numbers/{pn}/erasures", StatusCode::OK),
    // Roadmap M5c3 (D192): the one legal act, off in this harness (the
    // vault is read for the owner's token; the gate then refuses before
    // Meta is asked). The application and the contact book erasure are not
    // gated and are asked of Meta like any route.
    (
        Method::PUT,
        "/v1/numbers/{pn}/compliance-info",
        StatusCode::FORBIDDEN,
    ),
];

/// The control of M1.3: the counter counts, and A's own key does reach the
/// vault on the same routes.
#[tokio::test]
async fn the_owner_reads_the_vault_on_every_owned_route() {
    let h = two_tenants().await;
    let a_key = h.tenant_key(A, &ALL_SCOPES).await;
    // The sample's group is A's number's (Meta's creation event), as a
    // group route needs it to reach the vault (roadmap M5f).
    let held = h
        .webhook(&common::meta::group_created(
            WABA_A,
            PN_A,
            common::SAMPLE_TEMPLATE_GROUP_ID,
        ))
        .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.text);
    // And the sample's call (Meta's connect event, roadmap M5e).
    let held = h
        .webhook(&common::meta::call_connected(
            WABA_A,
            PN_A,
            common::SAMPLE_CALL_ID,
        ))
        .await;
    assert_eq!(held.status, StatusCode::OK, "{}", held.text);
    for operation in owned_routes() {
        if operation.admin() {
            continue;
        }
        let label = operation.label();
        // A number's live event stream needs the number to be the
        // tenant's (step 4, the first test above), and no token: it reads
        // the service's own outbox, never the vault nor Meta (roadmap M2b).
        if operation.template == "/v1/numbers/{pn}/events/stream" {
            let before = h.kv.vault_reads();
            let reply = h.call(call(&operation, &a_key, None)).await;
            assert_eq!(reply.status, StatusCode::OK, "{label}: {}", reply.text);
            assert_eq!(h.kv.vault_reads(), before, "{label} read the vault");
            continue;
        }
        let before = h.kv.vault_reads();
        let asked = h.graph.requests().len();
        if let Some((_, _, status)) = ASK_META_NOTHING
            .iter()
            .find(|(m, t, _)| *m == operation.method && *t == operation.template)
        {
            let reply = h.call(call(&operation, &a_key, None)).await;
            assert_eq!(reply.status, *status, "{label}: {}", reply.text);
            assert!(
                h.kv.vault_reads() > before,
                "{label}: A's own call did not read the vault"
            );
            assert_eq!(h.graph.requests().len(), asked, "{label} asked Meta");
            continue;
        }
        // Graph answers with an error: the point is the vault read before it.
        h.graph.push_json(
            500,
            json!({"error": {"message": "x", "type": "OAuthException", "code": 2}}),
        );
        let _ = h.call(call(&operation, &a_key, None)).await;
        assert!(
            h.kv.vault_reads() > before,
            "{label}: A's own call did not read the vault"
        );
        assert_eq!(
            h.graph.requests().len(),
            asked + 1,
            "{label}: Meta was not asked"
        );
        let request = h.graph.last_request().unwrap();
        // A resumable upload's session is opened with `Authorization: OAuth`
        // (the Graph guide's header), every other route with `Bearer`.
        let token = request.bearer().or_else(|| {
            request
                .header("authorization")
                .and_then(|value| value.strip_prefix("OAuth "))
        });
        assert_eq!(token, Some("TOKEN-OF-A"), "{label}");
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// A number of another tenant and a number of nobody answer alike: no
/// probing.
#[tokio::test]
async fn a_foreign_number_looks_like_a_missing_one() {
    let h = two_tenants().await;
    let b_key = h.tenant_key(B, &[Scope::Numbers]).await;
    let foreign = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&b_key))
        .await;
    let missing = h.call(Call::get("/v1/numbers/999999999").key(&b_key)).await;
    let mut foreign = foreign.json();
    let mut missing = missing.json();
    foreign["error"]["request_id"] = Value::Null;
    missing["error"]["request_id"] = Value::Null;
    assert_eq!(foreign, missing);
}

/// Step 1 on every keyed operation of the committed document: without a
/// key it is `401` and the body is never polled; with a key of the wrong
/// kind (a tenant key on `/v1/admin`, an admin key elsewhere) `403
/// forbidden`, before the body is read and before the vault is.
/// Decisive: the guards, and step 1 before any extractor reads the body.
#[tokio::test]
async fn every_keyed_operation_needs_a_key_of_its_kind_before_the_body() {
    let h = two_tenants().await;
    let admin = h.admin_key().await;
    let tenant_key = h.tenant_key(A, &ALL_SCOPES).await;
    let sample = common::Sample {
        tenant: A.to_owned(),
        waba: WABA_A.to_owned(),
        pn: PN_A.to_owned(),
        key_id: "placeholder".to_owned(),
    };
    let reads = h.kv.vault_reads();
    let mut keyed = 0;
    for operation in common::spec_operations() {
        if !operation.keyed {
            continue;
        }
        keyed += 1;
        let wrong_kind = if operation.admin() {
            &tenant_key
        } else {
            &admin
        };
        for (key, expected) in [
            (None, (StatusCode::UNAUTHORIZED, "unauthenticated")),
            (Some(wrong_kind), (StatusCode::FORBIDDEN, "forbidden")),
        ] {
            let polled = Arc::new(AtomicBool::new(false));
            let flag = polled.clone();
            let body = Body::from_stream(futures::stream::poll_fn(move |_| {
                flag.store(true, Ordering::SeqCst);
                Poll::Ready(Some(Ok::<_, std::io::Error>(Bytes::from_static(b"{}"))))
            }));
            let mut call = Call::new(operation.method.clone(), sample.fill(&operation.template))
                .header("content-type", "application/json")
                .body(body);
            if let Some(key) = key {
                call = call.key(key);
            }
            let reply = h.call(call).await;
            let label = format!("{} with {key:?}", operation.label());
            assert_eq!(
                (reply.status, reply.code().as_str()),
                expected,
                "{label}: {}",
                reply.text
            );
            if expected.0 == StatusCode::UNAUTHORIZED {
                assert_eq!(reply.headers["www-authenticate"], "Bearer", "{label}");
            }
            assert!(!polled.load(Ordering::SeqCst), "{label}: the body was read");
        }
    }
    assert!(keyed >= 19, "{keyed} keyed operations");
    assert_eq!(h.kv.vault_reads(), reads, "no refused call read the vault");
    assert!(h.graph.requests().is_empty());
}

/// Every way a key fails step 1 is the same `401`.
#[tokio::test]
async fn bad_keys_are_401() {
    let h = two_tenants().await;
    let key = h.tenant_key(A, &[Scope::Numbers]).await;
    let (id, secret) = key["wak_".len()..].split_once('_').unwrap();
    let flipped = format!(
        "wak_{id}_{}{}",
        &secret[..secret.len() - 1],
        if secret.ends_with('a') { 'b' } else { 'a' }
    );
    let unknown = format!("wak_{}_{secret}", "0".repeat(id.len()));
    for (authorization, why) in [
        (None, "no header"),
        (Some("Basic dXNlcjpwYXNz".to_owned()), "another scheme"),
        (Some("Bearer".to_owned()), "no key"),
        (Some("Bearer not-a-key".to_owned()), "not a key"),
        (Some(format!("Bearer {flipped}")), "wrong secret"),
        (Some(format!("Bearer {unknown}")), "unknown key id"),
        (Some(format!("Bearer {key}x")), "trailing character"),
    ] {
        let mut request = Call::get("/v1/numbers");
        if let Some(value) = authorization {
            request = request.header("authorization", &value);
        }
        let reply = h.call(request).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{why}");
        assert_eq!(reply.code(), "unauthenticated", "{why}");
    }
    // The genuine key works, with the scheme in any case.
    let reply = h
        .call(Call::get("/v1/numbers").header("authorization", &format!("bearer {key}")))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
}

/// A revoked or expired key stops at once.
#[tokio::test]
async fn revoked_and_expired_keys_are_401() {
    let h = two_tenants().await;
    let key = h.tenant_key(A, &[Scope::Numbers]).await;
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&key)).await.status,
        StatusCode::OK
    );
    let key_id = key["wak_".len()..].split_once('_').unwrap().0;
    assert!(
        h.store
            .revoke_key(&KeyScope::Tenant(tenant(A)), key_id)
            .await
            .unwrap()
    );
    let reply = h.call(Call::get("/v1/numbers").key(&key)).await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::UNAUTHORIZED, "unauthenticated")
    );

    let (minted, _) = meta_whatsapp_server::api::admin::mint(
        h.store.as_ref(),
        meta_whatsapp_server::model::KeyOwner::Tenant(tenant(A)),
        vec![Scope::Numbers],
        String::new(),
        Some(time::OffsetDateTime::now_utc() + time::Duration::milliseconds(300)),
    )
    .await
    .unwrap();
    let expiring = minted.expose_key().to_owned();
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&expiring)).await.status,
        StatusCode::OK
    );
    // Expiry is read from the service's one clock (roadmap S2: the core's
    // `Authorizer` takes it injected), the harness's.
    h.clock.advance(std::time::Duration::from_secs(2));
    let reply = h.call(Call::get("/v1/numbers").key(&expiring)).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "expired");
}

/// Step 2: which tenant a key acts as.
#[tokio::test]
async fn keys_act_only_as_their_tenants() {
    let h = two_tenants().await;
    h.tenant("tenant-c").await;
    let a_key = h.tenant_key(A, &[Scope::Numbers]).await;
    let only_a = h
        .platform_key(AllowedTenants::Only(vec![tenant(A)]), &[Scope::Numbers])
        .await;
    let any = h.platform_key(AllowedTenants::All, &[Scope::Numbers]).await;
    let admin = h.admin_key().await;
    let numbers_of = |reply: common::Reply| -> Vec<String> {
        reply.json()["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["phone_number_id"].as_str().unwrap().to_owned())
            .collect()
    };

    // A tenant key: its own tenant, named or not; never another.
    let own = h.call(Call::get("/v1/numbers").key(&a_key)).await;
    assert_eq!(numbers_of(own), [PN_A]);
    let named = h.call(Call::get("/v1/numbers").key(&a_key).tenant(A)).await;
    assert_eq!(numbers_of(named), [PN_A]);
    let other = h.call(Call::get("/v1/numbers").key(&a_key).tenant(B)).await;
    assert_eq!(
        (other.status, other.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden")
    );

    // A platform key: only with WA-Tenant, only within its set.
    for (key, tenant_header, expected) in [
        (&only_a, None, Err("forbidden")),
        (&only_a, Some(A), Ok(PN_A)),
        (&only_a, Some(B), Err("forbidden")),
        (&only_a, Some("not a tenant id!"), Err("forbidden")),
        (&any, Some(B), Ok(PN_B)),
        (&any, Some("tenant-nobody"), Err("forbidden")),
        (&any, Some("tenant-c"), Ok("")),
        (&admin, None, Err("forbidden")),
        (&admin, Some(A), Err("forbidden")),
    ] {
        let mut request = Call::get("/v1/numbers").key(key);
        if let Some(t) = tenant_header {
            request = request.tenant(t);
        }
        let reply = h.call(request).await;
        match expected {
            Ok(number) => {
                assert_eq!(
                    reply.status,
                    StatusCode::OK,
                    "{tenant_header:?}: {}",
                    reply.text
                );
                let expected: Vec<&str> = if number.is_empty() {
                    vec![]
                } else {
                    vec![number]
                };
                assert_eq!(numbers_of(reply), expected);
            }
            Err(code) => {
                assert_eq!(reply.status, StatusCode::FORBIDDEN, "{tenant_header:?}");
                assert_eq!(reply.code(), code);
            }
        }
    }
    // Tenant and platform keys never reach /v1/admin.
    for key in [&a_key, &any] {
        let reply = h
            .call(Call::get("/v1/admin/tenants").key(key).tenant(A))
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden")
        );
    }
}

/// Suspension: the tenant's keys and platform keys naming it stop, with
/// `tenant_suspended`; lifting it restores them.
#[tokio::test]
async fn a_suspended_tenant_is_403_tenant_suspended() {
    let h = two_tenants().await;
    let a_key = h.tenant_key(A, &[Scope::Numbers]).await;
    let platform = h.platform_key(AllowedTenants::All, &[Scope::Numbers]).await;
    let reads = h.kv.vault_reads();
    h.store
        .update_tenant(&tenant(A), None, Some(TenantStatus::Suspended))
        .await
        .unwrap();
    for request in [
        Call::get("/v1/numbers").key(&a_key),
        Call::get(format!("/v1/numbers/{PN_A}")).key(&a_key),
        Call::get("/v1/numbers").key(&platform).tenant(A),
    ] {
        let reply = h.call(request).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "tenant_suspended")
        );
    }
    // Other tenants are unaffected.
    let reply = h
        .call(Call::get("/v1/numbers").key(&platform).tenant(B))
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    h.store
        .update_tenant(&tenant(A), None, Some(TenantStatus::Active))
        .await
        .unwrap();
    assert_eq!(
        h.call(Call::get("/v1/numbers").key(&a_key)).await.status,
        StatusCode::OK
    );
    assert_eq!(h.kv.vault_reads(), reads, "no step reached the vault");
}

/// Step 3, and the order of steps 2, 3 and 4.
#[tokio::test]
async fn the_scope_is_checked_after_the_tenant_and_before_ownership() {
    let h = two_tenants().await;
    let no_numbers = h.tenant_key(B, &[Scope::Send, Scope::Templates]).await;
    let reads = h.kv.vault_reads();
    // Missing scope: 403 forbidden, on B's own number and on A's alike
    // (step 3 before step 4: the answer says nothing about ownership).
    for path in [
        "/v1/numbers".to_owned(),
        format!("/v1/numbers/{PN_B}"),
        format!("/v1/numbers/{PN_A}"),
    ] {
        let reply = h.call(Call::get(&path).key(&no_numbers)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{path}"
        );
    }
    // A suspended tenant is reported before a missing scope (step 2 first).
    h.store
        .update_tenant(&tenant(B), None, Some(TenantStatus::Suspended))
        .await
        .unwrap();
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&no_numbers))
        .await;
    assert_eq!(reply.code(), "tenant_suspended");
    assert_eq!(h.kv.vault_reads(), reads);
}

/// Step 5: no token, an expired one, a number marked after a `190`.
#[tokio::test]
async fn the_vault_decides_last() {
    let h = Harness::new();
    h.tenant(A).await;
    let key = h.tenant_key(A, &[Scope::Numbers]).await;
    // Bound, but no token stored.
    h.store
        .bind_waba(
            &tenant(A),
            &WabaId::new(WABA_A).unwrap(),
            &[PhoneNumberId::new(PN_A).unwrap()],
        )
        .await
        .unwrap();
    for path in [
        format!("/v1/numbers/{PN_A}"),
        format!("/v1/numbers/{PN_A}/profile"),
    ] {
        let reply = h.call(Call::get(&path).key(&key)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::CONFLICT, "number_not_connected"),
            "{path}"
        );
    }
    let reply = h
        .call(Call::new(Method::DELETE, format!("/v1/wabas/{WABA_A}")).key(&key))
        .await;
    assert_eq!(reply.code(), "number_not_connected");

    // An expired token.
    h.vault
        .store(
            &meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken::new(
                WabaId::new(WABA_A).unwrap(),
                meta_whatsapp_rs::core::secret::AccessToken::new("EXPIRED"),
            )
            .phone_number_ids([PhoneNumberId::new(PN_A).unwrap()])
            .expires_at(time::OffsetDateTime::now_utc() - time::Duration::hours(1)),
        )
        .await
        .unwrap();
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "reconnect_required")
    );

    // A number marked reconnect_required: refused before the vault is read.
    h.connect(A, WABA_A, &[PN_A], "TOKEN-OF-A").await;
    h.store
        .set_waba_status(
            &WabaId::new(WABA_A).unwrap(),
            NumberStatus::ReconnectRequired,
        )
        .await
        .unwrap();
    let before = h.kv.vault_reads();
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&key))
        .await;
    assert_eq!(reply.code(), "reconnect_required");
    assert_eq!(h.kv.vault_reads(), before);
    assert!(h.graph.requests().is_empty());
}

/// A `190` on a merchant's call marks its numbers: the next call is `409
/// reconnect_required` without asking Meta.
#[tokio::test]
async fn a_190_marks_the_numbers_reconnect_required() {
    let h = two_tenants().await;
    let key = h.tenant_key(A, &[Scope::Numbers]).await;
    // reference/whatsapp-business-phone-number/whatsapp-business-account-phone-number-api, 401 example.
    h.graph.push_json(
        401,
        json!({"error": {"message": "Invalid OAuth access token", "type": "OAuthException",
                         "code": 190, "error_subcode": 463, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "reconnect_required")
    );
    assert_eq!(reply.json()["error"]["graph"]["code"], 190);
    let number = h
        .store
        .number(&PhoneNumberId::new(PN_A).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(number.status, NumberStatus::ReconnectRequired);
    let listed = h.call(Call::get("/v1/numbers").key(&key)).await.json();
    assert_eq!(listed["data"][0]["status"], "reconnect_required");
    let again = h
        .call(Call::get(format!("/v1/numbers/{PN_A}/profile")).key(&key))
        .await;
    assert_eq!(again.code(), "reconnect_required");
    assert_eq!(
        h.graph.requests().len(),
        1,
        "the second call never reached Meta"
    );
    assert_eq!(h.graph.remaining(), 0);
    // B's number is untouched.
    let b = h
        .store
        .number(&PhoneNumberId::new(PN_B).unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b.status, NumberStatus::Connected);
}

/// Step 5 uses a token only for the WABA the number is bound to: when the
/// vault's phone index names another WABA (another tenant's onboarding
/// listed the number: Embedded Signup stores before it binds), the number
/// is not connected, and that WABA's token never reaches Meta. Decisive:
/// the vault record's WABA compared with the binding's.
#[tokio::test]
async fn a_token_for_another_waba_is_never_used() {
    let h = two_tenants().await;
    let a_key = h.tenant_key(A, &[Scope::Numbers]).await;
    // B's WABA token now indexes A's number too; A's binding is unchanged.
    h.vault
        .store(
            &meta_whatsapp_rs::client::embedded_signup::StoredBusinessToken::new(
                WabaId::new(WABA_B).unwrap(),
                meta_whatsapp_rs::core::secret::AccessToken::new("TOKEN-OF-B"),
            )
            .phone_number_ids([
                PhoneNumberId::new(PN_B).unwrap(),
                PhoneNumberId::new(PN_A).unwrap(),
            ]),
        )
        .await
        .unwrap();
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_A}")).key(&a_key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "number_not_connected"),
        "{}",
        reply.text
    );
    assert!(h.graph.requests().is_empty(), "B's token reached Meta");
}

/// Step 5 for a number marked `disconnected`: `409 number_not_connected`,
/// though its token is still in the vault, and refused before the vault is
/// read, as a number marked `reconnect_required` is. Decisive: the
/// `Disconnected` arm of the number's status in `Authorizer::owned_number`.
#[tokio::test]
async fn a_disconnected_number_is_not_connected_before_the_vault() {
    let h = two_tenants().await;
    let key = h.tenant_key(A, &[Scope::Numbers]).await;
    h.store
        .set_waba_status(&WabaId::new(WABA_A).unwrap(), NumberStatus::Disconnected)
        .await
        .unwrap();
    let before = h.kv.vault_reads();
    for path in [
        format!("/v1/numbers/{PN_A}"),
        format!("/v1/numbers/{PN_A}/profile"),
    ] {
        let reply = h.call(Call::get(&path).key(&key)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::CONFLICT, "number_not_connected"),
            "{path}: {}",
            reply.text
        );
    }
    assert_eq!(h.kv.vault_reads(), before, "the vault was read");
    assert!(h.graph.requests().is_empty(), "A's token reached Meta");
    // B's number, connected, still reaches Meta with B's token.
    let b_key = h.tenant_key(B, &[Scope::Numbers]).await;
    // solution-providers/manage-phone-numbers, as in tests/numbers.rs.
    h.graph.push_json(
        200,
        json!({
            "id": PN_B,
            "display_phone_number": "+1 631-555-1111",
            "verified_name": "John's Cake Shop",
            "quality_rating": "GREEN",
            "name_status": "APPROVED",
            "throughput": {"level": "STANDARD"}
        }),
    );
    let reply = h
        .call(Call::get(format!("/v1/numbers/{PN_B}")).key(&b_key))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.path(), format!("/v25.0/{PN_B}"));
    assert_eq!(request.bearer(), Some("TOKEN-OF-B"));
    assert_eq!(h.graph.remaining(), 0);
}

/// One token reaching two tenants' WABAs: the platform's system user
/// token, attached (`POST /v1/admin/tenants/{id}/wabas`) to a WABA of each.
async fn one_token_two_tenants() -> Harness {
    let h = Harness::new();
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA_A, &[PN_A], "PLATFORM-SYSTEM-TOKEN").await;
    h.connect(B, WABA_B, &[PN_B], "PLATFORM-SYSTEM-TOKEN").await;
    h
}

/// `error` without its request id, to compare two answers.
fn without_request_id(reply: &common::Reply) -> Value {
    let mut body = reply.json();
    body["error"]["request_id"] = Value::Null;
    body
}

/// B's media id on A's number, with a token that reaches both: the lookup
/// and the deletion carry A's `phone_number_id`, so Meta refuses B's
/// media; the refusal is `404 not_found`, the answer for a media id that
/// does not exist, whatever Meta's code, and nothing is downloaded or
/// deleted. Decisive: `phone_number_id` on the lookup and the deletion,
/// and the refusal answered as a missing media id.
#[tokio::test]
async fn another_tenants_media_id_is_not_found() {
    const B_MEDIA: &str = "1037543291543637";
    let h = one_token_two_tenants().await;
    let a_key = h.tenant_key(A, &[Scope::Media]).await;
    // How Meta may refuse a media id that is not the number's: an invalid
    // parameter (100, subcode 33: "does not exist, cannot be loaded due
    // to missing permissions"), a permission error, a 404 of its own.
    let refusals = [
        (
            400,
            json!({"error": {"message": "Unsupported get request. B-MEDIA-SENTINEL", "type": "GraphMethodException",
                             "code": 100, "error_subcode": 33, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
        ),
        (
            403,
            json!({"error": {"message": "B-MEDIA-SENTINEL", "type": "OAuthException", "code": 200,
                             "error_data": {"details": "B-MEDIA-SENTINEL belongs to another number"}}}),
        ),
        (
            404,
            json!({"error": {"message": "B-MEDIA-SENTINEL", "type": "GraphMethodException", "code": 803}}),
        ),
    ];
    let mut answers = Vec::new();
    for (status, refusal) in &refusals {
        for method in [Method::GET, Method::DELETE] {
            let asked = h.graph.requests().len();
            h.graph.push_json(*status, refusal.clone());
            let reply = h
                .call(
                    Call::new(
                        method.clone(),
                        format!("/v1/numbers/{PN_A}/media/{B_MEDIA}"),
                    )
                    .key(&a_key),
                )
                .await;
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{method} <- {status}: {}",
                reply.text
            );
            assert!(!reply.text.contains("B-MEDIA-SENTINEL"), "{}", reply.text);
            let requests = h.graph.requests();
            assert_eq!(requests.len(), asked + 1, "{method}: one call, no download");
            // The lookup, for a deletion too: no DELETE is sent.
            let request = requests.last().unwrap();
            assert_eq!(
                (request.method.clone(), request.path().to_owned()),
                (Method::GET, format!("/v25.0/{B_MEDIA}"))
            );
            assert_eq!(
                request.query("phone_number_id").as_deref(),
                Some(PN_A),
                "{method}: Meta is asked for A's number's media only"
            );
            answers.push(without_request_id(&reply));
        }
    }
    // Every refusal reads like a number that does not exist.
    let missing = h
        .call(Call::get("/v1/numbers/999999999/media/1").key(&a_key))
        .await;
    for answer in answers {
        assert_eq!(answer, without_request_id(&missing));
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// B's template id on A's WABA, with a token that reaches both: Meta's
/// template object does not name its WABA, so the id is looked for among
/// A's WABA's templates of its name; not there, it is `404 not_found`,
/// nothing of it is answered, and a deletion by that id deletes nothing.
/// Decisive: the lookup through the WABA's own edge.
#[tokio::test]
async fn another_tenants_template_id_is_not_found() {
    const B_TEMPLATE: &str = "1407680676729942";
    let h = one_token_two_tenants().await;
    let a_key = h.tenant_key(A, &[Scope::Templates]).await;
    // The token reaches B's template: Meta answers its name.
    h.graph
        .push_json(200, json!({"name": "b_private_offer", "id": B_TEMPLATE}));
    // A's WABA has no template of that name.
    h.graph.push_json(200, json!({"data": []}));
    let reply = h
        .call(Call::get(format!("/v1/wabas/{WABA_A}/templates/{B_TEMPLATE}")).key(&a_key))
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        reply.text
    );
    assert!(!reply.text.contains("b_private_offer"), "{}", reply.text);
    let requests = h.graph.requests();
    let [named, listed] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(
        named.query("fields").as_deref(),
        Some("name"),
        "its name only"
    );
    assert_eq!(
        listed.path(),
        format!("/v25.0/{WABA_A}/message_templates"),
        "then A's own templates"
    );
    assert_eq!(listed.query("name").as_deref(), Some("b_private_offer"));
    // B's template shares a name with one of A's: A's is not B's.
    h.graph
        .push_json(200, json!({"name": "order_confirmation", "id": B_TEMPLATE}));
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1407680676729941", "name": "order_confirmation", "language": "en_US"}]}),
    );
    let same_name = h
        .call(Call::get(format!("/v1/wabas/{WABA_A}/templates/{B_TEMPLATE}")).key(&a_key))
        .await;
    assert_eq!(without_request_id(&same_name), without_request_id(&reply));
    // A template id of nobody (Meta: 803) reads the same.
    h.graph.push_json(
        404,
        json!({"error": {"message": "Template not found", "type": "GraphMethodException", "code": 803}}),
    );
    let missing = h
        .call(Call::get(format!("/v1/wabas/{WABA_A}/templates/1407680676729943")).key(&a_key))
        .await;
    assert_eq!(without_request_id(&missing), without_request_id(&reply));
    // A has a template of the same name, with another id: still not B's.
    let before = h.graph.requests().len();
    h.graph.push_json(
        200,
        json!({"data": [{"id": "1407680676729941", "name": "order_confirmation"}]}),
    );
    let deleted = h
        .call(
            Call::new(
                Method::DELETE,
                format!("/v1/wabas/{WABA_A}/templates?name=order_confirmation&id={B_TEMPLATE}"),
            )
            .key(&a_key),
        )
        .await;
    assert_eq!(
        (deleted.status, deleted.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        deleted.text
    );
    let requests = h.graph.requests();
    assert_eq!(requests.len(), before + 1, "no DELETE reached Meta");
    assert_eq!(requests.last().unwrap().method, Method::GET);
    assert_eq!(
        requests.last().unwrap().path(),
        format!("/v25.0/{WABA_A}/message_templates")
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// The calls on an object named by id (a media id, a template id), with
/// the Graph request Meta's scripted failure answers: `(label, method,
/// path, Meta's answers before the one that fails)`.
fn object_calls() -> Vec<(&'static str, Method, String, Vec<Value>)> {
    const MEDIA: &str = "1037543291543636";
    let media_info = json!({"messaging_product": "whatsapp", "id": MEDIA, "mime_type": "image/jpeg",
        "sha256": "3f9d94d399fa61c191bc1d4ca71375a035cd9b9f5b1128e1f0963a415c16b0cc",
        "url": "https://lookaside.fbsbx.com/whatsapp_business/attachments/?mid=1037543291543636"});
    let media = format!("/v1/numbers/{PN_A}/media/{MEDIA}");
    vec![
        ("download: the lookup", Method::GET, media.clone(), vec![]),
        ("delete: the lookup", Method::DELETE, media.clone(), vec![]),
        (
            "delete: the DELETE",
            Method::DELETE,
            media,
            vec![media_info],
        ),
        (
            "template: its name",
            Method::GET,
            format!("/v1/wabas/{WABA_A}/templates/1407680676729941"),
            vec![],
        ),
    ]
}

/// Only Meta refusing the object is `404 not_found` on a route naming an
/// object by id: the token's own failure (`190`) is `409
/// reconnect_required` and marks the WABA's numbers (section 5.2: Meta's
/// `authentication` on a stored token; the design keeps `404` for an
/// object that is not there, and a dead token says nothing about it),
/// and Meta failing (a 5xx, whatever
/// its code, even one the library does not know) is Meta's failure,
/// `502`, with its code, never taken for a missing object. A plain 4xx
/// without a Graph error (`404` from a proxy of Meta's, say) refuses the
/// object: `404`, without its body. Decisive: the token's failure and
/// the 5xx kept out of the refusals, and the plain 4xx kept in.
#[tokio::test]
async fn only_metas_refusal_of_the_object_is_not_found() {
    // reference/whatsapp-business-phone-number/whatsapp-business-account-phone-number-api, 401 example.
    let expired = json!({"error": {"message": "Error validating access token: Session has expired",
        "type": "OAuthException", "code": 190, "error_subcode": 463, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}});
    // A code the library does not map (`ErrorKind::Unknown`), on a 5xx.
    let failed = json!({"error": {"message": "An unexpected error SENTINEL-5XX", "type": "OAuthException",
        "code": 987654, "is_transient": true, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}});
    for (label, method, path, before) in object_calls() {
        let call = |key: &str| Call::new(method.clone(), &path).key(key);
        // 190: reconnect_required, the numbers marked, Meta not asked again.
        let h = two_tenants().await;
        let key = h.tenant_key(A, &[Scope::Media, Scope::Templates]).await;
        for answer in &before {
            h.graph.push_json(200, answer.clone());
        }
        h.graph.push_json(401, expired.clone());
        let reply = h.call(call(&key)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::CONFLICT, "reconnect_required"),
            "{label}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["graph"]["code"], 190, "{label}");
        let number = h
            .store
            .number(&PhoneNumberId::new(PN_A).unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(number.status, NumberStatus::ReconnectRequired, "{label}");
        // A number route checks the status first (a WABA route has
        // none of its own: step 5 is the number's).
        if path.starts_with("/v1/numbers/") {
            let asked = h.graph.requests().len();
            let again = h.call(call(&key)).await;
            assert_eq!(again.code(), "reconnect_required", "{label}");
            assert_eq!(h.graph.requests().len(), asked, "{label}: Meta not asked");
        }
        assert_eq!(h.graph.remaining(), 0, "{label}");

        // A 5xx with a code the library does not know: 502 `unknown`.
        let h = two_tenants().await;
        let key = h.tenant_key(A, &[Scope::Media, Scope::Templates]).await;
        for answer in &before {
            h.graph.push_json(200, answer.clone());
        }
        h.graph.push_json(500, failed.clone());
        let reply = h.call(call(&key)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::BAD_GATEWAY, "unknown"),
            "{label}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["graph"]["code"], 987_654, "{label}");
        assert!(!reply.text.contains("SENTINEL-5XX"), "{label}");
        assert_eq!(h.graph.remaining(), 0, "{label}");

        // A plain 4xx, no Graph error in it: the object refused, 404.
        for status in [400, 403, 404] {
            let h = two_tenants().await;
            let key = h.tenant_key(A, &[Scope::Media, Scope::Templates]).await;
            for answer in &before {
                h.graph.push_json(200, answer.clone());
            }
            h.graph.push_bytes(
                status,
                "text/html",
                b"<html><body>SENTINEL-PLAIN Not Found</body></html>".to_vec(),
            );
            let reply = h.call(call(&key)).await;
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::NOT_FOUND, "not_found"),
                "{label} <- plain {status}: {}",
                reply.text
            );
            assert!(reply.json()["error"]["graph"].is_null(), "{label}");
            assert!(!reply.text.contains("SENTINEL-PLAIN"), "{label}");
            assert_eq!(h.graph.remaining(), 0, "{label}");
        }
    }
}

/// Roadmap M3f: the hosted claim and the pool's routes are the operator's
/// (decision D202), and M1.3's table (which walks the committed document)
/// holds them: `every_keyed_operation_needs_a_key_of_its_kind_before_the_body`
/// covers each of them by construction (step 1, the key's kind, before
/// the body). Here, with the deployment's settings on: no tenant key
/// claims a hosted arrival (the WABA and business ids are no secret, so a
/// tenant naming both would take another tenant's merchant), whatever its
/// scopes, a platform key included, and the arrival and Meta are left
/// alone; the operator naming a tenant for a WABA another holds is `409`
/// before the arrival is touched or Meta asked (D4), and never reads the
/// vault; the pool's routes are the admin key's, a tenant's (platform
/// keys included) `403`, Meta never asked.
/// Decisive: the admin guard removed from the claim (the first
/// assertion) or from a pool route (the last), D4 after the claim (the
/// second).
#[tokio::test]
async fn the_hosted_and_pool_routes_follow_the_authorization_order() {
    use common::signup::{BUSINESS, HOSTED_TOKEN, POOL_BUSINESS, POOL_TOKEN, partner_added};
    use meta_whatsapp_rs::core::ids::BusinessId;
    use meta_whatsapp_rs::core::secret::AccessToken;
    let h = Harness::with_settings(common::signup::settings(
        common::signup::tech_provider()
            .hosted_signup(AccessToken::new(HOSTED_TOKEN))
            .pre_verified_pool(
                BusinessId::new(POOL_BUSINESS).unwrap(),
                AccessToken::new(POOL_TOKEN),
            ),
    ));
    h.tenant(A).await;
    h.tenant(B).await;
    h.connect(A, WABA_A, &[PN_A], "TOKEN-OF-A").await;
    let hosted_body = json!({"tenant_id": B, "waba_id": WABA_A, "owner_business_id": BUSINESS});
    h.webhook(&partner_added(WABA_A, BUSINESS)).await;
    let reads = h.kv.vault_reads();

    // Step 1: no tenant key claims, with the scope or without it.
    let no_scope = h.tenant_key(B, &[Scope::Send]).await;
    let signup = h.tenant_key(B, &[Scope::Signup]).await;
    let platform = h.platform_key(AllowedTenants::All, &ALL_SCOPES).await;
    for key in [&no_scope, &signup, &platform] {
        let reply = h
            .call(
                Call::new(Method::POST, "/v1/admin/signup/hosted")
                    .key(key)
                    .tenant(B)
                    .json(&hosted_body),
            )
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "forbidden"),
            "{}",
            reply.text
        );
    }

    // D4: the operator names B for A's WABA.
    let admin = h.admin_key().await;
    let reply = h
        .call(
            Call::new(Method::POST, "/v1/admin/signup/hosted")
                .key(&admin)
                .json(&hosted_body),
        )
        .await;
    assert_eq!(
        (reply.status, reply.code().as_str()),
        (StatusCode::CONFLICT, "waba_owned_by_another_tenant"),
        "{}",
        reply.text
    );
    assert_eq!(h.kv.vault_reads(), reads, "the vault was never read");
    assert!(h.graph.requests().is_empty(), "Meta was never asked");

    // The pool: the operator's only.
    for key in [&signup, &platform] {
        for (method, path, body) in [
            (Method::GET, "/v1/admin/pre-verified-numbers", json!({})),
            (
                Method::POST,
                "/v1/admin/pre-verified-numbers",
                json!({"phone_number": "+15550783881"}),
            ),
            (
                Method::PUT,
                "/v1/admin/pre-verified-numbers/1972385232742999/reservation",
                json!({"tenant_id": B}),
            ),
        ] {
            let reply = h
                .call(
                    Call::new(method.clone(), path)
                        .key(key)
                        .tenant(B)
                        .json(&body),
                )
                .await;
            assert_eq!(
                (reply.status, reply.code().as_str()),
                (StatusCode::FORBIDDEN, "forbidden"),
                "{method} {path}: {}",
                reply.text
            );
        }
    }
    assert!(h.graph.requests().is_empty(), "Meta was never asked");
}
