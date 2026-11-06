//! Roadmap M5j: the partner APIs through the service (docs/design/server.md,
//! section 4.9): the operator's routes (admin key; the deployment's
//! partner business and system token) and the tenant's (scope `partners`,
//! a WABA of its own).
//!
//! Each test sends the documented request and answer of Meta's pages
//! (`solution-providers/partner-led-business-verification`,
//! `solution-providers/multi-partner-solutions`,
//! `solution-providers/support/adding-waba-to-mps`,
//! `reference/user/assigned-whatsapp-business-accounts-api`,
//! `pricing/change-billing-currency`): the exact requests (method, path,
//! query, the token, the body), `remaining() == 0`. The legal acts are off
//! unless a test turns them on (`WA_SERVER_LEGAL_ACTS`, design D192 and
//! D250). M5.1 (another tenant's WABA, zero Meta requests) is
//! `tests/authz.rs`, which iterates every route of the document; the
//! operator's authorization order is its own test there; the logs and the
//! secrets are `tests/logs.rs`'s capture.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use base64::Engine as _;
use common::signup::{APP_ID, POOL_BUSINESS, POOL_TOKEN, tech_provider};
use common::{Call, Harness};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::BusinessId;
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::testing::RecordedBody;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use meta_whatsapp_server::state::Settings;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const OTHER_TENANT: &str = "merchant-43";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const SECOND_WABA: &str = "102290129340400";
const SECOND_PN: &str = "106540352242925";
const OTHER_WABA: &str = "102290129340399";
const OTHER_PN: &str = "106540352242924";
const TOKEN: &str = "EAAG-merchant-token";
const SECOND_TOKEN: &str = "EAAG-second-merchant-token";
const OTHER_TOKEN: &str = "EAAG-other-token";

/// `multi-partner-solutions`, the accept example's solution; the customer
/// of `partner-led-business-verification`.
const SOLUTION: &str = "795033096057724";
const CUSTOMER: &str = "2729063490586005";
/// `solution-providers/support/adding-waba-to-mps`.
const DESTINATION_SOLUTION: &str = "303610109049230";
/// `pricing/change-billing-currency`.
const MIGRATION: &str = "mig_01HZYK3ABCDEF4567890";

const PDF: &[u8] = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n1 0 obj\n";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0\0\x10JFIF\0";

fn settings(legal_acts: bool) -> Settings {
    Settings {
        signup: Some(tech_provider().pre_verified_pool(
            BusinessId::new(POOL_BUSINESS).unwrap(),
            AccessToken::new(POOL_TOKEN),
        )),
        legal_acts,
        ..common::test_settings()
    }
}

struct Fixture {
    h: Harness,
    key: String,
    admin: String,
}

/// Two tenants with a WABA each (the first with a second one), the first
/// tenant's key with the scope `partners`, an admin key.
async fn fixture(legal_acts: bool) -> Fixture {
    let h = Harness::with_settings(settings(legal_acts));
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    h.connect(TENANT, SECOND_WABA, &[SECOND_PN], SECOND_TOKEN)
        .await;
    h.tenant(OTHER_TENANT).await;
    h.connect(OTHER_TENANT, OTHER_WABA, &[OTHER_PN], OTHER_TOKEN)
        .await;
    let key = h.tenant_key(TENANT, &[Scope::Partners]).await;
    let admin = h.admin_key().await;
    Fixture { h, key, admin }
}

fn graph_error(code: i64, message: &str) -> Value {
    json!({"error": {"message": message, "type": "OAuthException", "code": code,
                     "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}})
}

fn admin_call(method: Method, path: &str, f: &Fixture) -> Call {
    Call::new(method, path).key(&f.admin)
}

fn tenant_call(method: Method, path: &str, f: &Fixture) -> Call {
    Call::new(method, path).key(&f.key)
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn submission_body() -> Value {
    json!({
        "client_business_id": CUSTOMER,
        "documents": [
            {"file_name": "wind_and_wool_bank_statement_04302024.pdf", "content_base64": b64(PDF)},
            {"file_name": "registration.png", "content_base64": b64(PNG)},
            {"file_name": "utility_bill.JPG", "content_base64": b64(JPEG)},
        ]
    })
}

/// Every legal act of the operator and of a tenant, as a call.
fn legal_acts(f: &Fixture) -> Vec<(&'static str, bool, Call)> {
    let solution = |tail: &str| format!("/v1/admin/partners/solutions/{SOLUTION}{tail}");
    vec![
        (
            "submit_business_verification",
            true,
            admin_call(Method::POST, "/v1/admin/partners/business-verifications", f)
                .json(&submission_body()),
        ),
        (
            "accept_solution",
            true,
            admin_call(Method::POST, &solution("/accept"), f),
        ),
        (
            "reject_solution",
            true,
            admin_call(Method::POST, &solution("/reject"), f),
        ),
        (
            "accept_solution_deactivation",
            true,
            admin_call(Method::POST, &solution("/deactivation-request/accept"), f),
        ),
        (
            "create_solution_migration_intent",
            false,
            tenant_call(
                Method::POST,
                &format!("/v1/wabas/{WABA}/solution-migration-intents"),
                f,
            )
            .json(&json!({"solution_id": DESTINATION_SOLUTION})),
        ),
        (
            "create_billing_migration",
            false,
            tenant_call(
                Method::POST,
                &format!("/v1/wabas/{WABA}/billing-migrations"),
                f,
            )
            .json(&json!({"currency": "INR"})),
        ),
        (
            "resume_billing_migration",
            false,
            tenant_call(
                Method::POST,
                &format!("/v1/wabas/{WABA}/billing-migrations/{MIGRATION}/resume"),
                f,
            ),
        ),
    ]
}

// ─── The legal acts ──────────────────────────────────────────────────────

/// AGENTS.md, Legal acts; design D192 and D250. With the setting off (its
/// default), each of the seven acts is `403 feature_not_available` and
/// **nothing is asked of Meta**: the four of the operator also read
/// nothing of the vault, and none leaves an audit-worthy trace. Decisive:
/// the arm of `is_legal_partner_act` or of `is_legal_act` for each, and
/// `gate_partner`/`gate_operation` before the first request.
#[tokio::test]
async fn a_legal_act_is_refused_before_any_request_while_the_setting_is_off() {
    let f = fixture(false).await;
    let reads = f.h.kv.vault_reads();
    for (name, admin_act, call) in legal_acts(&f) {
        let reply = f.h.call(call).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::FORBIDDEN, "feature_not_available"),
            "{name}: {}",
            reply.text
        );
        assert!(f.h.graph.requests().is_empty(), "{name}: Meta was asked");
        if admin_act {
            assert_eq!(f.h.kv.vault_reads(), reads, "{name}: the vault was read");
        }
    }
    assert_eq!(f.h.graph.remaining(), 0);
}

/// The acts that are not legal acts reach Meta with the setting off: the
/// reads, a solution's creation, asking for a deactivation and rejecting
/// one. Decisive: an arm added to `is_legal_partner_act` by mistake.
#[tokio::test]
async fn the_other_calls_reach_meta_with_the_setting_off() {
    let f = fixture(false).await;
    let solution = |tail: &str| format!("/v1/admin/partners/solutions/{SOLUTION}{tail}");
    f.h.graph.push_json(200, json!({"solution_id": SOLUTION}));
    let created =
        f.h.call(
            admin_call(Method::POST, "/v1/admin/partners/solutions", &f).json(&json!({
                "name": "Social OVD with Lucky Shrub", "partner_app_id": "21202248997039",
                "owner_permissions": ["MESSAGING"], "partner_permissions": ["MESSAGING"]
            })),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    f.h.graph.push_json(200, json!({"success": true}));
    let asked =
        f.h.call(admin_call(
            Method::POST,
            &solution("/deactivation-request"),
            &f,
        ))
        .await;
    assert_eq!(asked.status, StatusCode::NO_CONTENT, "{}", asked.text);
    f.h.graph.push_json(200, json!({"success": true}));
    let rejected =
        f.h.call(admin_call(
            Method::POST,
            &solution("/deactivation-request/reject"),
            &f,
        ))
        .await;
    assert_eq!(rejected.status, StatusCode::NO_CONTENT, "{}", rejected.text);
    f.h.graph.push_json(
        200,
        json!({"id": WABA, "business_verification_status": "verified"}),
    );
    let status =
        f.h.call(tenant_call(
            Method::GET,
            &format!("/v1/wabas/{WABA}/business-verification"),
            &f,
        ))
        .await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.text);
    assert_eq!(f.h.graph.requests().len(), 4);
    assert_eq!(f.h.graph.remaining(), 0);
}

// ─── Business verification ───────────────────────────────────────────────

/// `partner-led-business-verification`, "Submitting a business for
/// verification": a multipart form (`end_business_id`, then one
/// `business_documents[]` per document, in order, each with the type its
/// name says) to the **partner's** business, with the deployment's system
/// token; the answer is what Meta counted, not its text. Then the list:
/// the documented `GET` with `end_business_id` and the cursor, the
/// customer's id and the rejection reasons as Meta sent them. Decisive:
/// the pool's token and business in `Partners::open`, and the form.
#[tokio::test]
async fn a_submission_is_metas_form_with_the_partners_token_and_the_list_is_read() {
    let f = fixture(true).await;
    f.h.graph.push_json(
        200,
        json!({"success": true,
               "message": "Your request has been received and will be reviewed shortly.",
               "verification_attempts": 1}),
    );
    let reply =
        f.h.call(
            admin_call(
                Method::POST,
                "/v1/admin/partners/business-verifications",
                &f,
            )
            .json(&submission_body()),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"verification_attempts": 1, "attempts_left": 2})
    );
    assert!(
        !reply.text.contains("received"),
        "Meta's text is not answered"
    );
    let sent = f.h.graph.last_request().unwrap();
    assert_eq!(sent.method, Method::POST);
    assert_eq!(
        sent.path(),
        format!("/v25.0/{POOL_BUSINESS}/self_certify_whatsapp_business")
    );
    assert_eq!(sent.url.query(), None);
    assert_eq!(sent.bearer(), Some(POOL_TOKEN));
    let RecordedBody::Multipart(parts) = &sent.body else {
        panic!("multipart expected: {:?}", sent.body)
    };
    let parts: Vec<_> = parts
        .iter()
        .map(|(name, file, content_type, data)| {
            (
                name.as_str(),
                file.as_deref(),
                content_type.as_deref(),
                data.to_vec(),
            )
        })
        .collect();
    assert_eq!(
        parts,
        vec![
            ("end_business_id", None, None, CUSTOMER.as_bytes().to_vec()),
            (
                "business_documents[]",
                Some("wind_and_wool_bank_statement_04302024.pdf"),
                Some("application/pdf"),
                PDF.to_vec()
            ),
            (
                "business_documents[]",
                Some("registration.png"),
                Some("image/png"),
                PNG.to_vec()
            ),
            (
                "business_documents[]",
                Some("utility_bill.JPG"),
                Some("image/jpeg"),
                JPEG.to_vec()
            ),
        ]
    );

    f.h.graph.push_json(
        200,
        json!({"data": [
            {"verification_status": "APPROVED", "submitted_time": "2024-11-04T20:39:21+0000",
             "update_time": "2024-11-04T20:44:02+0000", "client_business_id": CUSTOMER,
             "submitted_info": {"business_vertical": "RETAIL"}, "id": "1066221828388475"},
            {"verification_status": "FAILED",
             "rejection_reasons": ["LEGAL_NAME_NOT_FOUND_IN_DOCUMENTS", "WEBSITE NOT MATCHING"],
             "submitted_time": "2024-11-03T10:00:00+0000", "update_time": "2024-11-03T10:05:00+0000",
             "client_business_id": CUSTOMER, "submitted_info": {}, "id": "1066221828388476"}],
            "paging": {"cursors": {"before": "QVFIUjBEFORE", "after": "QVFIUjAFTER"},
                       "next": "https://graph.facebook.com/v25.0/x?after=QVFIUjAFTER"}}),
    );
    let listed = f
        .h
        .call(admin_call(
            Method::GET,
            &format!(
                "/v1/admin/partners/business-verifications?client_business_id={CUSTOMER}&cursor=QVFIUjCURSOR"
            ),
            &f,
        ))
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(
        listed.json(),
        json!({"data": [
            {"id": "1066221828388475", "status": "APPROVED", "rejection_reasons": [],
             "submitted_time": "2024-11-04T20:39:21+0000",
             "update_time": "2024-11-04T20:44:02+0000", "client_business_id": CUSTOMER},
            {"id": "1066221828388476", "status": "FAILED",
             "rejection_reasons": ["LEGAL_NAME_NOT_FOUND_IN_DOCUMENTS", "WEBSITE NOT MATCHING"],
             "submitted_time": "2024-11-03T10:00:00+0000",
             "update_time": "2024-11-03T10:05:00+0000", "client_business_id": CUSTOMER}],
            "next_cursor": "QVFIUjAFTER"})
    );
    let read = f.h.graph.last_request().unwrap();
    assert_eq!(read.method, Method::GET);
    assert_eq!(
        read.path(),
        format!("/v25.0/{POOL_BUSINESS}/self_certified_whatsapp_business_submissions")
    );
    assert_eq!(read.query("end_business_id").as_deref(), Some(CUSTOMER));
    assert_eq!(read.query("after").as_deref(), Some("QVFIUjCURSOR"));
    assert_eq!(read.bearer(), Some(POOL_TOKEN));
    assert_eq!(f.h.graph.remaining(), 0);
}

/// A submission is checked before anything is sent: no document, four,
/// bytes that are not the type their name says, a type the page does not
/// list, text that is not base64, a customer that is no id. Each names its
/// field and Meta is never asked. Decisive: the library's checks, reached
/// through the service's, and the order (every check before the request).
#[tokio::test]
async fn a_submission_that_cannot_pass_is_refused_before_any_request() {
    let f = fixture(true).await;
    let doc = |name: &str, content: &str| json!({"file_name": name, "content_base64": content});
    let good = doc("a.pdf", &b64(PDF));
    for (body, field) in [
        (
            json!({"client_business_id": CUSTOMER, "documents": []}),
            "documents",
        ),
        (
            json!({"client_business_id": CUSTOMER,
                   "documents": [good, good, good, good]}),
            "documents",
        ),
        (
            json!({"client_business_id": CUSTOMER,
                   "documents": [doc("a.pdf", &b64(PNG))]}),
            "documents",
        ),
        (
            json!({"client_business_id": CUSTOMER,
                   "documents": [doc("notes.txt", &b64(PDF))]}),
            "documents",
        ),
        (
            json!({"client_business_id": CUSTOMER,
                   "documents": [doc("a.pdf", "not base64 !!")]}),
            "documents",
        ),
        (
            json!({"client_business_id": CUSTOMER, "documents": [doc("a.pdf", "")]}),
            "documents",
        ),
        (
            json!({"client_business_id": "not-an-id", "documents": [good]}),
            "client_business_id",
        ),
        (json!({"documents": [good]}), "body"),
    ] {
        let reply =
            f.h.call(
                admin_call(
                    Method::POST,
                    "/v1/admin/partners/business-verifications",
                    &f,
                )
                .json(&body),
            )
            .await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
            "{field}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{}", reply.text);
    }
    assert!(f.h.graph.requests().is_empty());
}

/// A submission counts towards the customer's three, so a timeout is not
/// retried: one request, a `504` that says it may have been sent, and
/// Meta's refusal is its code, never its text (which may quote the
/// business). Decisive: `idempotent(false)` in the library.
#[tokio::test]
async fn a_submission_that_timed_out_is_not_replayed_and_metas_text_is_not_answered() {
    let f = fixture(true).await;
    let call = || {
        admin_call(
            Method::POST,
            "/v1/admin/partners/business-verifications",
            &f,
        )
        .json(&submission_body())
    };
    f.h.graph.push_error(|| TransportError::Timeout);
    let timed_out = f.h.call(call()).await;
    assert_eq!(
        timed_out.status,
        StatusCode::GATEWAY_TIMEOUT,
        "{}",
        timed_out.text
    );
    assert_eq!(timed_out.json()["error"]["may_have_been_sent"], true);
    assert_eq!(f.h.graph.requests().len(), 1);
    f.h.graph.push_json(
        400,
        graph_error(100, "(#100) Invalid parameter SECRET-LEGAL-NAME-9Q"),
    );
    let refused = f.h.call(call()).await;
    assert!(
        !refused.text.contains("SECRET-LEGAL-NAME-9Q"),
        "{}",
        refused.text
    );
    assert_eq!(refused.json()["error"]["graph"]["code"], 100);
    assert_eq!(f.h.graph.remaining(), 0);
}

/// Without the partner business (`WA_PARTNER_BUSINESS_ID`) every route of
/// the operator's is `409 signup_not_configured`, and nothing is asked of
/// Meta.
#[tokio::test]
async fn without_a_partner_business_the_operators_routes_say_so() {
    let h = Harness::with_settings(Settings {
        signup: Some(tech_provider()),
        legal_acts: true,
        ..common::test_settings()
    });
    let admin = h.admin_key().await;
    for (method, path) in [
        (Method::GET, "/v1/admin/partners/business-verifications"),
        (Method::GET, "/v1/admin/partners/solutions"),
        (Method::GET, "/v1/admin/partners/client-businesses"),
        (
            Method::POST,
            "/v1/admin/partners/solutions/795033096057724/accept",
        ),
        (
            Method::GET,
            "/v1/admin/partners/users/1972555232742222/wabas",
        ),
    ] {
        let reply = h.call(Call::new(method.clone(), path).key(&admin)).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::CONFLICT, "signup_not_configured"),
            "{method} {path}: {}",
            reply.text
        );
    }
    assert!(h.graph.requests().is_empty());
}

// ─── Multi-Partner Solutions ─────────────────────────────────────────────

/// `multi-partner-solution-embedded-creation`, step 3: the creation
/// request is Meta's body, from the deployment's app, with the system
/// token; the answer is the new solution. Checked first: a name, an app,
/// task names Meta lists, `MESSAGING` on one side.
#[tokio::test]
async fn a_solution_is_created_as_metas_body_and_checked_first() {
    let f = fixture(false).await;
    f.h.graph.push_json(200, json!({"solution_id": SOLUTION}));
    let reply =
        f.h.call(
            admin_call(Method::POST, "/v1/admin/partners/solutions", &f).json(&json!({
                "name": "Social OVD with Lucky Shrub",
                "partner_app_id": "21202248997039",
                "owner_permissions": ["MESSAGING"],
                "partner_permissions": ["MESSAGING"]
            })),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text);
    assert_eq!(reply.json(), json!({"solution_id": SOLUTION}));
    let sent = f.h.graph.last_request().unwrap();
    assert_eq!(sent.method, Method::POST);
    assert_eq!(
        sent.path(),
        format!("/v25.0/{APP_ID}/whatsapp_business_solution")
    );
    assert_eq!(sent.bearer(), Some(POOL_TOKEN));
    assert_eq!(
        sent.json().unwrap(),
        json!({
            "owner_permissions": ["MESSAGING"],
            "partner_app_id": "21202248997039",
            "partner_permissions": ["MESSAGING"],
            "solution_name": "Social OVD with Lucky Shrub"
        })
    );
    assert_eq!(f.h.graph.remaining(), 0);

    for (body, field) in [
        (
            json!({"name": "x", "partner_app_id": "21202248997039", "partner_permissions": ["MESSAGING"]}),
            "name",
        ),
        (
            json!({"name": "Lucky Shrub", "partner_app_id": "app", "partner_permissions": ["MESSAGING"]}),
            "partner_app_id",
        ),
        (
            json!({"name": "Lucky Shrub", "partner_app_id": "21202248997039",
                   "partner_permissions": ["MESSAGING", "SUPERUSER"]}),
            "partner_permissions",
        ),
        (
            json!({"name": "Lucky Shrub", "partner_app_id": "21202248997039",
                   "owner_permissions": ["SUPERUSER"], "partner_permissions": ["MESSAGING"]}),
            "owner_permissions",
        ),
        (
            json!({"name": "Lucky Shrub", "partner_app_id": "21202248997039",
                   "partner_permissions": ["VIEW_COST"]}),
            "owner_permissions",
        ),
    ] {
        let refused =
            f.h.call(admin_call(Method::POST, "/v1/admin/partners/solutions", &f).json(&body))
                .await;
        assert_eq!(
            (refused.status, refused.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{}",
            refused.text
        );
    }
    assert_eq!(f.h.graph.requests().len(), 1, "only the first was sent");
}

/// The guides' reads: an app's solutions (the documented query), a
/// solution as the guide asks before accepting, its partners and an app's
/// client businesses, each from Meta's example answer.
#[tokio::test]
#[allow(clippy::too_many_lines)] // four reads, each with its request
async fn the_solutions_and_client_businesses_are_read_as_the_guides_do() {
    let f = fixture(false).await;
    f.h.graph.push_json(
        200,
        json!({"data": [
            {"name": "Social OVD with Lucky Shrub", "status": "INITIATED",
             "status_for_pending_request": "PENDING_ACTIVATION", "id": "19702253086782"},
            {"name": "Social OVD with Social Brew", "status": "ACTIVE",
             "status_for_pending_request": "NONE", "id": "17602267745700"}],
            "paging": {"cursors": {"before": "QVFIUkxl", "after": "QVFIUlgy"}, "next": "https://x"}}),
    );
    let listed =
        f.h.call(admin_call(
            Method::GET,
            "/v1/admin/partners/solutions?role=OWNER&limit=25&cursor=QVFIUcur",
            &f,
        ))
        .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(listed.json()["next_cursor"], "QVFIUlgy");
    assert_eq!(
        listed.json()["data"][0],
        json!({"id": "19702253086782", "name": "Social OVD with Lucky Shrub",
               "status": "INITIATED", "status_for_pending_request": "PENDING_ACTIVATION",
               "owner_app_id": null, "owner_permissions": null, "partners": null})
    );
    let read = f.h.graph.last_request().unwrap();
    assert_eq!(
        read.path(),
        format!("/v25.0/{APP_ID}/whatsapp_business_solutions")
    );
    assert_eq!(read.query("role").as_deref(), Some("OWNER"));
    assert_eq!(read.query("limit").as_deref(), Some("25"));
    assert_eq!(read.query("after").as_deref(), Some("QVFIUcur"));
    assert_eq!(
        read.query("fields").as_deref(),
        Some("id,name,status,status_for_pending_request")
    );
    assert_eq!(read.bearer(), Some(POOL_TOKEN));

    f.h.graph.push_json(
        200,
        json!({"name": "Social OVD with Lucky Shrub",
               "owner_permissions": ["MANAGE", "VIEW_INSIGHTS"],
               "partners": {"data": [{"partner_permissions": ["MANAGE", "DEVELOP"],
                   "partner_app": {"link": "https://www.facebook.com/games/?app_id=21202248997039",
                                   "name": "Lucky Shrub", "id": "21202248997039"},
                   "id": "795033099391057"}],
                   "paging": {"cursors": {"before": "b", "after": "a"}}},
               "id": SOLUTION}),
    );
    let solution =
        f.h.call(admin_call(
            Method::GET,
            &format!("/v1/admin/partners/solutions/{SOLUTION}"),
            &f,
        ))
        .await;
    assert_eq!(solution.status, StatusCode::OK, "{}", solution.text);
    assert_eq!(
        solution.json(),
        json!({"id": SOLUTION, "name": "Social OVD with Lucky Shrub", "status": null,
               "status_for_pending_request": null, "owner_app_id": null,
               "owner_permissions": ["MANAGE", "VIEW_INSIGHTS"],
               "partners": [{"id": "795033099391057", "partner_app_id": "21202248997039",
                             "partner_app_name": "Lucky Shrub", "status": null,
                             "partner_permissions": ["MANAGE", "DEVELOP"]}]})
    );
    let read = f.h.graph.last_request().unwrap();
    assert_eq!(read.path(), format!("/v25.0/{SOLUTION}"));
    assert_eq!(
        read.query("fields").as_deref(),
        Some(
            "id,name,status,status_for_pending_request,owner_app,owner_permissions,partners{partner_permissions,partner_app}"
        )
    );

    f.h.graph.push_json(
        200,
        json!({"data": [{"partner_app": {"link": "https://www.socialoverdrive.com",
                                         "name": "Social Overdrive", "id": "637576208107267"},
                         "status": "ACCEPTED", "id": "17602267745704"}],
               "paging": {"cursors": {"before": "b", "after": "a"}}}),
    );
    let partners =
        f.h.call(admin_call(
            Method::GET,
            &format!("/v1/admin/partners/solutions/{SOLUTION}/partners?cursor=QVFIUc"),
            &f,
        ))
        .await;
    assert_eq!(partners.status, StatusCode::OK, "{}", partners.text);
    assert_eq!(partners.json()["data"][0]["status"], "ACCEPTED");
    let read = f.h.graph.last_request().unwrap();
    assert_eq!(read.path(), format!("/v25.0/{SOLUTION}/partners"));
    assert_eq!(read.query("after").as_deref(), Some("QVFIUc"));

    f.h.graph.push_json(
        200,
        json!({"data": [{"id": CUSTOMER, "name": "Lucky Shrub"}],
               "paging": {"cursors": {"before": "b", "after": "a"}}}),
    );
    let clients =
        f.h.call(admin_call(
            Method::GET,
            "/v1/admin/partners/client-businesses?limit=100",
            &f,
        ))
        .await;
    assert_eq!(
        clients.json()["data"],
        json!([{"id": CUSTOMER, "name": "Lucky Shrub"}])
    );
    let read = f.h.graph.last_request().unwrap();
    assert_eq!(
        read.path(),
        format!("/v25.0/{APP_ID}/connected_client_businesses")
    );
    assert_eq!(read.query("limit").as_deref(), Some("100"));
    assert_eq!(f.h.graph.remaining(), 0);

    // A bad id, role, limit or cursor is the request's fault.
    for (path, field) in [
        ("/v1/admin/partners/solutions/not-an-id", "id"),
        ("/v1/admin/partners/solutions?role=ADMIN", "role"),
        ("/v1/admin/partners/solutions?limit=0", "limit"),
        ("/v1/admin/partners/client-businesses?limit=101", "limit"),
        ("/v1/admin/partners/client-businesses?cursor=", "cursor"),
    ] {
        let reply = f.h.call(admin_call(Method::GET, path, &f)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{path}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["field"], field, "{path}");
    }
    assert_eq!(f.h.graph.requests().len(), 4);
}

/// The state changes of a solution: accept, reject, ask for a deactivation,
/// accept one, reject one, each an empty `POST` to its edge with the
/// system token, answered `204` (Meta's `{"success": true}` or a solution),
/// and never repeated after a timeout. Decisive: each edge's name.
#[tokio::test]
async fn a_solutions_state_changes_are_metas_edges_and_are_not_replayed() {
    let f = fixture(true).await;
    for (route, edge) in [
        ("accept", "accept"),
        ("reject", "reject"),
        ("deactivation-request", "send_deactivation_request"),
        ("deactivation-request/accept", "accept_deactivation_request"),
        ("deactivation-request/reject", "reject_deactivation_request"),
    ] {
        // The guide's `{"success": true}`, or a solution (the reference).
        f.h.graph.push_json(
            200,
            if edge.starts_with("accept_deactivation") || edge.starts_with("reject_deactivation") {
                json!({"id": SOLUTION, "status": "ACTIVE"})
            } else {
                json!({"success": true})
            },
        );
        let reply =
            f.h.call(admin_call(
                Method::POST,
                &format!("/v1/admin/partners/solutions/{SOLUTION}/{route}"),
                &f,
            ))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::NO_CONTENT,
            "{route}: {}",
            reply.text
        );
        assert!(reply.text.is_empty());
        let sent = f.h.graph.last_request().unwrap();
        assert_eq!(sent.method, Method::POST, "{route}");
        assert_eq!(sent.path(), format!("/v25.0/{SOLUTION}/{edge}"), "{route}");
        assert_eq!(sent.url.query(), None);
        assert_eq!(sent.bearer(), Some(POOL_TOKEN));
        assert_eq!(sent.json(), None, "no body: {route}");
    }
    assert_eq!(f.h.graph.remaining(), 0);

    // Not repeated after a timeout.
    f.h.graph.push_error(|| TransportError::Timeout);
    let reply =
        f.h.call(admin_call(
            Method::POST,
            &format!("/v1/admin/partners/solutions/{SOLUTION}/accept"),
            &f,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::GATEWAY_TIMEOUT, "{}", reply.text);
    assert_eq!(f.h.graph.requests().len(), 6);
    // Meta's refusal (not `INITIATED`) is its code, not its text.
    f.h.graph.push_json(
        422,
        graph_error(
            100,
            "Solution cannot be accepted: solution is not in INITIATED status",
        ),
    );
    let refused =
        f.h.call(admin_call(
            Method::POST,
            &format!("/v1/admin/partners/solutions/{SOLUTION}/accept"),
            &f,
        ))
        .await;
    assert!(
        !refused.text.contains("INITIATED status"),
        "{}",
        refused.text
    );
    assert_eq!(f.h.graph.remaining(), 0);
}

/// The solution token (`GET /{solution}/access_token`) is a bearer
/// credential for a client's assets: the service has no route for it, and
/// the document names none. A mutation that adds one fails here.
#[test]
fn the_solution_token_is_not_served() {
    for operation in common::spec_operations() {
        assert!(
            !operation.template.contains("access_token")
                && !operation.template.contains("access-token")
                && !(operation.template.contains("/partners/")
                    && operation.template.contains("token")),
            "{}",
            operation.label()
        );
    }
    assert!(
        !common::SPEC.contains("SolutionAccessToken"),
        "no schema for the solution token"
    );
}

/// `reference/user/assigned-whatsapp-business-accounts-api`: the WABAs a
/// system user holds, `id` and `name` (the only fields the reference
/// lists), a page size and a cursor.
#[tokio::test]
async fn a_users_assigned_wabas_are_read_with_the_documented_query() {
    let f = fixture(false).await;
    f.h.graph.push_json(
        200,
        json!({"data": [{"id": "106526625562206", "name": "Lucky Shrub"}, {"id": WABA}],
               "paging": {"cursors": {"before": "QVFI", "after": "QVFJ"}, "next": "https://x"}}),
    );
    let reply =
        f.h.call(admin_call(
            Method::GET,
            "/v1/admin/partners/users/1972555232742222/wabas?limit=100&cursor=QVFIc",
            &f,
        ))
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    assert_eq!(
        reply.json(),
        json!({"data": [{"id": "106526625562206", "name": "Lucky Shrub"},
                        {"id": WABA, "name": null}], "next_cursor": "QVFJ"})
    );
    let read = f.h.graph.last_request().unwrap();
    assert_eq!(read.method, Method::GET);
    assert_eq!(
        read.path(),
        "/v25.0/1972555232742222/assigned_whatsapp_business_accounts"
    );
    assert_eq!(read.query("fields").as_deref(), Some("id,name"));
    assert_eq!(read.query("limit").as_deref(), Some("100"));
    assert_eq!(read.query("after").as_deref(), Some("QVFIc"));
    assert_eq!(read.bearer(), Some(POOL_TOKEN));
    let bad =
        f.h.call(admin_call(
            Method::GET,
            "/v1/admin/partners/users/not-an-id/wabas",
            &f,
        ))
        .await;
    assert_eq!(bad.json()["error"]["field"], "user_id");
    assert_eq!(f.h.graph.requests().len(), 1);
    assert_eq!(f.h.graph.remaining(), 0);
}

// ─── The tenant's WABA ───────────────────────────────────────────────────

/// `whatsapp-business-accounts`: the WABA's `business_verification_status`
/// read with the tenant's own token, the status as Meta sent it and
/// whether it is `verified`. A key without the scope `partners` (a
/// `numbers` key) is `403`; another tenant's WABA is `404` with Meta not
/// asked (M5.2's WABA half).
#[tokio::test]
async fn a_wabas_business_verification_is_read_with_its_own_token() {
    let f = fixture(false).await;
    for (status, verified) in [("verified", true), ("pending", false)] {
        f.h.graph.push_json(
            200,
            json!({"id": WABA, "business_verification_status": status}),
        );
        let reply =
            f.h.call(tenant_call(
                Method::GET,
                &format!("/v1/wabas/{WABA}/business-verification"),
                &f,
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
        assert_eq!(
            reply.json(),
            json!({"status": status, "verified": verified})
        );
        let read = f.h.graph.last_request().unwrap();
        assert_eq!(read.method, Method::GET);
        assert_eq!(read.path(), format!("/v25.0/{WABA}"));
        assert_eq!(
            read.query("fields").as_deref(),
            Some("business_verification_status")
        );
        assert_eq!(read.bearer(), Some(TOKEN));
    }
    // Meta said nothing: not verified.
    f.h.graph.push_json(200, json!({"id": WABA}));
    let reply =
        f.h.call(tenant_call(
            Method::GET,
            &format!("/v1/wabas/{WABA}/business-verification"),
            &f,
        ))
        .await;
    assert_eq!(reply.json(), json!({"status": null, "verified": false}));
    assert_eq!(f.h.graph.remaining(), 0);

    let before = f.h.graph.requests().len();
    let numbers = f.h.tenant_key(TENANT, &[Scope::Numbers]).await;
    let denied =
        f.h.call(Call::get(format!("/v1/wabas/{WABA}/business-verification")).key(&numbers))
            .await;
    assert_eq!(
        (denied.status, denied.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden"),
        "{}",
        denied.text
    );
    let foreign =
        f.h.call(tenant_call(
            Method::GET,
            &format!("/v1/wabas/{OTHER_WABA}/business-verification"),
            &f,
        ))
        .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{}", foreign.text);
    assert_eq!(f.h.graph.requests().len(), before, "Meta was not asked");
}

/// `solution-providers/support/adding-waba-to-mps`, step 1: the intent is
/// `{"solution_id": …}` to the WABA, with the **tenant's** token; the
/// answer is the intent's id. An `Idempotency-Key` makes the same request
/// one request; a timeout is not retried (the client may have been
/// asked). Decisive: `idempotent(false)` and the key.
#[tokio::test]
async fn a_solution_migration_intent_is_metas_body_with_the_tenants_token() {
    let f = fixture(true).await;
    let path = format!("/v1/wabas/{WABA}/solution-migration-intents");
    let create = |key: &str| {
        tenant_call(Method::POST, &path, &f)
            .header("idempotency-key", key)
            .json(&json!({"solution_id": DESTINATION_SOLUTION}))
    };
    f.h.graph.push_json(200, json!({"id": "1972385232742999"}));
    let first = f.h.call(create("intent-1")).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    assert_eq!(first.json(), json!({"id": "1972385232742999"}));
    let sent = f.h.graph.last_request().unwrap();
    assert_eq!(sent.method, Method::POST);
    assert_eq!(
        sent.path(),
        format!("/v25.0/{WABA}/set_solution_migration_intent")
    );
    assert_eq!(sent.bearer(), Some(TOKEN));
    assert_eq!(
        sent.json().unwrap(),
        json!({"solution_id": DESTINATION_SOLUTION})
    );
    let again = f.h.call(create("intent-1")).await;
    assert_eq!(again.text, first.text, "byte for byte");
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(f.h.graph.requests().len(), 1, "one request, not two");

    f.h.graph.push_error(|| TransportError::Timeout);
    let timed_out = f.h.call(create("intent-2")).await;
    assert_eq!(
        timed_out.status,
        StatusCode::GATEWAY_TIMEOUT,
        "{}",
        timed_out.text
    );
    let replayed = f.h.call(create("intent-2")).await;
    assert_eq!(replayed.text, timed_out.text);
    assert_eq!(f.h.graph.requests().len(), 2, "the timeout was not retried");
    assert_eq!(f.h.graph.remaining(), 0);

    for (body, field) in [
        (json!({"solution_id": "not-an-id"}), "solution_id"),
        (json!({}), "body"),
        (
            json!({"solution_id": DESTINATION_SOLUTION, "extra": 1}),
            "body",
        ),
    ] {
        let refused =
            f.h.call(tenant_call(Method::POST, &path, &f).json(&body))
                .await;
        assert_eq!(refused.json()["error"]["field"], field, "{}", refused.text);
    }
    assert_eq!(f.h.graph.requests().len(), 2);
}

/// `adding-waba-to-mps`, step 2, "Get migration intent": the intent is read
/// with the tenant's token, asked with the fields that prove whose it is,
/// and answered only when it names **this** WABA (as its destination or
/// its source). An intent that names another WABA, or none, is `404` and
/// nothing of it is answered (M5.2's WABA half). Decisive: `ours` in
/// `solution_migration_intent`.
#[tokio::test]
async fn a_migration_intent_is_answered_only_when_it_names_the_wabas_own() {
    let f = fixture(false).await;
    let intent = |destination: &str| {
        json!({
            "solution": {"name": "Social OVD with Lucky Shrub", "status": "ACTIVE",
                         "status_for_pending_request": "NONE", "id": DESTINATION_SOLUTION},
            "status": "ACCEPTED",
            "destination_waba": {"id": destination, "name": "SECRET-OTHER-WABA-NAME",
                                 "currency": "USD", "timezone_id": "1", "business_type": "ent",
                                 "message_template_namespace": "ns"},
            "id": "1972385232742999"
        })
    };
    let path = format!("/v1/wabas/{WABA}/solution-migration-intents/1972385232742999");
    f.h.graph.push_json(200, intent(WABA));
    let ours = f.h.call(tenant_call(Method::GET, &path, &f)).await;
    assert_eq!(ours.status, StatusCode::OK, "{}", ours.text);
    assert_eq!(
        ours.json(),
        json!({"id": "1972385232742999", "status": "ACCEPTED",
               "solution_id": DESTINATION_SOLUTION, "solution_name": "Social OVD with Lucky Shrub",
               "destination_waba_id": WABA, "waba_id": null})
    );
    let read = f.h.graph.last_request().unwrap();
    assert_eq!(read.method, Method::GET);
    assert_eq!(read.path(), "/v25.0/1972385232742999");
    assert_eq!(
        read.query("fields").as_deref(),
        Some("id,status,solution,destination_waba,waba")
    );
    assert_eq!(read.bearer(), Some(TOKEN));

    // Another WABA's: not found, and nothing of it answered.
    f.h.graph.push_json(200, intent(OTHER_WABA));
    let foreign = f.h.call(tenant_call(Method::GET, &path, &f)).await;
    assert_eq!(
        (foreign.status, foreign.code().as_str()),
        (StatusCode::NOT_FOUND, "not_found"),
        "{}",
        foreign.text
    );
    assert!(!foreign.text.contains("SECRET-OTHER-WABA-NAME"));
    // One that names no WABA proves nothing: not found.
    f.h.graph.push_json(
        200,
        json!({"status": "INITIATED", "id": "1972385232742999"}),
    );
    let silent = f.h.call(tenant_call(Method::GET, &path, &f)).await;
    assert_eq!(silent.status, StatusCode::NOT_FOUND, "{}", silent.text);
    // The source side counts too.
    f.h.graph.push_json(
        200,
        json!({"status": "INITIATED", "id": "1972385232742999",
               "waba": {"id": WABA}, "destination_waba": {"id": OTHER_WABA}}),
    );
    let source = f.h.call(tenant_call(Method::GET, &path, &f)).await;
    assert_eq!(source.status, StatusCode::OK, "{}", source.text);
    assert_eq!(source.json()["waba_id"], WABA);
    // An id that is no id: not found, Meta not asked.
    let before = f.h.graph.requests().len();
    let bad =
        f.h.call(tenant_call(
            Method::GET,
            &format!("/v1/wabas/{WABA}/solution-migration-intents/not-an-id"),
            &f,
        ))
        .await;
    assert_eq!(bad.status, StatusCode::NOT_FOUND, "{}", bad.text);
    assert_eq!(f.h.graph.requests().len(), before);
    assert_eq!(f.h.graph.remaining(), 0);
}

/// `pricing/change-billing-currency`: the intent is Meta's body (`currency`
/// and the optional `extended_credit_id`) with the tenant's token; the
/// answer is the migration; the id is then **this WABA's**: its status and
/// its resumption are Meta's `GET /{migration}` and `POST
/// /{migration}/resume_migration`, and another WABA's (the tenant's second
/// one, another tenant's) is `404` with **no request to Meta** (M5.2's WABA
/// half). Decisive: the stored record and its check in `own_migration`.
#[tokio::test]
#[allow(clippy::too_many_lines)] // three calls, then each way to a foreign id
async fn a_billing_migration_is_the_wabas_own_to_read_and_to_resume() {
    let f = fixture(true).await;
    let path = format!("/v1/wabas/{WABA}/billing-migrations");
    f.h.graph.push_json(
        200,
        json!({"migration_id": MIGRATION, "migration_status": "INITIATED"}),
    );
    let started =
        f.h.call(
            tenant_call(Method::POST, &path, &f)
                .header("idempotency-key", "migration-1")
                .json(&json!({"currency": "INR", "extended_credit_id": "987654321098765"})),
        )
        .await;
    assert_eq!(started.status, StatusCode::CREATED, "{}", started.text);
    assert_eq!(
        started.json(),
        json!({"id": MIGRATION, "status": "INITIATED",
               "destination_waba_id": null, "destination_currency": null})
    );
    let sent = f.h.graph.last_request().unwrap();
    assert_eq!(sent.method, Method::POST);
    assert_eq!(
        sent.path(),
        format!("/v25.0/{WABA}/set_payment_method_migration_intent")
    );
    assert_eq!(sent.bearer(), Some(TOKEN));
    assert_eq!(
        sent.json().unwrap(),
        json!({"currency": "INR", "extended_credit_id": "987654321098765"})
    );

    // The status: Meta's example answer.
    let migration = format!("{path}/{MIGRATION}");
    f.h.graph.push_json(
        200,
        json!({"status": "READY_TO_COMPLETE",
               "destination_waba": {"id": "998877665544332", "name": "Acme WABA (INR)",
                                    "currency": "INR", "timezone_id": "1",
                                    "message_template_namespace": "1234abcd_namespace"},
               "id": MIGRATION}),
    );
    let status = f.h.call(tenant_call(Method::GET, &migration, &f)).await;
    assert_eq!(status.status, StatusCode::OK, "{}", status.text);
    assert_eq!(
        status.json(),
        json!({"id": MIGRATION, "status": "READY_TO_COMPLETE",
               "destination_waba_id": "998877665544332", "destination_currency": "INR"})
    );
    let read = f.h.graph.last_request().unwrap();
    assert_eq!(read.method, Method::GET);
    assert_eq!(read.path(), format!("/v25.0/{MIGRATION}"));
    assert_eq!(read.bearer(), Some(TOKEN));

    // The resumption.
    f.h.graph.push_json(
        200,
        json!({"status": "COMPLETED", "id": MIGRATION,
               "destination_waba": {"id": "998877665544332", "currency": "INR"}}),
    );
    let resumed =
        f.h.call(tenant_call(
            Method::POST,
            &format!("{migration}/resume"),
            &f,
        ))
        .await;
    assert_eq!(resumed.status, StatusCode::OK, "{}", resumed.text);
    assert_eq!(resumed.json()["status"], "COMPLETED");
    let sent = f.h.graph.last_request().unwrap();
    assert_eq!(sent.method, Method::POST);
    assert_eq!(sent.path(), format!("/v25.0/{MIGRATION}/resume_migration"));
    assert_eq!(sent.bearer(), Some(TOKEN));
    assert_eq!(sent.json(), None);
    assert_eq!(f.h.graph.remaining(), 0);

    // Not this WABA's: the tenant's second one, another tenant's, a
    // migration nobody started, an id that is no id.
    let before = f.h.graph.requests().len();
    let other_tenant = f.h.tenant_key(OTHER_TENANT, &[Scope::Partners]).await;
    for (what, call) in [
        (
            "the second WABA, read",
            tenant_call(
                Method::GET,
                &format!("/v1/wabas/{SECOND_WABA}/billing-migrations/{MIGRATION}"),
                &f,
            ),
        ),
        (
            "the second WABA, resumed",
            tenant_call(
                Method::POST,
                &format!("/v1/wabas/{SECOND_WABA}/billing-migrations/{MIGRATION}/resume"),
                &f,
            ),
        ),
        (
            "another tenant, read",
            Call::get(format!(
                "/v1/wabas/{OTHER_WABA}/billing-migrations/{MIGRATION}"
            ))
            .key(&other_tenant),
        ),
        (
            "another tenant, resumed",
            Call::new(
                Method::POST,
                format!("/v1/wabas/{OTHER_WABA}/billing-migrations/{MIGRATION}/resume"),
            )
            .key(&other_tenant),
        ),
        (
            "unknown",
            tenant_call(
                Method::GET,
                &format!("{path}/mig_01HZYK3UNKNOWN0000000"),
                &f,
            ),
        ),
        (
            "not an id",
            tenant_call(Method::GET, &format!("{path}/..%2Fx"), &f),
        ),
    ] {
        let reply = f.h.call(call).await;
        assert_eq!(
            (reply.status, reply.code().as_str()),
            (StatusCode::NOT_FOUND, "not_found"),
            "{what}: {}",
            reply.text
        );
    }
    assert_eq!(f.h.graph.requests().len(), before, "Meta was never asked");
}

/// A billing migration is checked before it is sent (a currency that is
/// not three capitals, a blank credit line), and neither it nor its
/// resumption is retried after a timeout: a replay would create a second
/// clone. Decisive: `PaymentMethodMigrationIntent::validate`, and the
/// library's no-replay.
#[tokio::test]
async fn a_billing_migration_is_checked_first_and_not_replayed() {
    let f = fixture(true).await;
    let path = format!("/v1/wabas/{WABA}/billing-migrations");
    for (body, field) in [
        (json!({"currency": "inr"}), "currency"),
        (json!({"currency": ""}), "currency"),
        (json!({"currency": "INRR"}), "currency"),
        (
            json!({"currency": "INR", "extended_credit_id": "not-an-id"}),
            "extended_credit_id",
        ),
        (json!({}), "body"),
    ] {
        let refused =
            f.h.call(tenant_call(Method::POST, &path, &f).json(&body))
                .await;
        assert_eq!(
            (refused.status, refused.json()["error"]["field"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some(field)),
            "{}",
            refused.text
        );
    }
    assert!(f.h.graph.requests().is_empty());
    f.h.graph.push_error(|| TransportError::Timeout);
    let timed_out =
        f.h.call(tenant_call(Method::POST, &path, &f).json(&json!({"currency": "INR"})))
            .await;
    assert_eq!(
        timed_out.status,
        StatusCode::GATEWAY_TIMEOUT,
        "{}",
        timed_out.text
    );
    assert_eq!(f.h.graph.requests().len(), 1);

    // A resumption that timed out is not retried either.
    f.h.graph.push_json(
        200,
        json!({"migration_id": MIGRATION, "migration_status": "INITIATED"}),
    );
    f.h.call(tenant_call(Method::POST, &path, &f).json(&json!({"currency": "MXN"})))
        .await;
    f.h.graph.push_error(|| TransportError::Timeout);
    let resume =
        f.h.call(tenant_call(
            Method::POST,
            &format!("{path}/{MIGRATION}/resume"),
            &f,
        ))
        .await;
    assert_eq!(
        resume.status,
        StatusCode::GATEWAY_TIMEOUT,
        "{}",
        resume.text
    );
    assert_eq!(f.h.graph.requests().len(), 3);
    assert_eq!(f.h.graph.remaining(), 0);
}
