//! Acceptance test M1.5: every `ErrorKind` (iterating `ErrorKind::ALL`,
//! library change L1) maps to a documented code and status, and Meta's
//! error text reaches no response.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::collections::{BTreeSet, HashMap};

use common::{Call, Harness, Sample, sample_call, spec_operations};
use meta_whatsapp_rs::ErrorKind;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::error::{CODES, code_info, kind_code};
use meta_whatsapp_server::model::Scope;
use serde_json::{Value, json};

/// docs/design/server.md, section 5.2, as written there: every code and
/// its status. A code the service answers that is not here, or with
/// another status, fails. (`unauthenticated`, for `401`, is the one code
/// the design's table leaves unnamed.)
const DESIGN: &[(u16, &[&str])] = &[
    (
        422,
        &[
            "invalid_request",
            "invalid_parameter",
            "unsupported_message_type",
            "recipient_not_supported",
            "undeliverable",
            "template_parameter_mismatch",
            "template_not_found",
            "template_text_too_long",
            "template_policy_violation",
            "template_rejected",
            "idempotency_key_reused",
        ],
    ),
    (
        409,
        &[
            "customer_service_window_closed",
            "marketing_opted_out",
            "blocked_by_business",
            "experiment_holdout",
            "template_paused",
            "template_disabled",
            "template_syncing",
            "template_unavailable",
            "template_limit_reached",
            "flow_unavailable",
            "registration",
            "two_step_verification",
            "sync_not_allowed",
            "duplicate_onboarding",
            "number_not_connected",
            "reconnect_required",
            "waba_owned_by_another_tenant",
            "idempotency_in_progress",
            "outcome_unknown",
        ],
    ),
    (
        403,
        &[
            "permission",
            "account_restricted",
            "country_restricted",
            "payment",
            "feature_not_available",
            "marketing_not_allowed",
            "forbidden",
            "tenant_suspended",
            "stale_attempt",
        ],
    ),
    (
        429,
        &[
            "rate_limited",
            "pair_rate_limited",
            "spam_rate_limited",
            "ecosystem_engagement_limit",
            "classification_limit_reached",
            "too_many_requests",
            "too_many_streams",
        ],
    ),
    (404, &["not_found", "nothing_to_resume"]),
    (410, &["cursor_expired"]),
    (413, &["payload_too_large", "media_too_large"]),
    (
        502,
        &[
            "service_unavailable",
            "unknown",
            "upstream",
            "integrity",
            "media_download_failed",
            "media_upload_failed",
            "onboarding_failed",
        ],
    ),
    (504, &["timeout"]),
    (503, &["storage_unavailable", "shutting_down"]),
    (500, &["internal"]),
    (401, &["unauthenticated"]),
];

fn design() -> HashMap<&'static str, u16> {
    let mut map = HashMap::new();
    for (status, codes) in DESIGN {
        for code in *codes {
            assert!(
                map.insert(*code, *status).is_none(),
                "{code} twice in the design table"
            );
        }
    }
    map
}

/// The service's table is the design's, code for code and status for
/// status.
#[test]
fn the_codes_are_the_designs() {
    let design = design();
    assert_eq!(
        CODES.len(),
        design.len(),
        "codes the design does not list, or missing ones"
    );
    for (code, status, _) in CODES {
        assert_eq!(design.get(code), Some(status), "{code}");
    }
}

/// M1.5, first half. Decisive: a kind a new library release adds (it
/// appears in `ErrorKind::ALL`) and the service does not map.
#[test]
fn every_error_kind_maps_to_a_documented_code_and_status() {
    let design = design();
    assert!(ErrorKind::ALL.len() >= 40);
    for kind in ErrorKind::ALL {
        let code = kind_code(*kind);
        let status = design
            .get(code)
            .unwrap_or_else(|| panic!("{kind:?} maps to `{code}`, which the design does not list"));
        let (answered, _) = code_info(code).unwrap();
        assert_eq!(answered.as_u16(), *status, "{kind:?}");
        // The code is the kind's L1 name, but for the one the design renames.
        if *kind == ErrorKind::Authentication {
            assert_eq!(code, "reconnect_required");
        } else {
            assert_eq!(code, kind.as_str());
        }
    }
}

/// A Graph code of each kind (from `ErrorKind::from_code`'s documentation);
/// the test fails for a kind that has none here.
const REPRESENTATIVE: &[(ErrorKind, i64)] = &[
    (ErrorKind::Authentication, 190),
    (ErrorKind::Permission, 200),
    (ErrorKind::RateLimited, 130429),
    (ErrorKind::SpamRateLimited, 131048),
    (ErrorKind::PairRateLimited, 131056),
    (ErrorKind::ClassificationLimitReached, 131064),
    (ErrorKind::AccountRestricted, 368),
    (ErrorKind::CountryRestricted, 130497),
    (ErrorKind::InvalidParameter, 100),
    (ErrorKind::UnsupportedMessageType, 131051),
    (ErrorKind::CustomerServiceWindowClosed, 131047),
    (ErrorKind::EcosystemEngagementLimit, 131049),
    (ErrorKind::MarketingOptedOut, 131050),
    (ErrorKind::MarketingNotAllowed, 131055),
    (ErrorKind::Undeliverable, 131026),
    (ErrorKind::BlockedByBusiness, 130403),
    (ErrorKind::ExperimentHoldout, 130472),
    (ErrorKind::RecipientNotSupported, 131062),
    (ErrorKind::MediaDownloadFailed, 131052),
    (ErrorKind::MediaUploadFailed, 131053),
    (ErrorKind::TemplateParameterMismatch, 132000),
    (ErrorKind::TemplateNotFound, 132001),
    (ErrorKind::TemplateTextTooLong, 132005),
    (ErrorKind::TemplatePolicyViolation, 132007),
    (ErrorKind::TemplatePaused, 132015),
    (ErrorKind::TemplateDisabled, 132016),
    (ErrorKind::TemplateSyncing, 134101),
    (ErrorKind::TemplateUnavailable, 134102),
    (ErrorKind::TemplateLimitReached, 2388019),
    (ErrorKind::TemplateRejected, 2388039),
    (ErrorKind::FlowUnavailable, 132068),
    (ErrorKind::Registration, 133010),
    (ErrorKind::TwoStepVerification, 133005),
    (ErrorKind::SyncNotAllowed, 2593107),
    (ErrorKind::DuplicateOnboarding, 1752041),
    (ErrorKind::NotFound, 2494164),
    (ErrorKind::FeatureNotAvailable, 2494165),
    (ErrorKind::Payment, 131042),
    (ErrorKind::ServiceUnavailable, 131000),
    (ErrorKind::Unknown, 999999999),
];

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const SENTINEL: &str = "SENTINEL-7f3a91";

/// A Graph error whose every text carries the sentinel.
fn graph_error(code: i64) -> Value {
    json!({"error": {
        "message": format!("({code}) {SENTINEL} message"),
        "title": format!("{SENTINEL} title"),
        "type": "OAuthException",
        "code": code,
        "error_user_title": format!("{SENTINEL} user title"),
        "error_user_msg": format!("{SENTINEL} user message"),
        "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"
    }})
}

/// M1.5, second half, end to end: each kind of Graph error, on a number
/// route, answers its code and status, `graph.code`, and none of Meta's
/// texts.
#[tokio::test]
async fn each_kind_answers_its_code_and_no_meta_text() {
    let covered: Vec<ErrorKind> = REPRESENTATIVE.iter().map(|(k, _)| *k).collect();
    for kind in ErrorKind::ALL {
        assert!(
            covered.contains(kind),
            "{kind:?} has no representative code here"
        );
    }
    for (kind, code) in REPRESENTATIVE {
        assert_eq!(ErrorKind::from_code(*code), *kind, "{code}");
        let h = Harness::new();
        h.tenant(TENANT).await;
        h.connect(TENANT, WABA, &[PN], "TOKEN").await;
        let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
        h.graph.push_json(400, graph_error(*code));
        let reply = h
            .call(Call::get(format!("/v1/numbers/{PN}")).key(&key))
            .await;
        let expected = kind_code(*kind);
        assert_eq!(reply.code(), expected, "{kind:?}");
        assert_eq!(reply.status, code_info(expected).unwrap().0, "{kind:?}");
        let body = reply.json();
        assert_eq!(body["error"]["graph"]["code"], *code);
        assert_eq!(
            body["error"]["graph"]["fbtrace_id"],
            "AXsgnV2Cm3ZMGF3dF_cfYIn"
        );
        // A 4xx Graph error proves Meta did nothing.
        assert_eq!(body["error"]["may_have_been_sent"], false, "{kind:?}");
        assert!(
            body["error"]["request_id"]
                .as_str()
                .unwrap()
                .starts_with("req_")
        );
        assert!(!reply.text.contains(SENTINEL), "{kind:?}: {}", reply.text);
        assert_eq!(h.graph.remaining(), 0);
    }
}

/// The operations of the committed document that ask Meta something when
/// called with `common::sample_call`. The test below finds them by calling
/// every operation, and fails when they differ from this list: a new route
/// is looked at before it passes.
const CALLS_META: &[&str] = &[
    "GET /v1/numbers/{pn}",
    "GET /v1/numbers/{pn}/profile",
    "PATCH /v1/numbers/{pn}/profile",
    "DELETE /v1/wabas/{waba_id}",
    "POST /v1/admin/tenants/{id}/wabas",
    "DELETE /v1/admin/tenants/{id}",
];

/// M1.5's sentinel, on every operation of the committed document: Meta's
/// error texts, a non-Graph answer and an unreadable one reach no
/// response. Decisive: a Meta text copied into a body, on any route.
#[tokio::test]
async fn a_sentinel_in_metas_answer_reaches_no_response() {
    let answers: Vec<(u16, String)> = vec![
        (400, graph_error(100).to_string()),
        (500, graph_error(131000).to_string()),
        (
            502,
            format!("<html><body>{SENTINEL} bad gateway</body></html>"),
        ),
        (
            200,
            format!("{{\"id\": \"{SENTINEL}\", \"quality_rating\": 7"),
        ),
    ];
    let sample = Sample {
        tenant: TENANT.to_owned(),
        waba: WABA.to_owned(),
        pn: PN.to_owned(),
        key_id: "placeholder".to_owned(),
    };
    let mut calling = BTreeSet::new();
    let mut quiet = BTreeSet::new();
    for operation in spec_operations() {
        for (status, body) in &answers {
            let h = Harness::new();
            let admin = h.admin_key().await;
            h.tenant(TENANT).await;
            h.connect(TENANT, WABA, &[PN], "TOKEN").await;
            let key = h.tenant_key(TENANT, &Scope::ALL).await;
            let key = match (operation.keyed, operation.admin()) {
                (false, _) => None,
                (true, true) => Some(admin.as_str()),
                (true, false) => Some(key.as_str()),
            };
            h.graph
                .push_bytes(*status, "application/json", body.clone().into_bytes());
            let reply = h.call(sample_call(&operation, &sample, key)).await;
            let label = format!("{} <- {status}", operation.label());
            assert!(!reply.text.contains(SENTINEL), "{label}: {}", reply.text);
            if h.graph.remaining() == 1 {
                quiet.insert(operation.label());
                continue;
            }
            calling.insert(operation.label());
            assert!(
                reply.status.is_client_error() || reply.status.is_server_error(),
                "{label}: {}",
                reply.status
            );
            let code = reply.code();
            assert!(CODES.iter().any(|(c, _, _)| *c == code), "{label}: {code}");
            if *status >= 500 || *status == 200 {
                assert!(
                    matches!(code.as_str(), "service_unavailable" | "upstream"),
                    "{label}: {code}"
                );
            }
        }
    }
    let listed: BTreeSet<String> = CALLS_META.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(calling, listed, "the operations that call Meta");
    assert!(
        calling.is_disjoint(&quiet),
        "an operation called Meta for some answers only"
    );
    assert!(quiet.len() >= 15, "{quiet:?}");
}

/// `may_have_been_sent` and `retryable` are the library's.
#[tokio::test]
async fn sent_and_retryable_are_the_librarys() {
    for (status, code, sent, retryable) in [
        (400, 100, false, false),
        (500, 131000, true, true),
        (429, 130429, false, true),
    ] {
        let h = Harness::new();
        h.tenant(TENANT).await;
        h.connect(TENANT, WABA, &[PN], "TOKEN").await;
        let key = h.tenant_key(TENANT, &[Scope::Numbers]).await;
        h.graph.push_json(status, graph_error(code));
        let reply = h
            .call(Call::new(Method::DELETE, format!("/v1/wabas/{WABA}")).key(&key))
            .await;
        let body = reply.json();
        assert_eq!(body["error"]["may_have_been_sent"], sent, "{code}");
        assert_eq!(body["error"]["retryable"], retryable, "{code}");
        assert!(reply.status.as_u16() >= 400);
    }
    // A status the design keeps for a service condition.
    assert_eq!(code_info("timeout").unwrap().0, StatusCode::GATEWAY_TIMEOUT);
}
