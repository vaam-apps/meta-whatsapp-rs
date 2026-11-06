//! Analytics (scope `analytics`; docs/design/server.md, section 4.2;
//! roadmap M5g, page `analytics`): a WABA's messaging, conversation,
//! pricing, template and template group analytics, and the two switches the
//! page documents.
//!
//! | Route | Graph call, with the tenant's WABA token |
//! | --- | --- |
//! | `GET /v1/wabas/{waba_id}/analytics/messaging` | `GET /{waba_id}?fields=analytics.start(…).end(…).granularity(…)…` |
//! | `GET /v1/wabas/{waba_id}/analytics/conversations` | `GET /{waba_id}?fields=conversation_analytics.start(…).end(…).granularity(…)…` |
//! | `GET /v1/wabas/{waba_id}/analytics/calls` | `GET /{waba_id}?fields=call_analytics.start(…).end(…).granularity(…)…` (roadmap M5e) |
//! | `GET /v1/wabas/{waba_id}/analytics/pricing` | `GET /{waba_id}?fields=pricing_analytics.start(…).end(…).granularity(…)…` |
//! | `GET /v1/wabas/{waba_id}/analytics/templates` | the walk of the WABA's templates (every `template_ids` among them), then `GET /{waba_id}/template_analytics?…` |
//! | `GET /v1/wabas/{waba_id}/analytics/template-groups` | each group read first (its WABA must be this one, D118), then `GET /{waba_id}/template_group_analytics?…` (roadmap M5g2) |
//! | `POST /v1/wabas/{waba_id}/analytics/template-insights` | `POST /{waba_id}?is_enabled_for_insights=true` (roadmap M5g2; a legal act) |
//! | `PUT /v1/wabas/{waba_id}/analytics/templates/{template_id}/click-tracking` | the template found among the WABA's own, then `POST /{template_id}?cta_url_link_tracking_opted_out=…&category=…` (roadmap M5g2) |
//!
//! Times are RFC 3339 (percent-encode a `+` offset), and a template query
//! also takes `YYYY-MM-DD` days. The ranges are bounded (the core's
//! [`meta_whatsapp_server_core::analytics`], D145): the look-back Meta
//! documents (a year; 90 days for templates), and, for `HALF_HOUR`, the
//! service's 31 days. Lists are comma-separated, and each value one of
//! Meta's. Not offered: group analytics (M5f's, on the group's route).
//!
//! **The two switches** (roadmap M5g2, D285, D286) are changes by the
//! caller, each under an `Idempotency-Key`, audited, answering `204` and no
//! text of Meta's. Confirming template insights is a **legal act**: Meta's
//! page says it directs Meta to add link tracking and to collect and
//! anonymize chat data, and that it cannot be disabled. It is `403
//! feature_not_available`, with nothing asked of Meta, until the deployment
//! sets `WA_SERVER_LEGAL_ACTS=on`.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::analytics::{
    self as domain, Analytics, Bound, CallRequest, ConversationRequest, MessagingRequest, Moment,
    PricingRequest, TemplateGroupRequest, TemplateRequest, parse_day,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::ToSchema;

use super::common::{ApiJson, encode_cursor, rfc3339};
use super::listing::{self, csv};
use super::templates::ApiQuery;
use crate::auth::{Caller, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

// ─── The queries ─────────────────────────────────────────────────────────

/// An RFC 3339 time of a query, else `422 invalid_request` on `field`.
fn time_of(field: &str, value: &str) -> Result<OffsetDateTime, ApiError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| ApiError::invalid(field))
}

/// A template query's bound: an RFC 3339 time or a `YYYY-MM-DD` day.
fn bound_of(field: &str, value: &str) -> Result<Bound, ApiError> {
    match parse_day(value) {
        Some(day) => Ok(Bound::Day(day)),
        None => time_of(field, value).map(Bound::At),
    }
}

/// The query of messaging analytics.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct MessagingQuery {
    /// Range start (RFC 3339): at most a year before now.
    pub start: String,
    /// Range end (RFC 3339): after `start`, at most a year later.
    pub end: String,
    /// `HALF_HOUR` (a range of 31 days at most), `DAY` or `MONTH`.
    pub granularity: String,
    /// Comma-separated business phone numbers, digits only (`16505550111`).
    pub phone_numbers: Option<String>,
    /// Comma-separated two-letter country codes.
    pub country_codes: Option<String>,
}

/// The query of conversation analytics.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ConversationQuery {
    /// Range start (RFC 3339): at most a year before now.
    pub start: String,
    /// Range end (RFC 3339): after `start`, at most a year later.
    pub end: String,
    /// `HALF_HOUR` (a range of 31 days at most), `DAILY` or `MONTHLY`.
    pub granularity: String,
    /// Comma-separated business phone numbers, digits only.
    pub phone_numbers: Option<String>,
    /// Comma-separated: `COST`, `CONVERSATION`.
    pub metric_types: Option<String>,
    /// Comma-separated: `AUTHENTICATION`, `MARKETING`, `SERVICE`, `UTILITY`.
    pub conversation_categories: Option<String>,
    /// Comma-separated: `FREE_ENTRY_POINT`, `FREE_TIER`, `REGULAR`.
    pub conversation_types: Option<String>,
    /// Comma-separated: `BUSINESS_INITIATED`, `USER_INITIATED`, `UNKNOWN`.
    pub conversation_directions: Option<String>,
    /// Comma-separated breakdowns: `CONVERSATION_CATEGORY`,
    /// `CONVERSATION_DIRECTION`, `CONVERSATION_TYPE`, `COUNTRY`, `PHONE`.
    pub dimensions: Option<String>,
}

/// The query of pricing analytics.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct PricingQuery {
    /// Range start (RFC 3339): at most a year before now.
    pub start: String,
    /// Range end (RFC 3339): after `start`, at most a year later.
    pub end: String,
    /// `HALF_HOUR` (a range of 31 days at most), `DAILY` or `MONTHLY`.
    pub granularity: String,
    /// Comma-separated business phone numbers, digits only.
    pub phone_numbers: Option<String>,
    /// Comma-separated two-letter country codes.
    pub country_codes: Option<String>,
    /// Comma-separated: `COST`, `VOLUME`.
    pub metric_types: Option<String>,
    /// Comma-separated: `FREE_CUSTOMER_SERVICE`, `FREE_ENTRY_POINT`, `REGULAR`.
    pub pricing_types: Option<String>,
    /// Comma-separated: `AUTHENTICATION`, `AUTHENTICATION_INTERNATIONAL`,
    /// `MARKETING`, `MARKETING_LITE`, `SERVICE`, `UTILITY`,
    /// `REFERRAL_CONVERSION`.
    pub pricing_categories: Option<String>,
    /// Comma-separated breakdowns: `COUNTRY`, `PHONE`, `PRICING_CATEGORY`,
    /// `PRICING_TYPE`, `TIER`.
    pub dimensions: Option<String>,
}

/// The query of call analytics.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CallQuery {
    /// Range start (RFC 3339): at most a year before now.
    pub start: String,
    /// Range end (RFC 3339): after `start`, at most a year later.
    pub end: String,
    /// `HALF_HOUR` (a range of 31 days at most), `DAILY` or `MONTHLY`.
    pub granularity: String,
    /// Comma-separated business phone numbers, digits only.
    pub phone_numbers: Option<String>,
    /// Comma-separated two-letter country codes.
    pub country_codes: Option<String>,
    /// Comma-separated: `USER_INITIATED`, `BUSINESS_INITIATED`.
    pub directions: Option<String>,
    /// Comma-separated breakdowns, lowercase as Meta writes them: `phone`,
    /// `direction`, `country`.
    pub dimensions: Option<String>,
    /// Comma-separated: `COUNT`, `COST`, `AVERAGE_DURATION`.
    pub metric_types: Option<String>,
}

/// The query of template analytics.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct TemplateQuery {
    /// Range start: RFC 3339, or a `YYYY-MM-DD` day (required with
    /// `use_waba_timezone`); at most 90 days before now.
    pub start: String,
    /// Range end, in the same format: after `start`, at most 90 days later.
    pub end: String,
    /// Comma-separated template ids, 1 to 10, each of the WABA's own.
    pub template_ids: String,
    /// Comma-separated: `COST`, `CLICKED`, `DELIVERED`, `READ`, `SENT`
    /// (none: all).
    pub metric_types: Option<String>,
    /// Report in the WABA's timezone (`start` and `end` must be days).
    pub use_waba_timezone: Option<bool>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// The query of template group analytics.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct TemplateGroupQuery {
    /// Range start: RFC 3339, or a `YYYY-MM-DD` day (required with
    /// `use_waba_timezone`); at most 90 days before now.
    pub start: String,
    /// Range end, in the same format: after `start`, at most 90 days later.
    pub end: String,
    /// Comma-separated template group ids, 1 to 10, each of the WABA's own.
    pub group_ids: String,
    /// Comma-separated, lowercase as Meta writes them for this endpoint:
    /// `cost`, `clicked`, `delivered`, `read`, `sent` (none: all).
    pub metric_types: Option<String>,
    /// Report in the WABA's timezone (`start` and `end` must be days).
    pub use_waba_timezone: Option<bool>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// What to set for a template's button click tracking.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = TemplateClickTrackingUpdate)]
pub struct ClickTrackingBody {
    /// `true` turns button click tracking off for the template (the
    /// analytics then answer no clicks for it); `false` turns it back on
    /// (Meta's setting at a template's creation).
    pub opted_out: bool,
}

// ─── The answers ─────────────────────────────────────────────────────────

/// A messaging analytics interval.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = MessagingDataPoint)]
pub struct MessagingPointView {
    /// Interval start (RFC 3339).
    #[schema(format = DateTime)]
    pub start: String,
    /// Interval end (RFC 3339).
    #[schema(format = DateTime)]
    pub end: String,
    /// Messages sent.
    #[schema(required = true)]
    pub sent: Option<u64>,
    /// Messages delivered.
    #[schema(required = true)]
    pub delivered: Option<u64>,
}

/// Messaging analytics.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = MessagingAnalytics)]
pub struct MessagingView {
    /// The business phone numbers included.
    pub phone_numbers: Vec<String>,
    /// The countries included.
    pub country_codes: Vec<String>,
    /// The granularity of the data points, as Meta writes it.
    #[schema(required = true)]
    pub granularity: Option<String>,
    /// The data points (none when Meta has no data for the range).
    pub data_points: Vec<MessagingPointView>,
}

impl From<domain::Messaging> for MessagingView {
    fn from(analytics: domain::Messaging) -> Self {
        Self {
            phone_numbers: analytics.phone_numbers,
            country_codes: analytics.country_codes,
            granularity: analytics.granularity,
            data_points: analytics
                .data_points
                .into_iter()
                .map(|p| MessagingPointView {
                    start: rfc3339(p.start),
                    end: rfc3339(p.end),
                    sent: p.sent,
                    delivered: p.delivered,
                })
                .collect(),
        }
    }
}

/// A conversation analytics interval; a breakdown is `null` unless its
/// dimension was asked for.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ConversationDataPoint)]
pub struct ConversationPointView {
    /// Interval start (RFC 3339).
    #[schema(format = DateTime)]
    pub start: String,
    /// Interval end (RFC 3339).
    #[schema(format = DateTime)]
    pub end: String,
    /// Conversations.
    #[schema(required = true)]
    pub conversation: Option<u64>,
    /// Approximate cost, in the WABA's currency (not returned for a WABA
    /// on a partner's credit line).
    #[schema(required = true)]
    pub cost: Option<f64>,
    /// The business phone number.
    #[schema(required = true)]
    pub phone_number: Option<String>,
    /// The country.
    #[schema(required = true)]
    pub country: Option<String>,
    /// The conversation type, as Meta writes it.
    #[schema(required = true)]
    pub conversation_type: Option<String>,
    /// The conversation direction, as Meta writes it.
    #[schema(required = true)]
    pub conversation_direction: Option<String>,
    /// The conversation category, as Meta writes it.
    #[schema(required = true)]
    pub conversation_category: Option<String>,
}

impl From<domain::ConversationPoint> for ConversationPointView {
    fn from(p: domain::ConversationPoint) -> Self {
        Self {
            start: rfc3339(p.start),
            end: rfc3339(p.end),
            conversation: p.conversation,
            cost: p.cost,
            phone_number: p.phone_number,
            country: p.country,
            conversation_type: p.conversation_type,
            conversation_direction: p.conversation_direction,
            conversation_category: p.conversation_category,
        }
    }
}

/// Conversation analytics.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ConversationAnalytics)]
pub struct ConversationView {
    /// The data points (none when Meta has no data for the range).
    pub data_points: Vec<ConversationPointView>,
}

/// A pricing analytics interval.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = PricingDataPoint)]
pub struct PricingPointView {
    /// Interval start (RFC 3339).
    #[schema(format = DateTime)]
    pub start: String,
    /// Interval end (RFC 3339).
    #[schema(format = DateTime)]
    pub end: String,
    /// The business phone number.
    #[schema(required = true)]
    pub phone_number: Option<String>,
    /// The country.
    #[schema(required = true)]
    pub country: Option<String>,
    /// The volume tier (`0:1000`; the upper bound may be `MAX`).
    #[schema(required = true)]
    pub tier: Option<String>,
    /// The pricing type, as Meta writes it.
    #[schema(required = true)]
    pub pricing_type: Option<String>,
    /// The pricing category, as Meta writes it.
    #[schema(required = true)]
    pub pricing_category: Option<String>,
    /// Messages delivered.
    #[schema(required = true)]
    pub volume: Option<u64>,
    /// Approximate cost, in the WABA's currency.
    #[schema(required = true)]
    pub cost: Option<f64>,
}

impl From<domain::PricingPoint> for PricingPointView {
    fn from(p: domain::PricingPoint) -> Self {
        Self {
            start: rfc3339(p.start),
            end: rfc3339(p.end),
            phone_number: p.phone_number,
            country: p.country,
            tier: p.tier,
            pricing_type: p.pricing_type,
            pricing_category: p.pricing_category,
            volume: p.volume,
            cost: p.cost,
        }
    }
}

/// Pricing analytics.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = PricingAnalytics)]
pub struct PricingView {
    /// The data points (none when Meta has no data for the range).
    pub data_points: Vec<PricingPointView>,
}

/// A call analytics interval.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CallDataPoint)]
pub struct CallPointView {
    /// Interval start (RFC 3339).
    #[schema(format = DateTime)]
    pub start: String,
    /// Interval end (RFC 3339).
    #[schema(format = DateTime)]
    pub end: String,
    /// Calls.
    #[schema(required = true)]
    pub count: Option<u64>,
    /// Approximate cost, in the WABA's currency.
    #[schema(required = true)]
    pub cost: Option<f64>,
    /// Average duration, in seconds.
    #[schema(required = true)]
    pub average_duration: Option<f64>,
    /// The business phone number (the `phone` breakdown).
    #[schema(required = true)]
    pub phone_number: Option<String>,
    /// The country (the `country` breakdown).
    #[schema(required = true)]
    pub country: Option<String>,
    /// `USER_INITIATED` or `BUSINESS_INITIATED` (the `direction`
    /// breakdown).
    #[schema(required = true)]
    pub direction: Option<String>,
}

impl From<domain::CallPoint> for CallPointView {
    fn from(p: domain::CallPoint) -> Self {
        Self {
            start: rfc3339(p.start),
            end: rfc3339(p.end),
            count: p.count,
            cost: p.cost,
            average_duration: p.average_duration,
            phone_number: p.phone_number,
            country: p.country,
            direction: p.direction,
        }
    }
}

/// Call analytics.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CallAnalytics)]
pub struct CallsView {
    /// The data points (none when Meta has no data for the range).
    pub data_points: Vec<CallPointView>,
}

/// A button's clicks.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = TemplateClick)]
pub struct ClickView {
    /// `url_button`, `unique_url_button`, `quick_reply_button`, as Meta
    /// writes it.
    #[serde(rename = "type")]
    pub kind: String,
    /// The button's text.
    #[schema(required = true)]
    pub button_content: Option<String>,
    /// The clicks.
    #[schema(required = true)]
    pub count: Option<u64>,
}

/// A cost metric.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = TemplateCost)]
pub struct CostView {
    /// `amount_spent`, `cost_per_delivered`, `cost_per_url_button_click`,
    /// as Meta writes it.
    #[serde(rename = "type")]
    pub kind: String,
    /// The amount, in the WABA's currency.
    #[schema(required = true)]
    pub value: Option<f64>,
}

/// A template's day of analytics.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = TemplateDataPoint)]
pub struct TemplatePointView {
    /// The template.
    pub template_id: String,
    /// Day start: RFC 3339, or a `YYYY-MM-DD` day.
    pub start: String,
    /// Day end, in the same format.
    pub end: String,
    /// Sends.
    #[schema(required = true)]
    pub sent: Option<u64>,
    /// Deliveries.
    #[schema(required = true)]
    pub delivered: Option<u64>,
    /// Reads (tracked for 7 days after the send).
    #[schema(required = true)]
    pub read: Option<u64>,
    /// Button clicks (marketing and utility templates).
    pub clicked: Vec<ClickView>,
    /// Cost metrics.
    pub cost: Vec<CostView>,
}

/// A set of template analytics.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = TemplateAnalytics)]
pub struct TemplateSetView {
    /// `DAILY`, as Meta writes it.
    #[schema(required = true)]
    pub granularity: Option<String>,
    /// The product the metrics are for (`cloud_api`).
    #[schema(required = true)]
    pub product_type: Option<String>,
    /// The WABA's timezone, with `use_waba_timezone`.
    #[schema(required = true)]
    pub waba_timezone: Option<String>,
    /// The data points.
    pub data_points: Vec<TemplatePointView>,
}

/// A page of template analytics.
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateAnalyticsList {
    /// The sets.
    pub data: Vec<TemplateSetView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// A template group's day of analytics.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = TemplateGroupDataPoint)]
pub struct TemplateGroupPointView {
    /// The template group.
    pub template_group_id: String,
    /// Day start: RFC 3339, or a `YYYY-MM-DD` day.
    pub start: String,
    /// Day end, in the same format.
    pub end: String,
    /// Sends.
    #[schema(required = true)]
    pub sent: Option<u64>,
    /// Deliveries.
    #[schema(required = true)]
    pub delivered: Option<u64>,
    /// Reads.
    #[schema(required = true)]
    pub read: Option<u64>,
    /// Button clicks (marketing and utility templates).
    pub clicked: Vec<ClickView>,
    /// Cost metrics.
    pub cost: Vec<CostView>,
}

/// A set of template group analytics.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = TemplateGroupAnalytics)]
pub struct TemplateGroupSetView {
    /// `DAILY`, as Meta writes it.
    #[schema(required = true)]
    pub granularity: Option<String>,
    /// The product the metrics are for.
    #[schema(required = true)]
    pub product_type: Option<String>,
    /// The WABA's timezone, with `use_waba_timezone`.
    #[schema(required = true)]
    pub waba_timezone: Option<String>,
    /// The data points.
    pub data_points: Vec<TemplateGroupPointView>,
}

/// A page of template group analytics.
#[derive(Debug, Serialize, ToSchema)]
pub struct TemplateGroupAnalyticsList {
    /// The sets.
    pub data: Vec<TemplateGroupSetView>,
    /// Pass as `cursor` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

impl From<domain::TemplateGroupPage> for TemplateGroupAnalyticsList {
    fn from(page: domain::TemplateGroupPage) -> Self {
        Self {
            data: page
                .sets
                .into_iter()
                .map(|set| TemplateGroupSetView {
                    granularity: set.granularity,
                    product_type: set.product_type,
                    waba_timezone: set.waba_timezone,
                    data_points: set
                        .data_points
                        .into_iter()
                        .map(|p| TemplateGroupPointView {
                            template_group_id: p.template_group_id,
                            start: moment(p.start),
                            end: moment(p.end),
                            sent: p.sent,
                            delivered: p.delivered,
                            read: p.read,
                            clicked: p
                                .clicked
                                .into_iter()
                                .map(|c| ClickView {
                                    kind: c.kind,
                                    button_content: c.button_content,
                                    count: c.count,
                                })
                                .collect(),
                            cost: p
                                .cost
                                .into_iter()
                                .map(|c| CostView {
                                    kind: c.kind,
                                    value: c.value,
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
            next_cursor: page.next_after.as_deref().map(encode_cursor),
        }
    }
}

fn moment(moment: Moment) -> String {
    match moment {
        Moment::At(at) => rfc3339(at),
        Moment::Day(day) => format!(
            "{:04}-{:02}-{:02}",
            day.year(),
            u8::from(day.month()),
            day.day()
        ),
    }
}

impl From<domain::TemplatePage> for TemplateAnalyticsList {
    fn from(page: domain::TemplatePage) -> Self {
        Self {
            data: page
                .sets
                .into_iter()
                .map(|set| TemplateSetView {
                    granularity: set.granularity,
                    product_type: set.product_type,
                    waba_timezone: set.waba_timezone,
                    data_points: set
                        .data_points
                        .into_iter()
                        .map(|p| TemplatePointView {
                            template_id: p.template_id,
                            start: moment(p.start),
                            end: moment(p.end),
                            sent: p.sent,
                            delivered: p.delivered,
                            read: p.read,
                            clicked: p
                                .clicked
                                .into_iter()
                                .map(|c| ClickView {
                                    kind: c.kind,
                                    button_content: c.button_content,
                                    count: c.count,
                                })
                                .collect(),
                            cost: p
                                .cost
                                .into_iter()
                                .map(|c| CostView {
                                    kind: c.kind,
                                    value: c.value,
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
            next_cursor: page.next_after.as_deref().map(encode_cursor),
        }
    }
}

// ─── The routes ──────────────────────────────────────────────────────────

fn analytics<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedWaba,
) -> Result<Analytics<'a>, ApiError> {
    Ok(
        Analytics::new(state.authz(), caller, owned.capability(state))?
            .observed_by(state.metrics())
            .with_legal_acts(state.settings().legal_acts),
    )
}

/// `GET /v1/wabas/{waba_id}/analytics/messaging`: messages sent and
/// delivered.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/analytics/messaging",
    tag = "analytics",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        MessagingQuery,
    ),
    responses(
        (status = 200, description = "The analytics", body = MessagingView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `start`, `end`, `granularity`, `phone_numbers[i]`, `country_codes[i]` or `query`: nothing asked of Meta", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_messaging_analytics(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(query): ApiQuery<MessagingQuery>,
) -> Result<Json<MessagingView>, ApiError> {
    let request = MessagingRequest {
        start: time_of("start", &query.start)?,
        end: time_of("end", &query.end)?,
        granularity: query.granularity,
        phone_numbers: csv(query.phone_numbers.as_deref()),
        country_codes: csv(query.country_codes.as_deref()),
    };
    let analytics = analytics(&state, &caller, &owned)?
        .messaging(&request)
        .await?;
    Ok(Json(MessagingView::from(analytics)))
}

/// `GET /v1/wabas/{waba_id}/analytics/conversations`: conversations and
/// their cost.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/analytics/conversations",
    tag = "analytics",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ConversationQuery,
    ),
    responses(
        (status = 200, description = "The analytics", body = ConversationView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `start`, `end`, `granularity`, a filter (`name[i]` for a value) or `query`: nothing asked of Meta", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_conversation_analytics(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(query): ApiQuery<ConversationQuery>,
) -> Result<Json<ConversationView>, ApiError> {
    let request = ConversationRequest {
        start: time_of("start", &query.start)?,
        end: time_of("end", &query.end)?,
        granularity: query.granularity,
        phone_numbers: csv(query.phone_numbers.as_deref()),
        metric_types: csv(query.metric_types.as_deref()),
        conversation_categories: csv(query.conversation_categories.as_deref()),
        conversation_types: csv(query.conversation_types.as_deref()),
        conversation_directions: csv(query.conversation_directions.as_deref()),
        dimensions: csv(query.dimensions.as_deref()),
    };
    let points = analytics(&state, &caller, &owned)?
        .conversation(&request)
        .await?;
    Ok(Json(ConversationView {
        data_points: points
            .into_iter()
            .map(ConversationPointView::from)
            .collect(),
    }))
}

/// `GET /v1/wabas/{waba_id}/analytics/pricing`: delivered messages by
/// pricing category and type.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/analytics/pricing",
    tag = "analytics",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        PricingQuery,
    ),
    responses(
        (status = 200, description = "The analytics", body = PricingView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `start`, `end`, `granularity`, a filter (`name[i]` for a value) or `query`: nothing asked of Meta", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_pricing_analytics(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(query): ApiQuery<PricingQuery>,
) -> Result<Json<PricingView>, ApiError> {
    let request = PricingRequest {
        start: time_of("start", &query.start)?,
        end: time_of("end", &query.end)?,
        granularity: query.granularity,
        phone_numbers: csv(query.phone_numbers.as_deref()),
        country_codes: csv(query.country_codes.as_deref()),
        metric_types: csv(query.metric_types.as_deref()),
        pricing_types: csv(query.pricing_types.as_deref()),
        pricing_categories: csv(query.pricing_categories.as_deref()),
        dimensions: csv(query.dimensions.as_deref()),
    };
    let points = analytics(&state, &caller, &owned)?
        .pricing(&request)
        .await?;
    Ok(Json(PricingView {
        data_points: points.into_iter().map(PricingPointView::from).collect(),
    }))
}

/// `GET /v1/wabas/{waba_id}/analytics/calls`: calls made and received.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/analytics/calls",
    tag = "analytics",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        CallQuery,
    ),
    responses(
        (status = 200, description = "The analytics", body = CallsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `start`, `end`, `granularity`, a filter (`name[i]` for a value) or `query`: nothing asked of Meta", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_call_analytics(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(query): ApiQuery<CallQuery>,
) -> Result<Json<CallsView>, ApiError> {
    let request = CallRequest {
        start: time_of("start", &query.start)?,
        end: time_of("end", &query.end)?,
        granularity: query.granularity,
        phone_numbers: csv(query.phone_numbers.as_deref()),
        country_codes: csv(query.country_codes.as_deref()),
        directions: csv(query.directions.as_deref()),
        dimensions: csv(query.dimensions.as_deref()),
        metric_types: csv(query.metric_types.as_deref()),
    };
    let points = analytics(&state, &caller, &owned)?.calls(&request).await?;
    Ok(Json(CallsView {
        data_points: points.into_iter().map(CallPointView::from).collect(),
    }))
}

/// `GET /v1/wabas/{waba_id}/analytics/templates`: sends, deliveries, reads,
/// clicks and cost per template, daily.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/analytics/templates",
    tag = "analytics",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        TemplateQuery,
    ),
    responses(
        (status = 200, description = "A page of analytics", body = TemplateAnalyticsList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or a template not among its own (the analytics are not asked for)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `start`, `end`, `template_ids` (`[i]` for an id), `metric_types[i]`, `cursor` or `query`: nothing asked of Meta", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_template_analytics(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(query): ApiQuery<TemplateQuery>,
) -> Result<Json<TemplateAnalyticsList>, ApiError> {
    let request = TemplateRequest {
        start: bound_of("start", &query.start)?,
        end: bound_of("end", &query.end)?,
        template_ids: csv(Some(&query.template_ids)),
        metric_types: csv(query.metric_types.as_deref()),
        use_waba_timezone: query.use_waba_timezone,
        after: listing::after(query.cursor.as_deref())?,
    };
    let page = analytics(&state, &caller, &owned)?
        .template(&request)
        .await?;
    Ok(Json(TemplateAnalyticsList::from(page)))
}

/// `GET /v1/wabas/{waba_id}/analytics/template-groups`: sends, deliveries,
/// reads, clicks and cost per template group, daily.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/analytics/template-groups",
    tag = "analytics",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        TemplateGroupQuery,
    ),
    responses(
        (status = 200, description = "A page of analytics", body = TemplateGroupAnalyticsList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or a group not among its own (the analytics are not asked for)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `start`, `end`, `group_ids` (`[i]` for an id), `metric_types[i]`, `cursor` or `query`: nothing asked of Meta", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_template_group_analytics(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(query): ApiQuery<TemplateGroupQuery>,
) -> Result<Json<TemplateGroupAnalyticsList>, ApiError> {
    let request = TemplateGroupRequest {
        start: bound_of("start", &query.start)?,
        end: bound_of("end", &query.end)?,
        group_ids: csv(Some(&query.group_ids)),
        metric_types: csv(query.metric_types.as_deref()),
        use_waba_timezone: query.use_waba_timezone,
        after: listing::after(query.cursor.as_deref())?,
    };
    let page = analytics(&state, &caller, &owned)?
        .template_groups(&request)
        .await?;
    Ok(Json(TemplateGroupAnalyticsList::from(page)))
}

async fn enable_insights(service: &Analytics<'_>) -> Result<Success, ApiError> {
    service.enable_template_insights().await?;
    Ok(Success::no_content())
}

/// `POST /v1/wabas/{waba_id}/analytics/template-insights`: confirm template
/// insights for the WABA. **A legal act** (Meta's page: the business
/// directs Meta to add link tracking and to collect and anonymize data from
/// its chats; it cannot be disabled): `403 feature_not_available` until the
/// deployment sets `WA_SERVER_LEGAL_ACTS=on`. Audited.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/analytics/template-insights",
    tag = "analytics",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key never confirms twice"),
    ),
    responses(
        (status = 204, description = "Confirmed"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, `feature_not_available` (the legal acts are off: nothing was asked of Meta), or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: it may have been confirmed", body = ErrorBody),
    )
)]
pub(crate) async fn enable_template_insights(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
) -> Response {
    let fingerprint = Fingerprint::enable_template_insights(owned.waba_id());
    let service = match analytics(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, enable_insights(&service))
        .await
}

async fn set_click_tracking(
    service: &Analytics<'_>,
    template_id: &str,
    opted_out: bool,
) -> Result<Success, ApiError> {
    service
        .set_template_click_tracking(template_id, opted_out)
        .await?;
    Ok(Success::no_content())
}

/// `PUT /v1/wabas/{waba_id}/analytics/templates/{template_id}/click-tracking`:
/// opt a template of the WABA out of, or back into, button click tracking.
/// Audited.
#[utoipa::path(
    put,
    path = "/v1/wabas/{waba_id}/analytics/templates/{template_id}/click-tracking",
    tag = "analytics",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("template_id" = String, Path, description = "Template id: one of the WABA's"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never change the template twice"),
    ),
    request_body = ClickTrackingBody,
    responses(
        (status = 204, description = "Set"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such template in this WABA; nothing changed", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `template_id` or `body`; `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: read the template", body = ErrorBody),
    )
)]
pub(crate) async fn set_template_click_tracking(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, template_id)): Path<(String, String)>,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<serde_json::Value>,
) -> Response {
    let fingerprint =
        Fingerprint::set_template_click_tracking(owned.waba_id(), &template_id, &body);
    let Ok(ClickTrackingBody { opted_out }) = serde_json::from_value(body) else {
        return ApiError::invalid("body").into_response();
    };
    let service = match analytics(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(
            &caller,
            key,
            fingerprint,
            set_click_tracking(&service, &template_id, opted_out),
        )
        .await
}

#[cfg(test)]
mod tests {
    use time::macros::date;

    use super::*;

    /// A query's times are RFC 3339, a template's also a day; anything
    /// else is `422` on the field. Decisive: each form.
    #[test]
    fn times_are_rfc_3339_and_a_template_bound_may_be_a_day() {
        assert!(time_of("start", "2026-11-01T00:00:00Z").is_ok());
        assert!(time_of("start", "1793836800").is_err(), "unix seconds");
        assert!(time_of("start", "2026-11-01").is_err(), "a day");
        assert_eq!(time_of("end", "x").unwrap_err().field(), Some("end"));
        assert_eq!(
            bound_of("start", "2026-11-01").unwrap(),
            Bound::Day(date!(2026 - 11 - 01))
        );
        assert!(matches!(
            bound_of("start", "2026-11-01T00:00:00Z").unwrap(),
            Bound::At(_)
        ));
        assert_eq!(moment(Moment::Day(date!(2026 - 11 - 01))), "2026-11-01");
    }
}
