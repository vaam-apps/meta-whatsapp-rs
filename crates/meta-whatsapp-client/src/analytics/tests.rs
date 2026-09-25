use std::time::Duration;

use futures::StreamExt;
use http::Method;
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::testing::ScriptedTransport;
use meta_whatsapp_core::{Error, ErrorKind};
use serde_json::json;
use time::macros::date;

use super::*;
use crate::RetryPolicy;

const WABA: &str = "102290129340398";

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

fn at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).unwrap()
}

fn validation_field(err: &Error) -> &str {
    match err {
        Error::Validation(v) => &v.field,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

fn assert_get_on_waba(t: &ScriptedTransport, fields: &str) {
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), format!("/v25.0/{WABA}"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(req.query("fields").as_deref(), Some(fields));
    assert_eq!(req.url.query_pairs().count(), 1, "only `fields`");
}

// ── Messaging ───────────────────────────────────────────────────────────

#[tokio::test]
async fn messaging_matches_docs_example() {
    let t = ScriptedTransport::new();
    // analytics "Messaging analytics" example response (4 data points).
    t.push_json(
        200,
        json!({
            "analytics": {
                "phone_numbers": ["16505550111", "16505550112", "16505550113"],
                "country_codes": ["US", "BR"],
                "granularity": "DAY",
                "data_points": [
                    {"start": 1543543200, "end": 1543629600, "sent": 196093, "delivered": 179715},
                    {"start": 1543629600, "end": 1543716000, "sent": 147649, "delivered": 139032},
                    {"start": 1543716000, "end": 1543802400, "sent": 61988, "delivered": 58830},
                    {"start": 1543802400, "end": 1543888800, "sent": 132465, "delivered": 124392}
                ]
            },
            "id": "102290129340398"
        }),
    );
    let a = client(&t)
        .analytics(WABA)
        .messaging(&MessagingAnalyticsQuery::new(
            at(1543543200),
            at(1544148000),
            MessagingGranularity::Day,
        ))
        .await
        .unwrap()
        .unwrap();
    assert_get_on_waba(
        &t,
        "analytics.start(1543543200).end(1544148000).granularity(DAY)",
    );
    assert_eq!(a.phone_numbers.len(), 3);
    assert_eq!(a.country_codes, ["US", "BR"]);
    assert_eq!(a.granularity, Some(MessagingGranularity::Day));
    assert_eq!(a.data_points.len(), 4);
    assert_eq!(
        a.data_points[0],
        MessagingDataPoint {
            start: at(1543543200),
            end: at(1543629600),
            sent: Some(196093),
            delivered: Some(179715),
        }
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn messaging_filters_are_encoded_and_missing_object_is_none() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": WABA}));
    let a = client(&t)
        .analytics(WABA)
        .messaging(&MessagingAnalyticsQuery {
            phone_numbers: vec!["16505550111".into()],
            product_types: vec![
                MessageProductType::Template,
                MessageProductType::NonTemplate,
                MessageProductType::Incoming,
            ],
            country_codes: vec!["US".into(), "BR".into()],
            ..MessagingAnalyticsQuery::new(at(1), at(2), MessagingGranularity::HalfHour)
        })
        .await
        .unwrap();
    assert_get_on_waba(
        &t,
        r#"analytics.start(1).end(2).granularity(HALF_HOUR).phone_numbers(["16505550111"]).product_types([0,2,100]).country_codes(["US","BR"])"#,
    );
    assert_eq!(a, None);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn country_codes_must_be_two_letters() {
    let t = ScriptedTransport::new();
    let api = client(&t).analytics(WABA);
    let err = api
        .messaging(&MessagingAnalyticsQuery {
            country_codes: vec!["US".into(), "USA".into()],
            ..MessagingAnalyticsQuery::new(at(1), at(2), MessagingGranularity::Day)
        })
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "country_codes[1]");
    let err = api
        .pricing(&PricingAnalyticsQuery {
            country_codes: vec!["1N".into()],
            ..PricingAnalyticsQuery::new(at(1), at(2), Granularity::Daily)
        })
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "country_codes[0]");
    let err = api
        .calls(&CallAnalyticsQuery {
            country_codes: vec![String::new()],
            ..CallAnalyticsQuery::new(at(1), at(2), Granularity::Daily)
        })
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "country_codes[0]");
    assert!(t.requests().is_empty());
}

// ── Conversation ────────────────────────────────────────────────────────

#[tokio::test]
async fn conversation_matches_docs_example_with_breakdowns() {
    use ConversationDimension as D;
    let t = ScriptedTransport::new();
    // analytics "Get monthly data, using all breakdowns" (first two points).
    t.push_json(
        200,
        json!({
            "conversation_analytics": {"data": [{"data_points": [
                {"start": 1685602800, "end": 1688194800, "conversation": 1558, "phone_number": "15550458206", "country": "US", "conversation_type": "REGULAR", "conversation_direction": "UNKNOWN", "conversation_category": "AUTHENTICATION", "cost": 15.58},
                {"start": 1685602800, "end": 1688194800, "conversation": 1433, "phone_number": "15550458206", "country": "US", "conversation_type": "FREE_ENTRY_POINT", "conversation_category": "SERVICE", "cost": 14.33}
            ]}]},
            "id": "102290129340398"
        }),
    );
    let a = client(&t)
        .analytics(WABA)
        .conversation(&ConversationAnalyticsQuery {
            dimensions: vec![
                D::ConversationCategory,
                D::ConversationType,
                D::Country,
                D::Phone,
            ],
            ..ConversationAnalyticsQuery::new(at(1685602800), at(1688194800), Granularity::Monthly)
        })
        .await
        .unwrap()
        .unwrap();
    // The docs also send `.phone_numbers([])`, which means "all numbers",
    // the same as leaving it out.
    assert_get_on_waba(
        &t,
        r#"conversation_analytics.start(1685602800).end(1688194800).granularity(MONTHLY).dimensions(["CONVERSATION_CATEGORY","CONVERSATION_TYPE","COUNTRY","PHONE"])"#,
    );
    let points: Vec<_> = a.data_points().collect();
    assert_eq!(points.len(), 2);
    assert_eq!(points[0].conversation, Some(1558));
    assert_eq!(points[0].cost, Some(15.58));
    assert_eq!(points[0].phone_number.as_deref(), Some("15550458206"));
    assert_eq!(points[0].conversation_type, Some(ConversationType::Regular));
    assert_eq!(
        points[0].conversation_direction,
        Some(ConversationDirection::Unknown)
    );
    assert_eq!(
        points[0].conversation_category,
        Some(ConversationCategory::Authentication)
    );
    assert_eq!(points[1].conversation_direction, None);
    assert_eq!(
        points[1].conversation_type,
        Some(ConversationType::FreeEntryPoint)
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn conversation_every_filter_and_unwrapped_example() {
    let t = ScriptedTransport::new();
    // analytics "Get monthly data, using conversation type breakdowns": the
    // page shows this response without the `conversation_analytics` key.
    t.push_json(
        200,
        json!({"data": [{"data_points": [
            {"start": 1643702400, "end": 1646121600, "conversation": 8500, "conversation_type": "REGULAR", "cost": 88.1010},
            {"start": 1643702400, "end": 1646121600, "conversation": 1000, "conversation_type": "FREE_TIER", "cost": 0.0000}
        ]}]}),
    );
    let a = client(&t)
        .analytics(WABA)
        .conversation(&ConversationAnalyticsQuery {
            phone_numbers: vec!["19195552584".into()],
            metric_types: vec![ConversationMetric::Cost, ConversationMetric::Conversation],
            conversation_categories: vec![
                ConversationCategory::Marketing,
                ConversationCategory::Utility,
            ],
            conversation_types: vec![ConversationType::FreeTier],
            conversation_directions: vec![ConversationDirection::BusinessInitiated],
            dimensions: vec![ConversationDimension::ConversationType],
            ..ConversationAnalyticsQuery::new(at(1643702400), at(1646121600), Granularity::HalfHour)
        })
        .await
        .unwrap()
        .unwrap();
    assert_get_on_waba(
        &t,
        r#"conversation_analytics.start(1643702400).end(1646121600).granularity(HALF_HOUR).phone_numbers(["19195552584"]).metric_types(["COST","CONVERSATION"]).conversation_categories(["MARKETING","UTILITY"]).conversation_types(["FREE_TIER"]).conversation_directions(["BUSINESS_INITIATED"]).dimensions(["CONVERSATION_TYPE"])"#,
    );
    assert_eq!(a.data_points().count(), 2);
    assert_eq!(
        a.data[0].data_points[1].conversation_type,
        Some(ConversationType::FreeTier)
    );
    assert_eq!(t.remaining(), 0);
}

// ── Pricing ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn pricing_matches_docs_example() {
    use PricingDimension as D;
    let t = ScriptedTransport::new();
    // analytics "Pricing analytics" example response (subset).
    t.push_json(
        200,
        json!({"pricing_analytics": {"data": [{"data_points": [
            {"start": 1749193200, "end": 1749279600, "country": "IN", "pricing_type": "FREE_CUSTOMER_SERVICE", "pricing_category": "SERVICE", "volume": 2, "cost": 0},
            {"start": 1749106800, "end": 1749193200, "country": "IN", "tier": "0:750000", "pricing_type": "REGULAR", "pricing_category": "AUTHENTICATION_INTERNATIONAL", "volume": 2, "cost": 4.6},
            {"start": 1748761200, "end": 1748847600, "country": "US", "tier": "0:MAX", "pricing_type": "REGULAR", "pricing_category": "MARKETING_LITE", "volume": 1, "cost": 10}
        ]}]}}),
    );
    let a = client(&t)
        .analytics(WABA)
        .pricing(&PricingAnalyticsQuery {
            country_codes: vec!["US".into(), "IN".into()],
            dimensions: vec![D::PricingCategory, D::PricingType, D::Tier, D::Country],
            ..PricingAnalyticsQuery::new(at(1748761200), at(1749687703), Granularity::Daily)
        })
        .await
        .unwrap()
        .unwrap();
    assert_get_on_waba(
        &t,
        r#"pricing_analytics.start(1748761200).end(1749687703).granularity(DAILY).country_codes(["US","IN"]).dimensions(["PRICING_CATEGORY","PRICING_TYPE","TIER","COUNTRY"])"#,
    );
    let points: Vec<_> = a.data_points().collect();
    assert_eq!(points.len(), 3);
    assert_eq!(points[0].tier, None, "free messages carry no tier");
    assert_eq!(
        points[0].pricing_type,
        Some(PricingType::FreeCustomerService)
    );
    assert_eq!(points[0].cost, Some(0.0));
    assert_eq!(points[1].tier.as_deref(), Some("0:750000"));
    assert_eq!(
        points[1].pricing_category,
        Some(PricingCategory::AuthenticationInternational)
    );
    assert_eq!(points[1].cost, Some(4.6));
    assert_eq!(
        points[2].pricing_category,
        Some(PricingCategory::MarketingLite)
    );
    assert_eq!(points[2].volume, Some(1));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn pricing_filters_are_encoded() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"pricing_analytics": {"data": []}}));
    let a = client(&t)
        .analytics(WABA)
        .pricing(&PricingAnalyticsQuery {
            phone_numbers: vec!["15550783881".into()],
            metric_types: vec![PricingMetric::Cost, PricingMetric::Volume],
            pricing_types: vec![PricingType::Regular, PricingType::FreeEntryPoint],
            pricing_categories: vec![
                PricingCategory::ReferralConversion,
                PricingCategory::Other("NEW_ONE".into()),
            ],
            ..PricingAnalyticsQuery::new(at(1), at(2), Granularity::Monthly)
        })
        .await
        .unwrap()
        .unwrap();
    assert_get_on_waba(
        &t,
        r#"pricing_analytics.start(1).end(2).granularity(MONTHLY).phone_numbers(["15550783881"]).metric_types(["COST","VOLUME"]).pricing_types(["REGULAR","FREE_ENTRY_POINT"]).pricing_categories(["REFERRAL_CONVERSION","NEW_ONE"])"#,
    );
    assert_eq!(a.data_points().count(), 0);
    assert_eq!(t.remaining(), 0);
}

// ── Calls ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn calls_match_docs_example() {
    let t = ScriptedTransport::new();
    // analytics "Call analytics" example response.
    t.push_json(
        200,
        json!({
            "call_analytics": {
                "granularity": "DAILY",
                "directions": "USER_INITIATED",
                "data_points": [
                    {"start": 1765958400, "end": 1766044800, "cost": 0.47795, "count": 35, "average_duration": 106},
                    {"start": 1760943600, "end": 1761030000, "cost": 0, "count": 20, "average_duration": 103}
                ]
            },
            "id": "102290129340398"
        }),
    );
    let a = client(&t)
        .analytics(WABA)
        .calls(&CallAnalyticsQuery {
            directions: vec![CallDirection::UserInitiated],
            ..CallAnalyticsQuery::new(at(1759302000), at(1767168000), Granularity::Daily)
        })
        .await
        .unwrap()
        .unwrap();
    assert_get_on_waba(
        &t,
        r#"call_analytics.start(1759302000).end(1767168000).granularity(DAILY).directions(["USER_INITIATED"])"#,
    );
    assert_eq!(a.granularity, Some(Granularity::Daily));
    assert_eq!(a.directions, [CallDirection::UserInitiated]);
    assert_eq!(a.data_points.len(), 2);
    assert_eq!(a.data_points[0].count, Some(35));
    assert_eq!(a.data_points[0].cost, Some(0.47795));
    assert_eq!(a.data_points[0].average_duration, Some(106.0));
    assert_eq!(a.data_points[0].start, at(1765958400));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn call_dimensions_are_lowercase_and_metrics_upper() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"call_analytics": {"directions": ["USER_INITIATED", "BUSINESS_INITIATED"], "data_points": [{"start": 1, "end": 2, "direction": "BUSINESS_INITIATED", "country": "US", "phone_number": "15550783881"}]}}),
    );
    let a = client(&t)
        .analytics(WABA)
        .calls(&CallAnalyticsQuery {
            phone_numbers: vec!["15550783881".into()],
            country_codes: vec!["US".into(), "BR".into()],
            directions: vec![
                CallDirection::UserInitiated,
                CallDirection::BusinessInitiated,
            ],
            dimensions: vec![
                CallDimension::Phone,
                CallDimension::Direction,
                CallDimension::Country,
            ],
            metric_types: vec![
                CallMetric::Count,
                CallMetric::Cost,
                CallMetric::AverageDuration,
            ],
            ..CallAnalyticsQuery::new(at(1), at(2), Granularity::HalfHour)
        })
        .await
        .unwrap()
        .unwrap();
    assert_get_on_waba(
        &t,
        r#"call_analytics.start(1).end(2).granularity(HALF_HOUR).phone_numbers(["15550783881"]).country_codes(["US","BR"]).directions(["USER_INITIATED","BUSINESS_INITIATED"]).dimensions(["phone","direction","country"]).metric_types(["COUNT","COST","AVERAGE_DURATION"])"#,
    );
    assert_eq!(a.directions.len(), 2);
    assert_eq!(
        a.data_points[0].direction,
        Some(CallDirection::BusinessInitiated)
    );
    assert_eq!(t.remaining(), 0);
}

// ── Template analytics ──────────────────────────────────────────────────

fn template_docs_response() -> serde_json::Value {
    // analytics "Getting all template analytics" example response.
    json!({
        "data": [{
            "granularity": "DAILY",
            "product_type": "cloud_api",
            "data_points": [
                {
                    "template_id": "1421988012088524",
                    "start": 1718064000,
                    "end": 1718150400,
                    "sent": 1, "delivered": 1, "read": 1,
                    "cost": [
                        {"type": "amount_spent", "value": 0.01},
                        {"type": "cost_per_delivered", "value": 0.01}
                    ]
                },
                {
                    "template_id": "2632273056924580",
                    "start": 1718064000,
                    "end": 1718150400,
                    "sent": 1, "delivered": 1, "read": 1,
                    "clicked": [
                        {"type": "quick_reply_button", "button_content": "Contact Support", "count": 108},
                        {"type": "unique_url_button", "button_content": "Tell me more", "count": 16}
                    ],
                    "cost": [
                        {"type": "amount_spent", "value": 0.03},
                        {"type": "cost_per_delivered", "value": 0.03},
                        {"type": "cost_per_url_button_click", "value": 0.03}
                    ],
                    "website_purchases": 3
                }
            ]
        }],
        "paging": {"cursors": {"before": "MAZDZD", "after": "MjQZD"}}
    })
}

#[tokio::test]
async fn template_matches_docs_example() {
    use TemplateMetric as M;
    let t = ScriptedTransport::new();
    t.push_json(200, template_docs_response());
    let page = client(&t)
        .analytics("109259195336416")
        .template(&TemplateAnalyticsQuery {
            metric_types: vec![M::Cost, M::Clicked, M::Delivered, M::Read, M::Sent],
            ..TemplateAnalyticsQuery::new(
                at(1718064000),
                at(1718122745),
                vec!["1421988012088524".into(), "2632273056924580".into()],
            )
        })
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/109259195336416/template_analytics");
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(req.query("start").as_deref(), Some("1718064000"));
    assert_eq!(req.query("end").as_deref(), Some("1718122745"));
    assert_eq!(req.query("granularity").as_deref(), Some("DAILY"));
    assert_eq!(
        req.query("metric_types").as_deref(),
        Some("COST,CLICKED,DELIVERED,READ,SENT")
    );
    assert_eq!(
        req.query("template_ids").as_deref(),
        Some("[1421988012088524,2632273056924580]")
    );
    assert_eq!(req.query("product_type"), None);
    assert_eq!(req.query("use_waba_timezone"), None);

    let set = &page.data[0];
    assert_eq!(set.granularity, Some(Granularity::Daily));
    assert_eq!(set.product_type.as_deref(), Some("cloud_api"));
    let second = &set.data_points[1];
    assert_eq!(second.template_id, TemplateId::new("2632273056924580"));
    assert_eq!(second.start, AnalyticsTime::Timestamp(at(1718064000)));
    assert_eq!(second.read, Some(1));
    assert_eq!(
        second.clicked[0],
        ClickMetric {
            kind: ClickType::QuickReplyButton,
            button_content: Some("Contact Support".into()),
            count: Some(108),
        }
    );
    assert_eq!(second.clicked[1].kind, ClickType::UniqueUrlButton);
    assert_eq!(second.cost[2].kind, CostType::CostPerUrlButtonClick);
    assert_eq!(second.cost[2].value, Some(0.03));
    assert_eq!(second.other.get("website_purchases"), Some(&json!(3)));
    assert!(set.data_points[0].clicked.is_empty());
    assert_eq!(
        page.paging.unwrap().cursors.unwrap().after.as_deref(),
        Some("MjQZD")
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn template_with_waba_timezone_sends_dates_and_product_type() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"data": [{"waba_timezone": "America/Los_Angeles", "granularity": "DAILY", "product_type": "marketing_messages_api_for_whatsapp", "data_points": [{"template_id": "1", "start": "2024-06-11", "end": "2024-06-12", "sent": 4}]}]}),
    );
    let page = client(&t)
        .analytics(WABA)
        .template(&TemplateAnalyticsQuery {
            product_type: Some(TemplateProductType::MarketingMessagesApiForWhatsapp),
            use_waba_timezone: Some(true),
            ..TemplateAnalyticsQuery::new(
                date!(2024 - 06 - 11),
                date!(2024 - 06 - 12),
                vec!["1".into()],
            )
        })
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.query("start").as_deref(), Some("2024-06-11"));
    assert_eq!(req.query("end").as_deref(), Some("2024-06-12"));
    assert_eq!(
        req.query("product_type").as_deref(),
        Some("MARKETING_MESSAGES_API_FOR_WHATSAPP")
    );
    assert_eq!(req.query("use_waba_timezone").as_deref(), Some("true"));
    assert_eq!(req.query("metric_types"), None, "empty means all metrics");
    let set = &page.data[0];
    assert_eq!(set.waba_timezone.as_deref(), Some("America/Los_Angeles"));
    assert_eq!(
        set.data_points[0].start,
        AnalyticsTime::Date(date!(2024 - 06 - 11))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn template_ids_must_be_1_to_10() {
    let t = ScriptedTransport::new();
    let api = client(&t).analytics(WABA);
    for n in [0, 11] {
        let ids = (0..n).map(|i| TemplateId::new(i.to_string())).collect();
        let q = TemplateAnalyticsQuery::new(at(1), at(2), ids);
        let err = api.template(&q).await.unwrap_err();
        assert_eq!(validation_field(&err), "template_ids", "{n}");
        let items: Vec<_> = api.template_stream(&q).collect().await;
        assert_eq!(items.len(), 1);
        assert_eq!(
            validation_field(items[0].as_ref().unwrap_err()),
            "template_ids"
        );
    }
    assert!(t.requests().is_empty());
    t.push_json(200, json!({"data": []}));
    let ten = (0..10).map(|i| TemplateId::new(i.to_string())).collect();
    api.template(&TemplateAnalyticsQuery::new(at(1), at(2), ten))
        .await
        .unwrap();
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn waba_timezone_requires_date_bounds() {
    let t = ScriptedTransport::new();
    let api = client(&t).analytics(WABA);
    let err = api
        .template(&TemplateAnalyticsQuery {
            use_waba_timezone: Some(true),
            ..TemplateAnalyticsQuery::new(at(1), date!(2024 - 06 - 12), vec!["1".into()])
        })
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "start");
    let err = api
        .template_group(&TemplateGroupAnalyticsQuery {
            use_waba_timezone: Some(true),
            ..TemplateGroupAnalyticsQuery::new(date!(2024 - 06 - 11), at(2), vec!["1".into()])
        })
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "end");
    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn template_stream_follows_cursors() {
    let t = ScriptedTransport::new();
    let mut first = template_docs_response();
    first["paging"]["next"] =
        json!("https://graph.facebook.com/v25.0/x/template_analytics?after=MjQZD");
    t.push_json(200, first);
    t.push_json(
        200,
        json!({"data": [{"granularity": "DAILY", "data_points": []}]}),
    );
    let sets: Vec<TemplateAnalytics> = client(&t)
        .analytics(WABA)
        .template_stream(&TemplateAnalyticsQuery::new(at(1), at(2), vec!["1".into()]))
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(sets.len(), 2);
    let reqs = t.requests();
    assert_eq!(reqs[1].query("after").as_deref(), Some("MjQZD"));
    assert_eq!(reqs[1].query("template_ids").as_deref(), Some("[1]"));
    assert_eq!(t.remaining(), 0);
}

/// Every analytics list takes its cursor in its query (the docs' example
/// responses carry `paging.cursors`): the one-page methods send it, the
/// streams refuse it before any request.
#[tokio::test]
async fn analytics_lists_take_cursors_in_their_query() {
    let t = ScriptedTransport::new();
    for _ in 0..3 {
        t.push_json(200, json!({"data": []}));
    }
    let api = client(&t).analytics(WABA);
    let template = TemplateAnalyticsQuery {
        after: Some("MjQZD".into()),
        ..TemplateAnalyticsQuery::new(at(1), at(2), vec!["1".into()])
    };
    let group_of_templates = TemplateGroupAnalyticsQuery {
        before: Some("MAZDZD".into()),
        ..TemplateGroupAnalyticsQuery::new(at(1), at(2), vec!["7".into()])
    };
    let group = GroupAnalyticsQuery {
        after: Some("MjQZD".into()),
        ..GroupAnalyticsQuery::new(at(1), at(2), vec!["G".into()], vec![GroupMetric::Sent])
    };
    api.template(&template).await.unwrap();
    api.template_group(&group_of_templates).await.unwrap();
    api.groups(&group).await.unwrap();
    let reqs = t.requests();
    assert_eq!(reqs[0].path(), format!("/v25.0/{WABA}/template_analytics"));
    assert_eq!(reqs[0].query("after").as_deref(), Some("MjQZD"));
    assert_eq!(reqs[0].query("before"), None);
    assert_eq!(
        reqs[1].path(),
        format!("/v25.0/{WABA}/template_group_analytics")
    );
    assert_eq!(reqs[1].query("before").as_deref(), Some("MAZDZD"));
    assert_eq!(reqs[2].path(), format!("/v25.0/{WABA}/group_analytics"));
    assert_eq!(reqs[2].query("after").as_deref(), Some("MjQZD"));
    assert_eq!(t.remaining(), 0);

    let first: Vec<_> = api.template_stream(&template).collect().await;
    let second: Vec<_> = api
        .template_group_stream(&group_of_templates)
        .collect()
        .await;
    let third: Vec<_> = api.groups_stream(&group).collect().await;
    assert_eq!(validation_field(first[0].as_ref().unwrap_err()), "after");
    assert_eq!(validation_field(second[0].as_ref().unwrap_err()), "before");
    assert_eq!(validation_field(third[0].as_ref().unwrap_err()), "after");
    assert_eq!((first.len(), second.len(), third.len()), (1, 1, 1));
    assert_eq!(t.requests().len(), 3, "refused before any request");
}

#[tokio::test]
async fn groups_stream_follows_cursors() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"data": [{"granularity": "DAILY", "data_points": []}],
            "paging": {"cursors": {"before": "MAZDZD", "after": "MjQZD"},
                "next": "https://graph.facebook.com/v25.0/x/group_analytics?after=MjQZD"}}),
    );
    t.push_json(
        200,
        json!({"data": [{"granularity": "DAILY", "data_points": []}]}),
    );
    let sets: Vec<GroupAnalytics> = client(&t)
        .analytics(WABA)
        .groups_stream(&GroupAnalyticsQuery::new(
            at(1),
            at(2),
            vec!["G".into()],
            vec![GroupMetric::Sent],
        ))
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(sets.len(), 2);
    let second = &t.requests()[1];
    assert_eq!(second.query("after").as_deref(), Some("MjQZD"));
    assert_eq!(second.query("group_ids").as_deref(), Some(r#"["G"]"#));
    assert_eq!(t.remaining(), 0);
}

// ── Template group analytics ────────────────────────────────────────────

#[tokio::test]
async fn template_group_matches_docs_example() {
    use TemplateGroupMetric as M;
    let t = ScriptedTransport::new();
    // analytics "Template group analytics" example response.
    t.push_json(
        200,
        json!({
            "data": [{
                "granularity": "DAILY",
                "data_points": [
                    {"template_group_id": "1044106240855852", "start": 1739491200, "end": 1739577600, "sent": 1460, "delivered": 1460, "read": 1399},
                    {"template_group_id": "1044106240855852", "start": 1739404800, "end": 1739491200, "sent": 673, "delivered": 673, "read": 645}
                ]
            }],
            "paging": {"cursors": {"before": "MAZDZD", "after": "MjQZD"}}
        }),
    );
    let page = client(&t)
        .analytics(WABA)
        .template_group(&TemplateGroupAnalyticsQuery {
            metric_types: vec![M::Sent, M::Delivered, M::Read],
            ..TemplateGroupAnalyticsQuery::new(
                at(1738465116),
                at(1739559516),
                vec!["1044106240855852".into()],
            )
        })
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(
        req.path(),
        format!("/v25.0/{WABA}/template_group_analytics")
    );
    assert_eq!(req.bearer(), Some("TOKEN"));
    // Same parameters, in the same order, as the docs' example request.
    assert_eq!(
        req.url.query(),
        Some(
            "granularity=daily&start=1738465116&end=1739559516&metric_types=sent%2Cdelivered%2Cread&template_group_ids=%5B1044106240855852%5D"
        )
    );
    let points = &page.data[0].data_points;
    assert_eq!(points.len(), 2);
    assert_eq!(points[0].template_group_id.as_str(), "1044106240855852");
    assert_eq!(points[0].read, Some(1399));
    assert_eq!(points[1].start, AnalyticsTime::Timestamp(at(1739404800)));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn template_group_ids_must_be_1_to_10() {
    let t = ScriptedTransport::new();
    let api = client(&t).analytics(WABA);
    for n in [0, 11] {
        let ids = (0..n)
            .map(|i| TemplateGroupId::new(i.to_string()))
            .collect();
        let q = TemplateGroupAnalyticsQuery::new(at(1), at(2), ids);
        let err = api.template_group(&q).await.unwrap_err();
        assert_eq!(validation_field(&err), "template_group_ids", "{n}");
        let items: Vec<_> = api.template_group_stream(&q).collect().await;
        assert_eq!(
            validation_field(items[0].as_ref().unwrap_err()),
            "template_group_ids"
        );
    }
    assert!(t.requests().is_empty());
    t.push_json(200, json!({"data": []}));
    let ten = (0..10)
        .map(|i| TemplateGroupId::new(i.to_string()))
        .collect();
    api.template_group(&TemplateGroupAnalyticsQuery::new(at(1), at(2), ten))
        .await
        .unwrap();
    assert_eq!(t.remaining(), 0);
}

// ── Group analytics ─────────────────────────────────────────────────────

#[tokio::test]
async fn groups_match_docs_example() {
    use GroupMetric as M;
    let t = ScriptedTransport::new();
    // analytics "Group analytics" example response.
    t.push_json(
        200,
        json!({
            "data": [{
                "granularity": "DAILY",
                "data_points": [
                    {"group_id": "GROUP_ID", "start": 1685548801, "end": 1685635200, "sent": 100, "delivered": 250, "read": 200, "joined": 3, "left": 1},
                    {"group_id": "GROUP_ID", "start": 1685635201, "end": 1685721600, "sent": 80, "delivered": 200, "read": 150, "joined": 1, "left": 0}
                ]
            }],
            "paging": {"cursors": {"before": "MAZDZD", "after": "MjQZD"}}
        }),
    );
    let page = client(&t)
        .analytics(WABA)
        .groups(&GroupAnalyticsQuery::new(
            at(1764662400),
            at(1764921600),
            vec!["GROUP_ID".into()],
            vec![
                M::Sent,
                M::Delivered,
                M::Read,
                M::ParticipantsJoined,
                M::ParticipantsLeft,
            ],
        ))
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), format!("/v25.0/{WABA}/group_analytics"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(req.query("start").as_deref(), Some("1764662400"));
    assert_eq!(req.query("end").as_deref(), Some("1764921600"));
    assert_eq!(req.query("granularity").as_deref(), Some("DAILY"));
    assert_eq!(req.query("group_ids").as_deref(), Some(r#"["GROUP_ID"]"#));
    assert_eq!(
        req.query("metric_types").as_deref(),
        Some(r#"["SENT","DELIVERED","READ","PARTICIPANTS_JOINED","PARTICIPANTS_LEFT"]"#)
    );
    assert_eq!(
        page.data[0].data_points[0],
        GroupDataPoint {
            group_id: GroupId::new("GROUP_ID"),
            start: at(1685548801),
            end: at(1685635200),
            sent: Some(100),
            delivered: Some(250),
            read: Some(200),
            joined: Some(3),
            left: Some(1),
        }
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn group_analytics_needs_one_group_and_a_metric() {
    let t = ScriptedTransport::new();
    let api = client(&t).analytics(WABA);
    for ids in [vec![], vec!["G1".into(), "G2".into()]] {
        let q = GroupAnalyticsQuery::new(at(1), at(2), ids, vec![GroupMetric::Sent]);
        let err = api.groups(&q).await.unwrap_err();
        assert_eq!(validation_field(&err), "group_ids");
        let items: Vec<_> = api.groups_stream(&q).collect().await;
        assert_eq!(
            validation_field(items[0].as_ref().unwrap_err()),
            "group_ids"
        );
    }
    let err = api
        .groups(&GroupAnalyticsQuery::new(
            at(1),
            at(2),
            vec!["G1".into()],
            vec![],
        ))
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "metric_types");
    assert!(t.requests().is_empty());
}

// ── Switches ────────────────────────────────────────────────────────────

#[tokio::test]
async fn enable_template_insights_matches_docs_and_is_idempotent() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    // analytics "Confirming template analytics": the id is a JSON number.
    t.push_json(200, json!({"id": 102290129340398_u64}));
    let c = Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy {
            max_retries: 1,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        })
        .build()
        .unwrap();
    let id = c.analytics(WABA).enable_template_insights().await.unwrap();
    assert_eq!(id, WabaId::new("102290129340398"));
    let reqs = t.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].method, Method::POST);
    assert_eq!(
        reqs[1].url.as_str(),
        format!("https://graph.facebook.com/v25.0/{WABA}?is_enabled_for_insights=true")
    );
    assert_eq!(reqs[1].json(), None);
    assert_eq!(reqs[1].bearer(), Some("TOKEN"));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn set_button_click_tracking_matches_docs() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    client(&t)
        .analytics(WABA)
        .set_button_click_tracking(&TemplateId::new("245435364965041"), true, "marketing")
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(
        req.url.as_str(),
        "https://graph.facebook.com/v25.0/245435364965041?cta_url_link_tracking_opted_out=true&category=marketing"
    );
    assert_eq!(req.json(), None);
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn button_click_tracking_validates_template_and_category() {
    let t = ScriptedTransport::new();
    let api = client(&t).analytics(WABA);
    for bad in ["", ".."] {
        let err = api
            .set_button_click_tracking(&TemplateId::new(bad), true, "marketing")
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "path", "{bad:?}");
    }
    let err = api
        .set_button_click_tracking(&TemplateId::new("1"), false, " ")
        .await
        .unwrap_err();
    assert_eq!(validation_field(&err), "category");
    assert!(t.requests().is_empty());
    // An id with `/` stays one segment: it cannot reach another edge.
    t.push_json(200, json!({"success": true}));
    api.set_button_click_tracking(&TemplateId::new("245/subscribed_apps"), true, "marketing")
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().path(),
        "/v25.0/245%2Fsubscribed_apps"
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn cost_not_available_error_is_decoded() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({"error": {"message": "Cost not available", "type": "OAuthException", "code": 100, "error_user_title": "Cost not available"}}),
    );
    let err = client(&t)
        .analytics(WABA)
        .conversation(&ConversationAnalyticsQuery {
            metric_types: vec![ConversationMetric::Cost],
            ..ConversationAnalyticsQuery::new(at(1), at(2), Granularity::Daily)
        })
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    assert_eq!(
        err.graph().and_then(|g| g.error_user_title.as_deref()),
        Some("Cost not available")
    );
    assert_eq!(t.remaining(), 0);
}

#[test]
fn analytics_time_formats_and_parses() {
    assert_eq!(
        AnalyticsTime::from(at(1718064000)).to_string(),
        "1718064000"
    );
    assert_eq!(
        AnalyticsTime::from(date!(2024 - 06 - 01)).to_string(),
        "2024-06-01"
    );
    let parsed: Vec<AnalyticsTime> =
        serde_json::from_value(json!([1718064000, "1718064000", "2024-06-01"])).unwrap();
    assert_eq!(
        parsed,
        [
            AnalyticsTime::Timestamp(at(1718064000)),
            AnalyticsTime::Timestamp(at(1718064000)),
            AnalyticsTime::Date(date!(2024 - 06 - 01)),
        ]
    );
    assert!(serde_json::from_value::<AnalyticsTime>(json!("2024-13-01")).is_err());
    assert!(serde_json::from_value::<AnalyticsTime>(json!("yesterday")).is_err());
}
