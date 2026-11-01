//! Max price (roadmap L13): the agreement, template duplication, a
//! template's max price and reach estimates, against the examples of
//! `marketing-messages/pricing/enroll-max-price`,
//! `marketing-messages/pricing/duplicate-templates` and
//! `marketing-messages/pricing` (fetched 2026-11-01).

use std::time::Duration;

use http::Method;
use meta_whatsapp_core::Error;
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::ids::{BusinessId, PhoneNumberId, TemplateId, WabaId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::testing::ScriptedTransport;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::templates::{TemplateCategory, TemplateMessage, TemplateStatus};
use crate::{Client, RetryPolicy};

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

/// Retries on: a test using it proves a call is *not* replayed.
fn retrying_client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy {
            max_retries: 3,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        })
        .build()
        .unwrap()
}

fn business(c: &Client) -> MarketingBusiness {
    c.marketing_business(BusinessId::new("529216684107530").unwrap())
}

fn assert_validation(err: &Error, field: &str) {
    assert!(
        matches!(err, Error::Validation(v) if v.field == field),
        "expected a validation error on {field}, got {err:?}"
    );
}

// ─── The agreement ───────────────────────────────────────────────────────

#[tokio::test]
async fn signing_the_agreement_posts_the_documented_body() {
    let t = ScriptedTransport::new();
    // "Example response".
    t.push_json(
        200,
        json!({
            "status": "pending",
            "signer_name": "Jane Doe",
            "signer_email": "jane.doe@bsp.example",
            "message": "Agreement email sent to jane.doe@bsp.example"
        }),
    );
    let agreement = business(&client(&t))
        .sign_max_price_agreement(&MaxPriceAgreementSigner::new(
            "Jane Doe",
            "jane.doe@example.com",
        ))
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/529216684107530/max_price_agreements");
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({"signer_name": "Jane Doe", "signer_email": "jane.doe@example.com"}))
    );
    assert_eq!(
        agreement,
        MaxPriceAgreement {
            status: AgreementStatus::Pending,
            signer_name: Some("Jane Doe".into()),
            signer_email: Some("jane.doe@bsp.example".into()),
            message: Some("Agreement email sent to jane.doe@bsp.example".into()),
        }
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn agreement_status_reads_the_documented_examples() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"status": "accepted", "signer_name": "Jane Doe", "signer_email": "jane.doe@bsp.example"}),
    );
    t.push_json(
        200,
        json!({"status": "pending", "signer_name": "Jane Doe", "signer_email": "jane.doe@bsp.example"}),
    );
    t.push_json(200, json!({"status": "none"}));
    t.push_json(200, json!({"status": "revoked"}));
    let c = client(&t);
    let b = business(&c);
    let accepted = b.max_price_agreement().await.unwrap();
    assert_eq!(accepted.status, AgreementStatus::Accepted);
    assert_eq!(accepted.signer_name.as_deref(), Some("Jane Doe"));
    assert_eq!(
        b.max_price_agreement().await.unwrap().status,
        AgreementStatus::Pending
    );
    let none = b.max_price_agreement().await.unwrap();
    assert_eq!(none.status, AgreementStatus::NoAgreement);
    assert_eq!(none.signer_email, None);
    assert_eq!(
        b.max_price_agreement().await.unwrap().status,
        AgreementStatus::Unknown("revoked".into())
    );
    for req in t.requests() {
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/529216684107530/max_price_agreements");
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
    }
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn the_signer_is_required_and_checked_before_any_request() {
    let t = ScriptedTransport::new();
    let c = client(&t);
    let b = business(&c);
    let err = b
        .sign_max_price_agreement(&MaxPriceAgreementSigner::new(" ", "jane@example.com"))
        .await
        .unwrap_err();
    assert_validation(&err, "signer_name");
    let err = b
        .sign_max_price_agreement(&MaxPriceAgreementSigner::new("Jane Doe", ""))
        .await
        .unwrap_err();
    assert_validation(&err, "signer_email");
    assert!(t.requests().is_empty());
}

/// Signing is a legal act: a lost answer is surfaced, never replayed into a
/// second signature (or a second email).
#[tokio::test]
async fn signing_is_not_replayed_after_a_timeout() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let err = business(&retrying_client(&t))
        .sign_max_price_agreement(&MaxPriceAgreementSigner::new(
            "Jane Doe",
            "jane.doe@example.com",
        ))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(t.requests().len(), 1);
}

/// The response names the signer: an unreadable one is reported without
/// its body.
#[tokio::test]
async fn an_unreadable_agreement_response_never_quotes_the_signer() {
    let t = ScriptedTransport::new();
    t.push_bytes(
        200,
        "application/json",
        r#"{"status": 7, "signer_email": "jane.doe@bsp.example"}"#,
    );
    let err = business(&client(&t))
        .max_price_agreement()
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }), "{err}");
    assert!(!format!("{err} {err:?}").contains("jane.doe"), "{err:?}");
    assert_eq!(t.remaining(), 0);
}

/// Decisive (roadmap L13): a max-price send makes no agreement request. The
/// transport is scripted for the send alone, so any other call fails.
#[tokio::test]
async fn a_max_price_send_makes_no_agreement_request() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"messaging_product": "whatsapp", "contacts": [], "messages": [{"id": "wamid.1"}]}),
    );
    client(&t)
        .marketing(PhoneNumberId::new("106540352242922").unwrap())
        .send(
            &Recipient::phone("+16505551234"),
            &TemplateMessage::new("seasonal_sale_promo", "en"),
            &MarketingOptions::new().bid_multiplier(1.5),
        )
        .await
        .unwrap();
    let requests = t.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].path(),
        "/v25.0/106540352242922/marketing_messages"
    );
    assert_eq!(t.remaining(), 0);

    // Meta's "sign the testing legal agreement" refusal is returned as is:
    // the client does not answer it by signing.
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({"error": {
            "message": "You need to sign the testing legal agreement before sending out messages.",
            "type": "OAuthException",
            "code": 100,
            "fbtrace_id": "Ak6nxJSySLEJz32Ps-QiZ1t"
        }}),
    );
    let err = retrying_client(&t)
        .marketing(PhoneNumberId::new("106540352242922").unwrap())
        .send(
            &Recipient::phone("+16505551234"),
            &TemplateMessage::new("seasonal_sale_promo", "en"),
            &MarketingOptions::new().bid_multiplier(1.5),
        )
        .await
        .unwrap_err();
    assert_eq!(err.graph().unwrap().code, 100);
    assert_eq!(t.requests().len(), 1);
}

/// Nor does any other max price call: a duplicate refused for want of
/// enrolment (HTTP 403), a reach estimate, a template max price update.
#[tokio::test]
async fn no_other_max_price_call_signs_the_agreement() {
    let t = ScriptedTransport::new();
    t.push_json(
        403,
        json!({"error": {"message": "Enroll in max price before duplicating templates", "type": "OAuthException", "code": 10}}),
    );
    t.push_json(200, json!({"waba_currency": "USD", "estimates": []}));
    t.push_json(200, json!({"success": true}));
    let c = retrying_client(&t);
    let account = c.marketing_account(WabaId::new("102290129340398").unwrap());
    let err = account
        .duplicate_template(&DuplicateTemplate::new(
            TemplateId::new("1234567890123456").unwrap(),
            "seasonal_sale_promo_high_intent",
            MaxPrice::new(87000),
        ))
        .await
        .unwrap_err();
    assert_eq!(err.graph().unwrap().http_status, Some(403));
    account
        .reach_estimate(&ReachEstimateQuery::new(["IN"], DateInterval::Last7Days))
        .await
        .unwrap();
    c.marketing_template(TemplateId::new("1733678867511493").unwrap())
        .set_max_price(&MaxPrice::new(5000))
        .await
        .unwrap();
    let paths: Vec<String> = t.requests().iter().map(|r| r.path().to_owned()).collect();
    assert_eq!(
        paths,
        [
            "/v25.0/102290129340398/message_templates",
            "/v25.0/102290129340398/reachestimate",
            "/v25.0/1733678867511493",
        ]
    );
    assert_eq!(t.remaining(), 0);
}

/// The agreement's path is named in one file under `src/`: no helper,
/// builder or send path elsewhere can reach it.
#[test]
fn only_the_agreement_module_names_the_agreement() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    let mut naming: Vec<String> = files
        .iter()
        .filter(|path| {
            let text = std::fs::read_to_string(path).unwrap();
            text.contains("max_price_agreements\"") || text.contains("sign_max_price_agreement(")
        })
        .map(|path| {
            path.strip_prefix(&src)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    naming.sort();
    assert_eq!(
        naming,
        ["marketing/agreement.rs", "marketing/tests_max_price.rs"]
    );
}

// ─── Duplicating a template ──────────────────────────────────────────────

#[tokio::test]
async fn duplicate_matches_the_documented_example() {
    let t = ScriptedTransport::new();
    // The page's example response, without its trailing comma (as printed
    // it is not valid JSON).
    t.push_json(
        200,
        json!({"id": "1733678867511493", "status": "APPROVED", "category": "MARKETING"}),
    );
    let created = client(&t)
        .marketing_account(WabaId::new("102290129340398").unwrap())
        .duplicate_template(&DuplicateTemplate::new(
            TemplateId::new("1234567890123456").unwrap(),
            "seasonal_sale_promo_high_intent",
            MaxPrice::new(87000),
        ))
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/102290129340398/message_templates");
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({
            "source_template_id": "1234567890123456",
            "name": "seasonal_sale_promo_high_intent",
            "optimization_spec": {
                "bid_amount": 87000,
                "bid_strategy": "LOWEST_COST_WITH_BID_CAP"
            }
        }))
    );
    assert_eq!(created.id.as_str(), "1733678867511493");
    assert_eq!(created.status, Some(TemplateStatus::Approved));
    assert_eq!(created.category, Some(TemplateCategory::Marketing));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn duplicate_is_checked_locally_and_not_replayed() {
    let t = ScriptedTransport::new();
    let c = client(&t);
    let account = c.marketing_account(WabaId::new("102290129340398").unwrap());
    let source = TemplateId::new("1234567890123456").unwrap();
    for bad in ["", "Seasonal", "has space"] {
        let err = account
            .duplicate_template(&DuplicateTemplate::new(
                source.clone(),
                bad,
                MaxPrice::new(87000),
            ))
            .await
            .unwrap_err();
        assert_validation(&err, "name");
    }
    let err = account
        .duplicate_template(&DuplicateTemplate::new(
            source.clone(),
            "dup",
            MaxPrice::new(0),
        ))
        .await
        .unwrap_err();
    assert_validation(&err, "optimization_spec.bid_amount");
    assert!(t.requests().is_empty());

    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let err = retrying_client(&t)
        .marketing_account(WabaId::new("102290129340398").unwrap())
        .duplicate_template(&DuplicateTemplate::new(source, "dup", MaxPrice::new(87000)))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(t.requests().len(), 1);
}

// ─── A max price's limits ────────────────────────────────────────────────

#[test]
fn per_delivery_prices_follow_the_documented_arithmetic() {
    // ₹0.87 = 87 paise → 87000; $0.05 = 5 cents → 5000.
    assert_eq!(MaxPrice::per_delivery(87).unwrap().bid_amount, 87_000);
    assert_eq!(MaxPrice::per_delivery(5).unwrap().bid_amount, 5_000);
    // int32 per 1,000 deliveries: 2_147_483 * 1000 fits, one more does not.
    assert_eq!(
        MaxPrice::per_delivery(2_147_483).unwrap().bid_amount,
        2_147_483_000
    );
    assert_eq!(
        MaxPrice::per_delivery(2_147_484).unwrap_err().field,
        "optimization_spec.bid_amount"
    );
    assert_eq!(
        MaxPrice::per_delivery(u32::MAX).unwrap_err().field,
        "optimization_spec.bid_amount"
    );
}

#[test]
fn a_max_price_is_checked_against_the_documented_limits() {
    const FIELD: &str = "optimization_spec.bid_country_multiplier_overrides";
    assert!(MaxPrice::new(1).validate().is_ok());
    assert!(MaxPrice::new(i32::MAX as u32).validate().is_ok());
    for bad in [0, i32::MAX as u32 + 1, u32::MAX] {
        assert_eq!(
            MaxPrice::new(bad).validate().unwrap_err().field,
            "optimization_spec.bid_amount",
            "{bad}"
        );
    }

    // The 240-entry cap: 240 pass, 241 do not.
    let codes: Vec<String> = (b'A'..=b'Z')
        .flat_map(|a| (b'A'..=b'Z').map(move |b| String::from_utf8(vec![a, b]).unwrap()))
        .take(MAX_COUNTRY_MULTIPLIERS + 1)
        .collect();
    let at_cap = codes[..MAX_COUNTRY_MULTIPLIERS]
        .iter()
        .fold(MaxPrice::new(1000), |p, code| {
            p.country_multiplier(code, 1.0)
        });
    assert_eq!(MAX_COUNTRY_MULTIPLIERS, 240);
    assert!(at_cap.validate().is_ok());
    let over = at_cap.country_multiplier(&codes[MAX_COUNTRY_MULTIPLIERS], 1.0);
    assert_eq!(over.validate().unwrap_err().field, FIELD);

    // Each multiplier in (0, 10].
    for good in [0.01, 1.0, 10.0] {
        assert!(
            MaxPrice::new(1000)
                .country_multiplier("IN", good)
                .validate()
                .is_ok(),
            "{good}"
        );
    }
    for bad in [0.0, -1.0, 10.01, f64::NAN, f64::INFINITY] {
        assert_eq!(
            MaxPrice::new(1000)
                .country_multiplier("IN", bad)
                .validate()
                .unwrap_err()
                .field,
            FIELD,
            "{bad}"
        );
    }
    // Keys shaped as ISO 3166-1 alpha-2.
    for bad in ["in", "IND", "I", "", "1N", "I N"] {
        assert_eq!(
            MaxPrice::new(1000)
                .country_multiplier(bad, 1.2)
                .validate()
                .unwrap_err()
                .field,
            FIELD,
            "{bad:?}"
        );
    }
    assert!(
        MaxPrice::new(1000)
            .clear_country_multipliers()
            .validate()
            .is_ok()
    );
}

// ─── A template's max price ──────────────────────────────────────────────

#[tokio::test]
async fn set_max_price_matches_the_documented_update() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    t.push_json(200, json!({"success": true}));
    let c = client(&t);
    let template = c.marketing_template(TemplateId::new("1733678867511493").unwrap());
    template
        .set_max_price(
            &MaxPrice::new(5000)
                .country_multiplier("IN", 1.4)
                .country_multiplier("MX", 0.9),
        )
        .await
        .unwrap();
    let req = &t.requests()[0];
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/1733678867511493");
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({"optimization_spec": {
            "bid_strategy": "LOWEST_COST_WITH_BID_CAP",
            "bid_amount": 5000,
            "bid_country_multiplier_overrides": {"IN": 1.4, "MX": 0.9}
        }}))
    );
    // An empty map removes every override.
    template
        .set_max_price(&MaxPrice::new(5000).clear_country_multipliers())
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(json!({"optimization_spec": {
            "bid_strategy": "LOWEST_COST_WITH_BID_CAP",
            "bid_amount": 5000,
            "bid_country_multiplier_overrides": {}
        }}))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn clear_max_price_sends_exactly_the_two_nulls() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    client(&t)
        .marketing_template(TemplateId::new("1733678867511493").unwrap())
        .clear_max_price()
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/1733678867511493");
    assert_eq!(
        req.json(),
        Some(json!({"optimization_spec": {"bid_strategy": null, "bid_amount": null}}))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn template_max_price_updates_are_not_replayed_and_check_success() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_error(|| TransportError::Timeout);
    let c = retrying_client(&t);
    let template = c.marketing_template(TemplateId::new("1733678867511493").unwrap());
    assert!(template.set_max_price(&MaxPrice::new(5000)).await.is_err());
    assert!(template.clear_max_price().await.is_err());
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);

    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": false}));
    let err = client(&t)
        .marketing_template(TemplateId::new("1733678867511493").unwrap())
        .set_max_price(&MaxPrice::new(5000))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Http { status: 200, .. }), "{err}");

    let t = ScriptedTransport::new();
    let err = client(&t)
        .marketing_template(TemplateId::new("1733678867511493").unwrap())
        .set_max_price(&MaxPrice::new(5000).country_multiplier("XX", 11.0))
        .await
        .unwrap_err();
    assert_validation(&err, "optimization_spec.bid_country_multiplier_overrides");
    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn template_max_price_reads_the_documented_list_shape() {
    let t = ScriptedTransport::new();
    // "Retrieve max price information": overrides read back as a list.
    t.push_json(
        200,
        json!({
            "optimization_spec": {
                "bid_strategy": "LOWEST_COST_WITH_BID_CAP",
                "bid_amount": 87000,
                "bid_country_multiplier_overrides": [
                    {"key": "IN", "value": 1.2},
                    {"key": "BR", "value": 1.5},
                    {"key": "MX", "value": 0.8}
                ]
            },
            "id": "1733678867511493"
        }),
    );
    t.push_json(
        200,
        json!({"optimization_spec": {"bid_amount": 1000, "bid_strategy": "LOWEST_COST_WITH_BID_CAP", "bid_country_multiplier_overrides": []}, "id": "1"}),
    );
    t.push_json(200, json!({"id": "1"}));
    let c = client(&t);
    let template = c.marketing_template(TemplateId::new("1733678867511493").unwrap());
    let spec = template.max_price().await.unwrap().unwrap();
    let req = &t.requests()[0];
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/1733678867511493");
    assert_eq!(req.query("fields").as_deref(), Some("optimization_spec"));
    assert_eq!(spec.bid_amount, Some(87000));
    assert_eq!(spec.bid_strategy, Some(BidStrategy::LowestCostWithBidCap));
    assert_eq!(
        spec.bid_country_multiplier_overrides,
        [
            ("BR".to_owned(), 1.5),
            ("IN".to_owned(), 1.2),
            ("MX".to_owned(), 0.8)
        ]
        .into_iter()
        .collect()
    );
    let bare = template.max_price().await.unwrap().unwrap();
    assert!(bare.bid_country_multiplier_overrides.is_empty());
    assert_eq!(template.max_price().await.unwrap(), None);
    assert_eq!(t.remaining(), 0);
}

// ─── Reach estimates ─────────────────────────────────────────────────────

#[tokio::test]
async fn reach_estimate_matches_the_documented_example() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({
            "waba_currency": "USD",
            "estimates": [
                {
                    "bid_amount": 400,
                    "users": 1000,
                    "deliveries_lower_bound": 500,
                    "deliveries_upper_bound": 570,
                    "cost_lower_bound": 389.74,
                    "cost_upper_bound": 390.74
                },
                {
                    "bid_amount": 520,
                    "users": 1000,
                    "deliveries_lower_bound": 600,
                    "deliveries_upper_bound": 650,
                    "cost_lower_bound": 400.74,
                    "cost_upper_bound": 510.74
                }
            ]
        }),
    );
    let estimate = client(&t)
        .marketing_account(WabaId::new("102290129340398").unwrap())
        .reach_estimate(&ReachEstimateQuery::new(["IN"], DateInterval::Last7Days))
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/102290129340398/reachestimate");
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.query("targeting_spec").as_deref(),
        Some(r#"{"geo_locations":{"countries":["IN"]}}"#)
    );
    assert_eq!(req.query("date_interval").as_deref(), Some("L7D"));
    assert_eq!(estimate.waba_currency.as_deref(), Some("USD"));
    assert_eq!(
        estimate.estimates[0],
        ReachEstimatePoint {
            bid_amount: Some(400),
            users: Some(1000),
            deliveries_lower_bound: Some(500),
            deliveries_upper_bound: Some(570),
            cost_lower_bound: Some(389.74),
            cost_upper_bound: Some(390.74),
        }
    );
    assert_eq!(estimate.estimates[1].bid_amount, Some(520));
    assert_eq!(estimate.estimates[1].cost_upper_bound, Some(510.74));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn reach_estimate_intervals_and_local_checks() {
    let t = ScriptedTransport::new();
    for _ in 0..4 {
        t.push_json(200, json!({"estimates": []}));
    }
    let c = client(&t);
    let account = c.marketing_account(WabaId::new("102290129340398").unwrap());
    for (interval, wire) in [
        (DateInterval::LastDay, "L1D"),
        (DateInterval::Last7Days, "L7D"),
        (DateInterval::Last14Days, "L14D"),
        (DateInterval::Last28Days, "L28D"),
    ] {
        account
            .reach_estimate(&ReachEstimateQuery::new(["IN", "BR"], interval))
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.query("date_interval").as_deref(), Some(wire));
        let spec: serde_json::Value =
            serde_json::from_str(&req.query("targeting_spec").unwrap()).unwrap();
        assert_eq!(spec, json!({"geo_locations": {"countries": ["IN", "BR"]}}));
    }
    assert_eq!(t.remaining(), 0);
    let before = t.requests().len();
    let none: [&str; 0] = [];
    for query in [
        ReachEstimateQuery::new(none, DateInterval::Last7Days),
        ReachEstimateQuery::new(["IN", " "], DateInterval::Last7Days),
    ] {
        let err = account.reach_estimate(&query).await.unwrap_err();
        assert_validation(&err, "targeting_spec.geo_locations.countries");
    }
    assert_eq!(t.requests().len(), before);
}
