//! Analytics (docs/design/server.md, section 4.2; roadmap M5g), as services
//! an API adapter calls, with the tenant's WABA token, over the library's
//! `client::analytics`:
//!
//! | Operation | Graph call |
//! | --- | --- |
//! | [`Analytics::messaging`] | `GET /{waba_id}?fields=analytics.start(…).end(…).granularity(…)[.phone_numbers([…])][.country_codes([…])]` |
//! | [`Analytics::conversation`] | `GET /{waba_id}?fields=conversation_analytics.start(…).end(…).granularity(…)[.phone_numbers([…])]…` |
//! | [`Analytics::pricing`] | `GET /{waba_id}?fields=pricing_analytics.start(…).end(…).granularity(…)[.phone_numbers([…])]…` |
//! | [`Analytics::template`] | the walk of the WABA's templates (`GET /{waba_id}/message_templates?fields=id&limit=100`, as D116), then `GET /{waba_id}/template_analytics?start=…&end=…&granularity=DAILY&template_ids=[…]` |
//!
//! Page: `analytics`. Not offered: call analytics (roadmap M5e's), group
//! analytics (M5f's), template group analytics (they need the groups'
//! ids: not routed yet), and the two switches the page documents
//! (`enable_template_insights`, button click tracking), which change the
//! WABA's settings rather than read.
//!
//! **A WABA's analytics are its WABA's.** The services open only on a WABA
//! of the [`Caller`]'s own tenant (step 4 of the authorization order,
//! [`crate::authz`]); the field expansions are on the WABA's own node. A
//! `phone_numbers` filter names numbers by their digits (Meta's examples:
//! `16505550111`) and can only narrow the WABA's own: the page says only
//! the numbers of the account at the time of the request are counted. The
//! template ids of a template query are not Meta's alone: one token may
//! reach several tenants' WABAs, so each must be among the WABA's own
//! templates (the walk of D116) or the query is `404 not_found`, without
//! Meta's code or text, before the analytics are asked for.
//!
//! **Ranges are bounded** (D145), by Meta's documented look-back, and the
//! service's own cap on an answer's size. The page: the look-back of
//! messaging, conversation and pricing analytics is one year, that of
//! template analytics 90 days. So `start` is at most one year (90 days
//! for templates) before now, `end` is after `start`, and the range is at
//! most a year (90 days); with `HALF_HOUR` granularity, which answers a
//! data point per half hour and dimension, at most 31 days: **the service's
//! bound, not Meta's** (the library's `analytics` module documents answers
//! of tens of MiB for a long half-hour range). Lists (`phone_numbers`,
//! `country_codes`, the enum filters) are at most 50 long. Every
//! enum-valued filter is one of the page's values, and a phone number
//! filter is digits: they are written into a field expansion, whose
//! punctuation a stray value could close.

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::analytics::Granularity;
use meta_whatsapp_rs::client::analytics::{
    AnalyticsTime, ConversationAnalytics, ConversationAnalyticsQuery, ConversationDataPoint,
    MAX_TEMPLATE_IDS, MessagingAnalytics, MessagingAnalyticsQuery, MessagingGranularity,
    PricingAnalytics, PricingAnalyticsQuery, PricingDataPoint, TemplateAnalytics,
    TemplateAnalyticsQuery,
};
use meta_whatsapp_rs::core::ids::TemplateId;
use time::{Date, Duration, Month, OffsetDateTime, Time};

use crate::authz::{Authorizer, Caller, OwnedWaba};
use crate::error::ServiceError;
use crate::observe::Observer;
use crate::request::{typed_graph_id, wire};
use crate::templates::all_templates_in_waba;

/// Longest list of a filter: the service's bound (D145).
pub const MAX_FILTER_ITEMS: usize = 50;

/// Longest range of a `HALF_HOUR` query, in days: the service's bound on
/// an answer's size, not Meta's (D145).
pub const HALF_HOUR_MAX_DAYS: i64 = 31;

/// The look-back of template analytics, in days (`analytics`: 90 days).
pub const TEMPLATE_LOOKBACK_DAYS: i64 = 90;

/// Longest phone number of a filter, in digits.
const MAX_PHONE_DIGITS: usize = 20;

const MESSAGING_GRANULARITIES: [&str; 3] = ["HALF_HOUR", "DAY", "MONTH"];
const GRANULARITIES: [&str; 3] = ["HALF_HOUR", "DAILY", "MONTHLY"];
const CONVERSATION_METRICS: [&str; 2] = ["COST", "CONVERSATION"];
const CONVERSATION_CATEGORIES: [&str; 4] = ["AUTHENTICATION", "MARKETING", "SERVICE", "UTILITY"];
const CONVERSATION_TYPES: [&str; 3] = ["FREE_ENTRY_POINT", "FREE_TIER", "REGULAR"];
const CONVERSATION_DIRECTIONS: [&str; 3] = ["BUSINESS_INITIATED", "USER_INITIATED", "UNKNOWN"];
const CONVERSATION_DIMENSIONS: [&str; 5] = [
    "CONVERSATION_CATEGORY",
    "CONVERSATION_DIRECTION",
    "CONVERSATION_TYPE",
    "COUNTRY",
    "PHONE",
];
const PRICING_METRICS: [&str; 2] = ["COST", "VOLUME"];
const PRICING_TYPES: [&str; 3] = ["FREE_CUSTOMER_SERVICE", "FREE_ENTRY_POINT", "REGULAR"];
const PRICING_CATEGORIES: [&str; 7] = [
    "AUTHENTICATION",
    "AUTHENTICATION_INTERNATIONAL",
    "MARKETING",
    "MARKETING_LITE",
    "SERVICE",
    "UTILITY",
    "REFERRAL_CONVERSION",
];
const PRICING_DIMENSIONS: [&str; 5] = [
    "COUNTRY",
    "PHONE",
    "PRICING_CATEGORY",
    "PRICING_TYPE",
    "TIER",
];
const TEMPLATE_METRICS: [&str; 5] = ["COST", "CLICKED", "DELIVERED", "READ", "SENT"];

// ─── The requests ────────────────────────────────────────────────────────

/// A messaging analytics request, as the caller carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagingRequest {
    /// Range start.
    pub start: OffsetDateTime,
    /// Range end.
    pub end: OffsetDateTime,
    /// `HALF_HOUR`, `DAY` or `MONTH`.
    pub granularity: String,
    /// Only these business phone numbers (digits).
    pub phone_numbers: Vec<String>,
    /// Only these countries (two letters).
    pub country_codes: Vec<String>,
}

/// A conversation analytics request, as the caller carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRequest {
    /// Range start.
    pub start: OffsetDateTime,
    /// Range end.
    pub end: OffsetDateTime,
    /// `HALF_HOUR`, `DAILY` or `MONTHLY`.
    pub granularity: String,
    /// Only these business phone numbers (digits).
    pub phone_numbers: Vec<String>,
    /// `COST`, `CONVERSATION`.
    pub metric_types: Vec<String>,
    /// `AUTHENTICATION`, `MARKETING`, `SERVICE`, `UTILITY`.
    pub conversation_categories: Vec<String>,
    /// `FREE_ENTRY_POINT`, `FREE_TIER`, `REGULAR`.
    pub conversation_types: Vec<String>,
    /// `BUSINESS_INITIATED`, `USER_INITIATED`, `UNKNOWN`.
    pub conversation_directions: Vec<String>,
    /// The breakdowns.
    pub dimensions: Vec<String>,
}

/// A pricing analytics request, as the caller carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingRequest {
    /// Range start.
    pub start: OffsetDateTime,
    /// Range end.
    pub end: OffsetDateTime,
    /// `HALF_HOUR`, `DAILY` or `MONTHLY`.
    pub granularity: String,
    /// Only these business phone numbers (digits).
    pub phone_numbers: Vec<String>,
    /// Only these countries (two letters).
    pub country_codes: Vec<String>,
    /// `COST`, `VOLUME`.
    pub metric_types: Vec<String>,
    /// `FREE_CUSTOMER_SERVICE`, `FREE_ENTRY_POINT`, `REGULAR`.
    pub pricing_types: Vec<String>,
    /// The pricing categories.
    pub pricing_categories: Vec<String>,
    /// The breakdowns.
    pub dimensions: Vec<String>,
}

/// A bound of a template analytics range: a moment, or a calendar day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// A moment (Meta rounds it down to 00:00 UTC).
    At(OffsetDateTime),
    /// A calendar day (`YYYY-MM-DD`), required with `use_waba_timezone`.
    Day(Date),
}

impl Bound {
    fn moment(self) -> OffsetDateTime {
        match self {
            Self::At(at) => at,
            Self::Day(day) => day.with_time(Time::MIDNIGHT).assume_utc(),
        }
    }
}

impl From<Bound> for AnalyticsTime {
    fn from(bound: Bound) -> Self {
        match bound {
            Bound::At(at) => Self::Timestamp(at),
            Bound::Day(day) => Self::Date(day),
        }
    }
}

/// A template analytics request, as the caller carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateRequest {
    /// Range start.
    pub start: Bound,
    /// Range end.
    pub end: Bound,
    /// 1 to 10 template ids (the WABA's).
    pub template_ids: Vec<String>,
    /// `COST`, `CLICKED`, `DELIVERED`, `READ`, `SENT`; none: all.
    pub metric_types: Vec<String>,
    /// Report in the WABA's timezone (the bounds must then be days).
    pub use_waba_timezone: Option<bool>,
    /// Meta's cursor from the previous page.
    pub after: Option<String>,
}

fn invalid(field: &str) -> ServiceError {
    ServiceError::invalid(field)
}

/// A list filter: at most [`MAX_FILTER_ITEMS`] long, each value one of
/// `allowed` (`field[i]` names the one at fault).
fn one_of(field: &str, values: &[String], allowed: &[&str]) -> Result<(), ServiceError> {
    if values.len() > MAX_FILTER_ITEMS {
        return Err(invalid(field));
    }
    match values.iter().position(|v| !allowed.contains(&v.as_str())) {
        Some(i) => Err(invalid(&format!("{field}[{i}]"))),
        None => Ok(()),
    }
}

/// A granularity: one of `allowed`.
fn granularity(value: &str, allowed: &[&str]) -> Result<(), ServiceError> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(invalid("granularity"))
    }
}

/// A phone-number filter: digits only, 1 to 20 of them, no more than
/// [`MAX_FILTER_ITEMS`].
fn phone_numbers(values: &[String]) -> Result<(), ServiceError> {
    if values.len() > MAX_FILTER_ITEMS {
        return Err(invalid("phone_numbers"));
    }
    let bad = values.iter().position(|v| {
        v.is_empty() || v.len() > MAX_PHONE_DIGITS || !v.bytes().all(|b| b.is_ascii_digit())
    });
    match bad {
        Some(i) => Err(invalid(&format!("phone_numbers[{i}]"))),
        None => Ok(()),
    }
}

/// A country filter: two ASCII letters each, no more than
/// [`MAX_FILTER_ITEMS`].
fn country_codes(values: &[String]) -> Result<(), ServiceError> {
    if values.len() > MAX_FILTER_ITEMS {
        return Err(invalid("country_codes"));
    }
    let bad = values
        .iter()
        .position(|v| v.len() != 2 || !v.bytes().all(|b| b.is_ascii_alphabetic()));
    match bad {
        Some(i) => Err(invalid(&format!("country_codes[{i}]"))),
        None => Ok(()),
    }
}

/// One year before `now`, by the calendar (365 days on a 29 February).
fn a_year_before(now: OffsetDateTime) -> OffsetDateTime {
    now.replace_year(now.year() - 1)
        .unwrap_or(now - Duration::days(365))
}

/// The range of messaging, conversation and pricing analytics, bounded
/// (see the [module](self)): `start` within a year of `now`, `end` after
/// it, at most a year, and `HALF_HOUR` at most [`HALF_HOUR_MAX_DAYS`].
fn year_range(
    now: OffsetDateTime,
    start: OffsetDateTime,
    end: OffsetDateTime,
    granularity: &str,
) -> Result<(), ServiceError> {
    if start < a_year_before(now) {
        return Err(invalid("start"));
    }
    if end <= start {
        return Err(invalid("end"));
    }
    // A year, from the later of `start` and a year ago: end - start is
    // measured from `start` itself, so a leap year's 366 days pass.
    if end - start > Duration::days(366) {
        return Err(invalid("end"));
    }
    if granularity == "HALF_HOUR" && end - start > Duration::days(HALF_HOUR_MAX_DAYS) {
        return Err(invalid("end"));
    }
    Ok(())
}

/// The library's messaging query for `request`, checked against `now`.
///
/// # Errors
///
/// `422 invalid_request` on the field at fault (`start`, `end`,
/// `granularity`, `phone_numbers[i]`, `country_codes[i]`).
pub fn messaging_query(
    now: OffsetDateTime,
    request: &MessagingRequest,
) -> Result<MessagingAnalyticsQuery, ServiceError> {
    granularity(&request.granularity, &MESSAGING_GRANULARITIES)?;
    year_range(now, request.start, request.end, &request.granularity)?;
    phone_numbers(&request.phone_numbers)?;
    country_codes(&request.country_codes)?;
    let mut query = MessagingAnalyticsQuery::new(
        request.start,
        request.end,
        MessagingGranularity::from(request.granularity.as_str()),
    );
    query.phone_numbers.clone_from(&request.phone_numbers);
    query.country_codes.clone_from(&request.country_codes);
    Ok(query)
}

fn typed<T: for<'a> From<&'a str>>(values: &[String]) -> Vec<T> {
    values.iter().map(|v| T::from(v.as_str())).collect()
}

/// The library's conversation query for `request`, checked against `now`.
///
/// # Errors
///
/// `422 invalid_request` on the field at fault.
pub fn conversation_query(
    now: OffsetDateTime,
    request: &ConversationRequest,
) -> Result<ConversationAnalyticsQuery, ServiceError> {
    granularity(&request.granularity, &GRANULARITIES)?;
    year_range(now, request.start, request.end, &request.granularity)?;
    phone_numbers(&request.phone_numbers)?;
    one_of("metric_types", &request.metric_types, &CONVERSATION_METRICS)?;
    one_of(
        "conversation_categories",
        &request.conversation_categories,
        &CONVERSATION_CATEGORIES,
    )?;
    one_of(
        "conversation_types",
        &request.conversation_types,
        &CONVERSATION_TYPES,
    )?;
    one_of(
        "conversation_directions",
        &request.conversation_directions,
        &CONVERSATION_DIRECTIONS,
    )?;
    one_of("dimensions", &request.dimensions, &CONVERSATION_DIMENSIONS)?;
    let mut query = ConversationAnalyticsQuery::new(
        request.start,
        request.end,
        Granularity::from(request.granularity.as_str()),
    );
    query.phone_numbers.clone_from(&request.phone_numbers);
    query.metric_types = typed(&request.metric_types);
    query.conversation_categories = typed(&request.conversation_categories);
    query.conversation_types = typed(&request.conversation_types);
    query.conversation_directions = typed(&request.conversation_directions);
    query.dimensions = typed(&request.dimensions);
    Ok(query)
}

/// The library's pricing query for `request`, checked against `now`.
///
/// # Errors
///
/// `422 invalid_request` on the field at fault.
pub fn pricing_query(
    now: OffsetDateTime,
    request: &PricingRequest,
) -> Result<PricingAnalyticsQuery, ServiceError> {
    granularity(&request.granularity, &GRANULARITIES)?;
    year_range(now, request.start, request.end, &request.granularity)?;
    phone_numbers(&request.phone_numbers)?;
    country_codes(&request.country_codes)?;
    one_of("metric_types", &request.metric_types, &PRICING_METRICS)?;
    one_of("pricing_types", &request.pricing_types, &PRICING_TYPES)?;
    one_of(
        "pricing_categories",
        &request.pricing_categories,
        &PRICING_CATEGORIES,
    )?;
    one_of("dimensions", &request.dimensions, &PRICING_DIMENSIONS)?;
    let mut query = PricingAnalyticsQuery::new(
        request.start,
        request.end,
        Granularity::from(request.granularity.as_str()),
    );
    query.phone_numbers.clone_from(&request.phone_numbers);
    query.country_codes.clone_from(&request.country_codes);
    query.metric_types = typed(&request.metric_types);
    query.pricing_types = typed(&request.pricing_types);
    query.pricing_categories = typed(&request.pricing_categories);
    query.dimensions = typed(&request.dimensions);
    Ok(query)
}

/// The library's template query for `request`, checked against `now`; the
/// template ids are Meta ids here (that they are the WABA's is
/// [`Analytics::template`]'s check).
///
/// # Errors
///
/// `422 invalid_request` on the field at fault (`start`, `end`,
/// `template_ids`, `template_ids[i]`, `metric_types[i]`, `cursor`).
pub fn template_query(
    now: OffsetDateTime,
    request: &TemplateRequest,
) -> Result<TemplateAnalyticsQuery, ServiceError> {
    let (start, end) = (request.start.moment(), request.end.moment());
    if start < now - Duration::days(TEMPLATE_LOOKBACK_DAYS) {
        return Err(invalid("start"));
    }
    if end <= start || end - start > Duration::days(TEMPLATE_LOOKBACK_DAYS) {
        return Err(invalid("end"));
    }
    if request.use_waba_timezone == Some(true) {
        for (field, bound) in [("start", request.start), ("end", request.end)] {
            if !matches!(bound, Bound::Day(_)) {
                return Err(invalid(field));
            }
        }
    }
    if request.template_ids.is_empty() || request.template_ids.len() > MAX_TEMPLATE_IDS {
        return Err(invalid("template_ids"));
    }
    let mut ids = Vec::with_capacity(request.template_ids.len());
    for (i, id) in request.template_ids.iter().enumerate() {
        let id = typed_graph_id::<TemplateId>("template_ids", id)
            .map_err(|_| invalid(&format!("template_ids[{i}]")))?;
        ids.push(id);
    }
    one_of("metric_types", &request.metric_types, &TEMPLATE_METRICS)?;
    let mut query = TemplateAnalyticsQuery::new(request.start, request.end, ids);
    query.metric_types = typed(&request.metric_types);
    query.use_waba_timezone = request.use_waba_timezone;
    query.after.clone_from(&request.after);
    Ok(query)
}

// ─── The answers ─────────────────────────────────────────────────────────

/// Messaging analytics: sent and delivered counts per interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Messaging {
    /// The numbers included.
    pub phone_numbers: Vec<String>,
    /// The countries included.
    pub country_codes: Vec<String>,
    /// The granularity of the data points, as Meta writes it.
    pub granularity: Option<String>,
    /// The data points.
    pub data_points: Vec<MessagingPoint>,
}

/// An interval of messaging analytics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagingPoint {
    /// Interval start.
    pub start: OffsetDateTime,
    /// Interval end.
    pub end: OffsetDateTime,
    /// Messages sent.
    pub sent: Option<u64>,
    /// Messages delivered.
    pub delivered: Option<u64>,
}

impl From<MessagingAnalytics> for Messaging {
    fn from(analytics: MessagingAnalytics) -> Self {
        Self {
            granularity: wire(analytics.granularity.as_ref()),
            phone_numbers: analytics.phone_numbers,
            country_codes: analytics.country_codes,
            data_points: analytics
                .data_points
                .into_iter()
                .map(|p| MessagingPoint {
                    start: p.start,
                    end: p.end,
                    sent: p.sent,
                    delivered: p.delivered,
                })
                .collect(),
        }
    }
}

/// An interval of conversation analytics; a breakdown is present when its
/// dimension was asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationPoint {
    /// Interval start.
    pub start: OffsetDateTime,
    /// Interval end.
    pub end: OffsetDateTime,
    /// Conversations.
    pub conversation: Option<u64>,
    /// Approximate cost, in the WABA's currency.
    pub cost: Option<f64>,
    /// The business phone number.
    pub phone_number: Option<String>,
    /// The country.
    pub country: Option<String>,
    /// The conversation type, as Meta writes it.
    pub conversation_type: Option<String>,
    /// The conversation direction, as Meta writes it.
    pub conversation_direction: Option<String>,
    /// The conversation category, as Meta writes it.
    pub conversation_category: Option<String>,
}

impl From<&ConversationDataPoint> for ConversationPoint {
    fn from(p: &ConversationDataPoint) -> Self {
        Self {
            start: p.start,
            end: p.end,
            conversation: p.conversation,
            cost: p.cost,
            phone_number: p.phone_number.clone(),
            country: p.country.clone(),
            conversation_type: wire(p.conversation_type.as_ref()),
            conversation_direction: wire(p.conversation_direction.as_ref()),
            conversation_category: wire(p.conversation_category.as_ref()),
        }
    }
}

/// Every data point of conversation analytics, the sets flattened.
#[must_use]
pub fn conversation_points(analytics: &ConversationAnalytics) -> Vec<ConversationPoint> {
    analytics
        .data_points()
        .map(ConversationPoint::from)
        .collect()
}

/// An interval of pricing analytics.
#[derive(Debug, Clone, PartialEq)]
pub struct PricingPoint {
    /// Interval start.
    pub start: OffsetDateTime,
    /// Interval end.
    pub end: OffsetDateTime,
    /// The business phone number.
    pub phone_number: Option<String>,
    /// The country.
    pub country: Option<String>,
    /// The volume tier (`0:1000`, the upper bound may be `MAX`).
    pub tier: Option<String>,
    /// The pricing type, as Meta writes it.
    pub pricing_type: Option<String>,
    /// The pricing category, as Meta writes it.
    pub pricing_category: Option<String>,
    /// Messages delivered.
    pub volume: Option<u64>,
    /// Approximate cost, in the WABA's currency.
    pub cost: Option<f64>,
}

impl From<&PricingDataPoint> for PricingPoint {
    fn from(p: &PricingDataPoint) -> Self {
        Self {
            start: p.start,
            end: p.end,
            phone_number: p.phone_number.clone(),
            country: p.country.clone(),
            tier: p.tier.clone(),
            pricing_type: wire(p.pricing_type.as_ref()),
            pricing_category: wire(p.pricing_category.as_ref()),
            volume: p.volume,
            cost: p.cost,
        }
    }
}

/// Every data point of pricing analytics, the sets flattened.
#[must_use]
pub fn pricing_points(analytics: &PricingAnalytics) -> Vec<PricingPoint> {
    analytics.data_points().map(PricingPoint::from).collect()
}

/// A day, or a moment, of template analytics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Moment {
    /// A moment.
    At(OffsetDateTime),
    /// A calendar day.
    Day(Date),
}

impl From<AnalyticsTime> for Moment {
    fn from(time: AnalyticsTime) -> Self {
        match time {
            AnalyticsTime::Timestamp(at) => Self::At(at),
            AnalyticsTime::Date(day) => Self::Day(day),
            // `AnalyticsTime` is not `non_exhaustive`: a new variant would
            // fail here, to be handled.
        }
    }
}

/// A button's clicks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Click {
    /// The kind (`url_button`, `unique_url_button`, `quick_reply_button`),
    /// as Meta writes it.
    pub kind: String,
    /// The button's text.
    pub button_content: Option<String>,
    /// The clicks.
    pub count: Option<u64>,
}

/// A cost metric.
#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    /// The kind (`amount_spent`, `cost_per_delivered`,
    /// `cost_per_url_button_click`), as Meta writes it.
    pub kind: String,
    /// The amount, in the WABA's currency.
    pub value: Option<f64>,
}

/// A template's day of analytics. The conversion metrics of the Marketing
/// Messages API for WhatsApp, which the page does not name, are not
/// answered.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplatePoint {
    /// The template.
    pub template_id: String,
    /// Day start.
    pub start: Moment,
    /// Day end.
    pub end: Moment,
    /// Sends.
    pub sent: Option<u64>,
    /// Deliveries.
    pub delivered: Option<u64>,
    /// Reads (tracked for 7 days after the send).
    pub read: Option<u64>,
    /// Button clicks.
    pub clicked: Vec<Click>,
    /// Cost metrics.
    pub cost: Vec<Cost>,
}

/// A set of a template analytics page.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateSet {
    /// `DAILY`, as Meta writes it.
    pub granularity: Option<String>,
    /// The product the metrics are for (`cloud_api`).
    pub product_type: Option<String>,
    /// The WABA's timezone, with `use_waba_timezone`.
    pub waba_timezone: Option<String>,
    /// The data points.
    pub data_points: Vec<TemplatePoint>,
}

impl From<TemplateAnalytics> for TemplateSet {
    fn from(set: TemplateAnalytics) -> Self {
        Self {
            granularity: wire(set.granularity.as_ref()),
            product_type: set.product_type,
            waba_timezone: set.waba_timezone,
            data_points: set
                .data_points
                .into_iter()
                .map(|p| TemplatePoint {
                    template_id: p.template_id.into_inner(),
                    start: p.start.into(),
                    end: p.end.into(),
                    sent: p.sent,
                    delivered: p.delivered,
                    read: p.read,
                    clicked: p
                        .clicked
                        .into_iter()
                        .map(|c| Click {
                            kind: wire(Some(&c.kind)).unwrap_or_default(),
                            button_content: c.button_content,
                            count: c.count,
                        })
                        .collect(),
                    cost: p
                        .cost
                        .into_iter()
                        .map(|c| Cost {
                            kind: wire(Some(&c.kind)).unwrap_or_default(),
                            value: c.value,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

/// A page of template analytics.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplatePage {
    /// The sets.
    pub sets: Vec<TemplateSet>,
    /// Meta's cursor for the next page, when Meta says one follows.
    pub next_after: Option<String>,
}

// ─── The operations ──────────────────────────────────────────────────────

/// The analytics reads on a WABA of the caller's tenant: an [`OwnedWaba`]
/// its [`Authorizer`] made, for a [`Caller`] it made ([`Self::new`]), with
/// the WABA's token. Cheap: a few references.
pub struct Analytics<'a> {
    authz: &'a Authorizer,
    waba: &'a OwnedWaba,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Analytics<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Analytics")
            .field("waba", self.waba)
            .finish_non_exhaustive()
    }
}

impl<'a> Analytics<'a> {
    /// The reads on `waba` for `caller`, once `authz` admits both
    /// ([`Authorizer::admit`], [`Authorizer::admit_waba`]) and `waba` was
    /// made from a binding of `caller`'s tenant. Failed Graph calls are
    /// reported to nobody until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `caller` or `waba` another [`Authorizer`] made: `403 forbidden`.
    /// `waba` another tenant's: `404 not_found`, logged at `warn`. Either
    /// way, before anything is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        waba: &'a OwnedWaba,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_waba(waba)?;
        if waba.tenant() != caller.tenant() {
            tracing::warn!("refused the analytics services on another tenant's WABA");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            waba,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self.waba.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api.with_details(error)
    }

    fn client(&self) -> meta_whatsapp_rs::client::analytics::Analytics {
        self.waba.client().analytics(self.waba.waba_id().clone())
    }

    /// Messaging analytics for `request` (range and filters checked
    /// first).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on the field at fault, before any request.
    /// Meta's refusal or failure, with its `details`.
    pub async fn messaging(&self, request: &MessagingRequest) -> Result<Messaging, ServiceError> {
        let query = messaging_query(self.authz.now(), request)?;
        match self.client().messaging(&query).await {
            Ok(analytics) => Ok(analytics.map_or_else(
                || Messaging {
                    phone_numbers: Vec::new(),
                    country_codes: Vec::new(),
                    granularity: None,
                    data_points: Vec::new(),
                },
                Messaging::from,
            )),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Conversation analytics for `request`.
    ///
    /// # Errors
    ///
    /// As [`Self::messaging`].
    pub async fn conversation(
        &self,
        request: &ConversationRequest,
    ) -> Result<Vec<ConversationPoint>, ServiceError> {
        let query = conversation_query(self.authz.now(), request)?;
        match self.client().conversation(&query).await {
            Ok(analytics) => Ok(analytics
                .as_ref()
                .map(conversation_points)
                .unwrap_or_default()),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Pricing analytics for `request`.
    ///
    /// # Errors
    ///
    /// As [`Self::messaging`].
    pub async fn pricing(
        &self,
        request: &PricingRequest,
    ) -> Result<Vec<PricingPoint>, ServiceError> {
        let query = pricing_query(self.authz.now(), request)?;
        match self.client().pricing(&query).await {
            Ok(analytics) => Ok(analytics.as_ref().map(pricing_points).unwrap_or_default()),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Template analytics for `request`, once every template id is found
    /// among the WABA's own templates.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on the field at fault, before any request.
    /// `404 not_found`, without Meta's code or text, for a template not the
    /// WABA's (the analytics are not asked for). Meta's refusal or failure,
    /// with its `details`.
    pub async fn template(&self, request: &TemplateRequest) -> Result<TemplatePage, ServiceError> {
        let query = template_query(self.authz.now(), request)?;
        all_templates_in_waba(
            self.authz,
            self.observer,
            self.waba,
            &query.template_ids,
            None,
        )
        .await?;
        match self.client().template(&query).await {
            Ok(page) => {
                let next_after = page.next_cursor().map(str::to_owned);
                Ok(TemplatePage {
                    sets: page.data.into_iter().map(TemplateSet::from).collect(),
                    next_after,
                })
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }
}

/// A `YYYY-MM-DD` day.
///
/// # Errors
///
/// `None` when `text` is not one.
#[must_use]
pub fn parse_day(text: &str) -> Option<Date> {
    let mut parts = text.splitn(3, '-');
    let year = parts.next().filter(|y| y.len() == 4)?.parse::<i32>().ok()?;
    let month = parts.next().filter(|m| m.len() == 2)?.parse::<u8>().ok()?;
    let day = parts.next().filter(|d| d.len() == 2)?.parse::<u8>().ok()?;
    Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    const NOW: OffsetDateTime = datetime!(2026-11-05 12:00 UTC);

    fn field(error: &ServiceError) -> String {
        error.field().unwrap_or("?").to_owned()
    }

    fn messaging(
        start: OffsetDateTime,
        end: OffsetDateTime,
        granularity: &str,
    ) -> MessagingRequest {
        MessagingRequest {
            start,
            end,
            granularity: granularity.to_owned(),
            phone_numbers: Vec::new(),
            country_codes: Vec::new(),
        }
    }

    /// Ranges are Meta's documented look-back and the service's cap:
    /// within a year of now, an end after the start, at most a year, and
    /// `HALF_HOUR` at most 31 days. Decisive: each clause at its edge.
    #[test]
    fn a_range_is_bounded_by_the_look_back_and_the_half_hour_cap() {
        let ok =
            |start, end, granularity| messaging_query(NOW, &messaging(start, end, granularity));
        let year_ago = datetime!(2025-11-05 12:00 UTC);
        assert!(ok(year_ago, NOW, "DAY").is_ok(), "exactly a year back");
        assert_eq!(
            field(&ok(year_ago - Duration::seconds(1), NOW, "DAY").unwrap_err()),
            "start"
        );
        assert_eq!(field(&ok(NOW, NOW, "DAY").unwrap_err()), "end", "empty");
        assert_eq!(
            field(&ok(NOW - Duration::days(10), NOW - Duration::days(11), "DAY").unwrap_err()),
            "end",
            "backwards"
        );
        let start = NOW - Duration::days(30);
        assert!(ok(start, start + Duration::days(366), "MONTH").is_ok());
        assert_eq!(
            field(&ok(start, start + Duration::days(367), "MONTH").unwrap_err()),
            "end",
            "more than a year"
        );
        assert!(ok(start, start + Duration::days(31), "HALF_HOUR").is_ok());
        assert_eq!(
            field(
                &ok(
                    start,
                    start + Duration::days(31) + Duration::seconds(1),
                    "HALF_HOUR"
                )
                .unwrap_err()
            ),
            "end",
            "half hours past 31 days"
        );
        assert_eq!(
            field(&ok(start, NOW, "DAILY").unwrap_err()),
            "granularity",
            "messaging says DAY, not DAILY"
        );
        // A 29 February: a year back is the day before a year's days.
        let leap = datetime!(2028-02-29 12:00 UTC);
        assert_eq!(a_year_before(leap), leap - Duration::days(365));
    }

    /// The values written into a field expansion are Meta's: a number is
    /// digits, a country two letters, an enum one of the page's values,
    /// each list at most 50 long. Decisive: a value closing the
    /// expansion's parenthesis is refused, and the field at fault named
    /// with its position.
    #[test]
    fn filters_are_values_a_field_expansion_cannot_be_broken_by() {
        let start = NOW - Duration::days(2);
        let base = || ConversationRequest {
            start,
            end: NOW,
            granularity: "DAILY".to_owned(),
            phone_numbers: vec!["16505550111".to_owned()],
            metric_types: vec!["COST".to_owned()],
            conversation_categories: Vec::new(),
            conversation_types: Vec::new(),
            conversation_directions: Vec::new(),
            dimensions: vec!["COUNTRY".to_owned()],
        };
        assert!(conversation_query(NOW, &base()).is_ok());
        let mut bad = base();
        bad.phone_numbers = vec!["1".to_owned(), "1).end(1".to_owned()];
        assert_eq!(
            field(&conversation_query(NOW, &bad).unwrap_err()),
            "phone_numbers[1]"
        );
        let mut bad = base();
        bad.dimensions = vec!["COUNTRY".to_owned(), "country".to_owned()];
        assert_eq!(
            field(&conversation_query(NOW, &bad).unwrap_err()),
            "dimensions[1]"
        );
        let mut bad = base();
        bad.phone_numbers = vec!["1".to_owned(); 51];
        assert_eq!(
            field(&conversation_query(NOW, &bad).unwrap_err()),
            "phone_numbers"
        );
        let pricing = PricingRequest {
            start,
            end: NOW,
            granularity: "DAILY".to_owned(),
            phone_numbers: Vec::new(),
            country_codes: vec!["US".to_owned(), "USA".to_owned()],
            metric_types: Vec::new(),
            pricing_types: Vec::new(),
            pricing_categories: vec!["MARKETING_LITE".to_owned()],
            dimensions: vec!["TIER".to_owned()],
        };
        assert_eq!(
            field(&pricing_query(NOW, &pricing).unwrap_err()),
            "country_codes[1]"
        );
        let mut pricing = pricing;
        pricing.country_codes.truncate(1);
        assert!(pricing_query(NOW, &pricing).is_ok());
    }

    /// Template analytics keep to their 90 days, 10 ids, days with the
    /// WABA's timezone, and Meta's metric names. Decisive: each clause.
    #[test]
    fn a_template_range_is_90_days_and_its_ids_ten() {
        let request = |start: Bound, end: Bound, ids: &[&str]| TemplateRequest {
            start,
            end,
            template_ids: ids.iter().map(|&i| i.to_owned()).collect(),
            metric_types: vec!["SENT".to_owned()],
            use_waba_timezone: None,
            after: None,
        };
        let at = |days: i64| Bound::At(NOW - Duration::days(days));
        assert!(template_query(NOW, &request(at(90), at(0), &["1945418102598215"])).is_ok());
        assert_eq!(
            field(&template_query(NOW, &request(at(91), at(0), &["1"])).unwrap_err()),
            "start"
        );
        assert_eq!(
            field(&template_query(NOW, &request(at(60), at(-31), &["1"])).unwrap_err()),
            "end",
            "past 90 days"
        );
        let ids = ["1"; 11];
        assert_eq!(
            field(&template_query(NOW, &request(at(5), at(0), &ids)).unwrap_err()),
            "template_ids"
        );
        assert_eq!(
            field(&template_query(NOW, &request(at(5), at(0), &[])).unwrap_err()),
            "template_ids"
        );
        assert_eq!(
            field(&template_query(NOW, &request(at(5), at(0), &["1", "01"])).unwrap_err()),
            "template_ids[1]"
        );
        let mut tz = request(at(5), at(0), &["1"]);
        tz.use_waba_timezone = Some(true);
        assert_eq!(field(&template_query(NOW, &tz).unwrap_err()), "start");
        let day = |d: &str| Bound::Day(parse_day(d).unwrap());
        let mut tz = request(day("2026-11-01"), day("2026-11-04"), &["1"]);
        tz.use_waba_timezone = Some(true);
        assert!(template_query(NOW, &tz).is_ok());
        let mut metrics = request(at(5), at(0), &["1"]);
        metrics.metric_types = vec!["sent".to_owned()];
        assert_eq!(
            field(&template_query(NOW, &metrics).unwrap_err()),
            "metric_types[0]"
        );
        assert_eq!(parse_day("2026-02-30"), None);
        assert_eq!(parse_day("2026-1-05"), None);
        assert!(parse_day("2026-11-05").is_some());
    }
}
