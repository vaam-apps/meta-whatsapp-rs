//! Roadmap M5j2 (design D255, D290): the operator's creating POSTs, the
//! business verification submission and the solution creation, take an
//! `Idempotency-Key`, scoped to the **admin key** in a namespace no tenant
//! can be named (`admin/<key id>`).
//!
//! The decisive tests: the same key and body after a `504` is one request
//! to Meta, a different body with the key is `422 idempotency_key_reused`
//! (a submission is capped at three per customer, so a client's retry could
//! burn one); an admin key's record is not a tenant's or another admin
//! key's, with the same key string; the gate and the authorization answer
//! before the key is read; the records hold the answer, never the
//! documents.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use common::signup::{POOL_BUSINESS, POOL_TOKEN, tech_provider};
use common::{Call, Harness, Stores};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::BusinessId;
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::idempotency::Fingerprint;
use meta_whatsapp_server::model::{IdempotencyClaim, IdempotencyKey, Scope, TenantId};
use meta_whatsapp_server::state::Settings;
use meta_whatsapp_server::store::LocalNotifier;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
const SOLUTION: &str = "795033096057724";
const CUSTOMER: &str = "2729063490586005";
const DESTINATION_SOLUTION: &str = "303610109049230";
const VERIFICATIONS: &str = "/v1/admin/partners/business-verifications";
const SOLUTIONS: &str = "/v1/admin/partners/solutions";

const PDF: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n1 0 obj\n";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

struct Fixture {
    h: Harness,
    admin: String,
}

async fn fixture(legal_acts: bool) -> Fixture {
    let h = Harness::with_settings(Settings {
        signup: Some(tech_provider().pre_verified_pool(
            BusinessId::new(POOL_BUSINESS).unwrap(),
            AccessToken::new(POOL_TOKEN),
        )),
        legal_acts,
        ..common::test_settings()
    });
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let admin = h.admin_key().await;
    Fixture { h, admin }
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn submission(documents: &[(&str, &[u8])]) -> Value {
    json!({
        "client_business_id": CUSTOMER,
        "documents": documents
            .iter()
            .map(|(name, data)| json!({"file_name": name, "content_base64": b64(data)}))
            .collect::<Vec<_>>(),
    })
}

fn solution_body(name: &str) -> Value {
    json!({
        "name": name,
        "partner_app_id": "21202248997039",
        "owner_permissions": ["MESSAGING"],
        "partner_permissions": ["MESSAGING"]
    })
}

fn keyed(path: &str, key_of_caller: &str, key: &str, body: &Value) -> Call {
    Call::new(Method::POST, path)
        .key(key_of_caller)
        .header("idempotency-key", key)
        .json(body)
}

fn received() -> Value {
    json!({"success": true,
           "message": "Your request has been received and will be reviewed shortly.",
           "verification_attempts": 1})
}

/// The roadmap's decisive test. A submission whose answer was a `504` is
/// kept: the same key and body is one request to Meta, however often it is
/// repeated; a different body with the key is `422 idempotency_key_reused`
/// and Meta is not asked; the same body under another key is another
/// submission. Decisive: the key (ignored, the repeat reaches Meta), the
/// body in the fingerprint (dropped, the other body replays the `504`).
#[tokio::test]
async fn a_submission_retried_after_a_504_is_one_request_to_meta() {
    let f = fixture(true).await;
    let body = submission(&[("statement.pdf", PDF), ("registration.png", PNG)]);
    f.h.graph.push_error(|| TransportError::Timeout);
    let first =
        f.h.call(keyed(VERIFICATIONS, &f.admin, "verify-1", &body))
            .await;
    assert_eq!(first.status, StatusCode::GATEWAY_TIMEOUT, "{}", first.text);
    assert_eq!(first.json()["error"]["may_have_been_sent"], true);
    assert_eq!(f.h.graph.requests().len(), 1);

    for _ in 0..2 {
        let again =
            f.h.call(keyed(VERIFICATIONS, &f.admin, "verify-1", &body))
                .await;
        assert_eq!(again.status, StatusCode::GATEWAY_TIMEOUT, "{}", again.text);
        assert_eq!(again.headers["idempotent-replayed"], "true");
        assert_eq!(again.text, first.text, "byte for byte");
    }
    assert_eq!(f.h.graph.requests().len(), 1, "one request, not three");

    let other = submission(&[("statement.pdf", PDF)]);
    let reused =
        f.h.call(keyed(VERIFICATIONS, &f.admin, "verify-1", &other))
            .await;
    assert_eq!(
        (reused.status, reused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused"),
        "{}",
        reused.text
    );
    assert_eq!(f.h.graph.requests().len(), 1, "Meta was not asked");

    f.h.graph.push_json(200, received());
    let fresh =
        f.h.call(keyed(VERIFICATIONS, &f.admin, "verify-2", &body))
            .await;
    assert_eq!(fresh.status, StatusCode::CREATED, "{}", fresh.text);
    assert_eq!(f.h.graph.requests().len(), 2);
    assert_eq!(f.h.graph.remaining(), 0);
}

/// A submission that succeeded is kept byte for byte: the receipt, with
/// `Idempotent-Replayed: true`, and the records hold nothing of the
/// documents or of the customer's id (the replayed answer is the
/// receipt's two counts). One request. A request without a key is not
/// kept: it is sent each time, as before.
#[tokio::test]
async fn a_kept_submission_is_the_receipt_and_a_request_without_a_key_is_not_kept() {
    let f = fixture(true).await;
    let body = submission(&[("statement.pdf", PDF)]);
    f.h.graph.push_json(200, received());
    let first =
        f.h.call(keyed(VERIFICATIONS, &f.admin, "verify-ok", &body))
            .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    assert_eq!(
        first.json(),
        json!({"verification_attempts": 1, "attempts_left": 2})
    );
    let again =
        f.h.call(keyed(VERIFICATIONS, &f.admin, "verify-ok", &body))
            .await;
    assert_eq!(again.text, first.text);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert!(!again.text.contains(&b64(PDF)));
    assert!(!again.text.contains(CUSTOMER));
    assert_eq!(f.h.graph.requests().len(), 1);

    for _ in 0..2 {
        f.h.graph.push_json(200, received());
        let plain =
            f.h.call(
                Call::new(Method::POST, VERIFICATIONS)
                    .key(&f.admin)
                    .json(&body),
            )
            .await;
        assert_eq!(plain.status, StatusCode::CREATED, "{}", plain.text);
        assert!(!plain.headers.contains_key("idempotent-replayed"));
    }
    assert_eq!(f.h.graph.requests().len(), 3);
    assert_eq!(f.h.graph.remaining(), 0);
}

/// A submission that cannot pass (nothing sent) never keeps its key: the
/// refusal is released, so a corrected body may use it.
#[tokio::test]
async fn a_refused_submission_releases_its_key() {
    let f = fixture(true).await;
    let bad = submission(&[("notes.txt", PDF)]);
    let refused =
        f.h.call(keyed(VERIFICATIONS, &f.admin, "verify-fix", &bad))
            .await;
    assert_eq!(
        refused.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        refused.text
    );
    assert!(f.h.graph.requests().is_empty());
    f.h.graph.push_json(200, received());
    let fixed =
        f.h.call(keyed(
            VERIFICATIONS,
            &f.admin,
            "verify-fix",
            &submission(&[("statement.pdf", PDF)]),
        ))
        .await;
    assert_eq!(fixed.status, StatusCode::CREATED, "{}", fixed.text);
    assert_eq!(f.h.graph.requests().len(), 1);
}

/// The same for the solution creation, which needs no legal act: a retry
/// after a `504` does not invite the partner twice; a different body with
/// the key is `422 idempotency_key_reused`; success is kept and replayed.
#[tokio::test]
async fn a_solution_creation_retried_after_a_504_is_one_request_to_meta() {
    let f = fixture(false).await;
    let body = solution_body("Social OVD with Lucky Shrub");
    f.h.graph.push_error(|| TransportError::Timeout);
    let first =
        f.h.call(keyed(SOLUTIONS, &f.admin, "solution-1", &body))
            .await;
    assert_eq!(first.status, StatusCode::GATEWAY_TIMEOUT, "{}", first.text);
    let again =
        f.h.call(keyed(SOLUTIONS, &f.admin, "solution-1", &body))
            .await;
    assert_eq!(again.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, first.text);
    assert_eq!(f.h.graph.requests().len(), 1);

    let reused =
        f.h.call(keyed(
            SOLUTIONS,
            &f.admin,
            "solution-1",
            &solution_body("Another name"),
        ))
        .await;
    assert_eq!(
        (reused.status, reused.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused"),
        "{}",
        reused.text
    );
    assert_eq!(f.h.graph.requests().len(), 1);

    f.h.graph.push_json(200, json!({"solution_id": SOLUTION}));
    let created =
        f.h.call(keyed(SOLUTIONS, &f.admin, "solution-2", &body))
            .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    let replayed =
        f.h.call(keyed(SOLUTIONS, &f.admin, "solution-2", &body))
            .await;
    assert_eq!(replayed.text, created.text);
    assert_eq!(replayed.headers["idempotent-replayed"], "true");
    assert_eq!(f.h.graph.requests().len(), 2);
    assert_eq!(f.h.graph.remaining(), 0);
}

/// An admin key's record is not a tenant's, and a tenant's is not an admin
/// key's, with the same key string; two admin keys do not share theirs
/// either. Decisive: the admin key id in the records' scope (dropped, the
/// second admin key replays the first's answer), and the namespace itself
/// (a tenant claiming the string finds it free).
#[tokio::test]
async fn an_admin_keys_records_are_neither_a_tenants_nor_another_admin_keys() {
    let f = fixture(true).await;
    let tenant_key = f.h.tenant_key(TENANT, &[Scope::Partners]).await;
    let other_admin = f.h.admin_key().await;
    let body = solution_body("Social OVD with Lucky Shrub");

    // The admin's record, under "shared".
    f.h.graph.push_json(200, json!({"solution_id": SOLUTION}));
    let first = f.h.call(keyed(SOLUTIONS, &f.admin, "shared", &body)).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);

    // A tenant claiming "shared" finds it free, with the very fingerprint
    // the admin's request stored (so only the scope can tell them apart).
    let records = f.h.backend.idempotency();
    let tenant = TenantId::parse(TENANT).unwrap();
    let shared = IdempotencyKey::parse("shared").unwrap();
    let fingerprint = Fingerprint::create_partner_solution(&body);
    let claimed = records
        .claim_idempotency_key(
            &tenant,
            &shared,
            fingerprint.as_bytes(),
            "probe",
            Duration::from_secs(60),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(claimed, IdempotencyClaim::Claimed, "the tenant saw it");
    assert!(
        records
            .release_idempotency_key(&tenant, &shared, "probe")
            .await
            .unwrap()
    );

    // Another admin key does not see it: its request reaches Meta.
    f.h.graph
        .push_json(200, json!({"solution_id": "795033096057725"}));
    let second =
        f.h.call(keyed(SOLUTIONS, &other_admin, "shared", &body))
            .await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.text);
    assert!(!second.headers.contains_key("idempotent-replayed"));
    assert_eq!(second.json()["solution_id"], "795033096057725");
    assert_eq!(f.h.graph.requests().len(), 2);

    // The first admin key's record is intact.
    let again = f.h.call(keyed(SOLUTIONS, &f.admin, "shared", &body)).await;
    assert_eq!(again.text, first.text);
    assert_eq!(again.headers["idempotent-replayed"], "true");

    // And the other way round: a tenant's record under "tenant-shared" is
    // not the admin's, whatever the fingerprints.
    f.h.graph.push_json(200, json!({"id": "1972385232742999"}));
    let intent =
        f.h.call(keyed(
            &format!("/v1/wabas/{WABA}/solution-migration-intents"),
            &tenant_key,
            "tenant-shared",
            &json!({"solution_id": DESTINATION_SOLUTION}),
        ))
        .await;
    assert_eq!(intent.status, StatusCode::CREATED, "{}", intent.text);
    f.h.graph.push_json(200, json!({"solution_id": SOLUTION}));
    let admin =
        f.h.call(keyed(SOLUTIONS, &f.admin, "tenant-shared", &body))
            .await;
    assert_eq!(admin.status, StatusCode::CREATED, "{}", admin.text);
    assert!(!admin.headers.contains_key("idempotent-replayed"));
    assert_eq!(f.h.graph.requests().len(), 4);
    assert_eq!(f.h.graph.remaining(), 0);
}

/// No tenant can be named like an admin key's namespace: a tenant id never
/// holds a `/`, so `admin/<key id>` is a name only the engine writes.
#[test]
fn no_tenant_can_be_named_like_the_admin_namespace() {
    for name in ["admin/wak_abc", "admin/", "/", "a/b"] {
        assert_eq!(TenantId::parse(name), None, "{name}");
    }
}

/// The authorization, then the gate, then the key, then the body: with the
/// legal acts off a submission is `403 feature_not_available` whatever its
/// key is (malformed or well formed) and nothing is claimed or sent; a
/// tenant's key is `403 forbidden` and none is `401`, before the key; with
/// the setting on a malformed key is `422` on `Idempotency-Key` before the
/// body is read. Decisive: the order of the extractors.
#[tokio::test]
async fn the_gate_and_the_authorization_answer_before_the_key_is_read() {
    let off = fixture(false).await;
    let tenant_key = off.h.tenant_key(TENANT, &[Scope::Partners]).await;
    for key in [
        "has a space".to_owned(),
        "k".repeat(300),
        "ok-key".to_owned(),
    ] {
        let gated = off
            .h
            .call(keyed(
                VERIFICATIONS,
                &off.admin,
                &key,
                &submission(&[("a.pdf", PDF)]),
            ))
            .await;
        assert_eq!(
            (gated.status, gated.code().as_str()),
            (StatusCode::FORBIDDEN, "feature_not_available"),
            "{key}: {}",
            gated.text
        );
    }
    let forbidden = off
        .h
        .call(keyed(VERIFICATIONS, &tenant_key, "ok-key", &json!({})))
        .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);
    assert_ne!(forbidden.code(), "feature_not_available");
    let anonymous = off
        .h
        .call(
            Call::new(Method::POST, VERIFICATIONS)
                .header("idempotency-key", "ok-key")
                .json(&json!({})),
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert!(off.h.graph.requests().is_empty());

    let on = fixture(true).await;
    let malformed =
        on.h.call(keyed(
            VERIFICATIONS,
            &on.admin,
            "has a space",
            &json!("not a body"),
        ))
        .await;
    assert_eq!(
        (
            malformed.status,
            malformed.json()["error"]["field"].as_str()
        ),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("Idempotency-Key")),
        "{}",
        malformed.text
    );
    assert!(on.h.graph.requests().is_empty());
}

/// Two submissions racing under one key are one request to Meta: the second
/// finds the record, so it is the kept answer or `409 idempotency_in_progress`
/// (never a second submission, which would burn another of the customer's
/// three). Decisive: the claim (ignored, both reach Meta).
#[tokio::test]
async fn two_racing_submissions_with_one_key_are_one_request_to_meta() {
    for (path, body) in [
        (VERIFICATIONS, submission(&[("statement.pdf", PDF)])),
        (SOLUTIONS, solution_body("Social OVD with Lucky Shrub")),
    ] {
        let f = fixture(true).await;
        f.h.graph.push_json(
            200,
            if path == SOLUTIONS {
                json!({"solution_id": SOLUTION})
            } else {
                received()
            },
        );
        let (a, b) = tokio::join!(
            f.h.call(keyed(path, &f.admin, "race-1", &body)),
            f.h.call(keyed(path, &f.admin, "race-1", &body)),
        );
        assert_eq!(f.h.graph.requests().len(), 1, "{path}: one request to Meta");
        let mut statuses = [a.status, b.status];
        statuses.sort();
        assert!(
            statuses == [StatusCode::CREATED, StatusCode::CREATED]
                || statuses == [StatusCode::CREATED, StatusCode::CONFLICT],
            "{path}: {statuses:?}: {} / {}",
            a.text,
            b.text
        );
        for reply in [&a, &b] {
            if reply.status == StatusCode::CONFLICT {
                assert_eq!(reply.code(), "idempotency_in_progress", "{}", reply.text);
            }
        }
        assert_eq!(f.h.graph.remaining(), 0);
    }
}

/// A refusal answers with the field it names, never a document's content, name
/// or the customer's id.
#[tokio::test]
async fn a_refused_submission_echoes_no_document() {
    let f = fixture(true).await;
    let body = submission(&[("secret-statement-name.pdf", PNG)]);
    let reply =
        f.h.call(keyed(VERIFICATIONS, &f.admin, "verify-leak", &body))
            .await;
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        reply.text
    );
    assert!(!reply.text.contains(&b64(PNG)));
    assert!(!reply.text.contains("secret-statement-name"));
    assert!(!reply.text.contains(CUSTOMER));
}

/// With the legal acts off a keyed submission claims nothing: the same
/// stores served by a replica with the setting on (the operator turned it
/// on and retried) send the request to Meta instead of replaying a refusal
/// or finding the key busy. Decisive: the gate before the key is claimed
/// (moved after, the first `403` is kept or the key stays in progress).
#[tokio::test]
async fn a_gated_submission_claims_no_key() {
    let off = fixture(false).await;
    let body = submission(&[("statement.pdf", PDF)]);
    let gated = off
        .h
        .call(keyed(VERIFICATIONS, &off.admin, "gated-then-on", &body))
        .await;
    assert_eq!(
        (gated.status, gated.code().as_str()),
        (StatusCode::FORBIDDEN, "feature_not_available"),
        "{}",
        gated.text
    );
    assert!(off.h.graph.requests().is_empty());

    // The same stores (so the same records and the same admin key), the
    // setting on.
    let stores = Stores {
        store: off.h.store.clone(),
        kv: off.h.kv.clone(),
        conversations: off.h.conversations.clone(),
        events: off.h.outbox.clone(),
        notifier: Arc::new(LocalNotifier::new()),
        webhooks: off.h.backend.webhooks(),
    };
    let on = Harness::with_stores_and_settings(
        stores,
        Settings {
            signup: Some(tech_provider().pre_verified_pool(
                BusinessId::new(POOL_BUSINESS).unwrap(),
                AccessToken::new(POOL_TOKEN),
            )),
            legal_acts: true,
            ..common::test_settings()
        },
    );
    on.graph.push_json(200, received());
    let retried = on
        .call(keyed(VERIFICATIONS, &off.admin, "gated-then-on", &body))
        .await;
    assert_eq!(retried.status, StatusCode::CREATED, "{}", retried.text);
    assert!(!retried.headers.contains_key("idempotent-replayed"));
    assert_eq!(on.graph.requests().len(), 1, "the retry reached Meta");
    assert_eq!(on.graph.remaining(), 0);
}
