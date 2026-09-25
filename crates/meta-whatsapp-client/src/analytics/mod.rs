//! WABA analytics: messaging, conversation, pricing, template, template
//! group, call and group analytics, plus the two switches the analytics page
//! documents (confirming template insights, opting a template out of button
//! click tracking).
//!
//! Docs: `analytics`. Four kinds are *field expansions* on the WABA node —
//! `GET /{waba-id}?fields=<kind>.start(…).end(…)…` — and return one object:
//! [`Analytics::messaging`] (`analytics`), [`Analytics::conversation`],
//! [`Analytics::pricing`], [`Analytics::calls`]. Three are *edges* —
//! `GET /{waba-id}/<kind>?start=…` — and return pages:
//! [`Analytics::template`], [`Analytics::template_group`],
//! [`Analytics::groups`].
//!
//! Parameter spelling follows the page for each kind, which is not uniform:
//! granularity is `DAY`/`MONTH` for messaging but `DAILY`/`MONTHLY`
//! elsewhere, template-group values are lowercase, call dimensions are
//! lowercase, and list parameters are JSON arrays in field expansions,
//! comma-separated for template/template-group `metric_types`, and
//! bracketed for id lists — each as in that section's example request.
//!
//! Limits enforced locally: 1–10 template ids, 1–10 template group ids,
//! exactly one group id and at least one metric for group analytics,
//! two-letter country codes, and `YYYY-MM-DD` bounds when
//! `use_waba_timezone` is set. Look-back windows (one year; 90 days for
//! template, template group and group analytics) depend on the current date
//! and are left to Meta.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use std::collections::BTreeMap;
use std::fmt;

use futures::Stream;
use serde::{Deserialize, Deserializer, Serialize};
use time::{Date, Month, OffsetDateTime};
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{GroupId, TemplateGroupId, TemplateId, WabaId};
use meta_whatsapp_core::paging::Page;

use crate::request::{paginate_or_error, reject_cursors};
use crate::{Client, GraphRequest};

#[cfg(test)]
mod tests;

/// Most template ids per template analytics request.
pub const MAX_TEMPLATE_IDS: usize = 10;
/// Most template group ids per template group analytics request.
pub const MAX_TEMPLATE_GROUP_IDS: usize = 10;
/// Group ids per group analytics request ("Currently only supports 1 ID").
pub const GROUP_IDS_PER_REQUEST: usize = 1;

/// Entry point, see [`Client::analytics`].
#[derive(Debug, Clone)]
pub struct Analytics {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Analytics`] API for `waba_id`.
    pub fn analytics(&self, waba_id: impl Into<WabaId>) -> Analytics {
        Analytics {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

impl Analytics {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn expansion(&self, fields: String, context: &'static str) -> GraphRequest {
        self.client
            .get_at(&[self.waba_id.as_str()])
            .query("fields", fields)
            .context(context)
    }

    /// Messages sent and delivered: `?fields=analytics.…`
    /// (`analytics#messaging-analytics`). `None` when Meta returns no
    /// `analytics` object for the range.
    pub async fn messaging(
        &self,
        query: &MessagingAnalyticsQuery,
    ) -> Result<Option<MessagingAnalytics>> {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            analytics: Option<MessagingAnalytics>,
        }
        let fields = Expansion::new("analytics", query.start, query.end)
            .scalar("granularity", &query.granularity)?
            .json_list("phone_numbers", &query.phone_numbers)?
            .raw_list(
                "product_types",
                query.product_types.iter().map(|p| p.code().to_string()),
            )
            .json_list("country_codes", validate_countries(&query.country_codes)?)?
            .finish();
        let env: Envelope = self
            .expansion(fields, "messaging analytics response")
            .send()
            .await?;
        Ok(env.analytics)
    }

    /// Conversations and their cost: `?fields=conversation_analytics.…`
    /// (`analytics#conversation-analytics`). `None` when Meta returns no
    /// data for the range.
    pub async fn conversation(
        &self,
        query: &ConversationAnalyticsQuery,
    ) -> Result<Option<ConversationAnalytics>> {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            conversation_analytics: Option<ConversationAnalytics>,
            // One example on the page shows `data` at the top level, without
            // the `conversation_analytics` wrapper; accept it too.
            #[serde(default)]
            data: Option<Vec<DataPointSet<ConversationDataPoint>>>,
        }
        let fields = Expansion::new("conversation_analytics", query.start, query.end)
            .scalar("granularity", &query.granularity)?
            .json_list("phone_numbers", &query.phone_numbers)?
            .json_list("metric_types", &query.metric_types)?
            .json_list("conversation_categories", &query.conversation_categories)?
            .json_list("conversation_types", &query.conversation_types)?
            .json_list("conversation_directions", &query.conversation_directions)?
            .json_list("dimensions", &query.dimensions)?
            .finish();
        let env: Envelope = self
            .expansion(fields, "conversation analytics response")
            .send()
            .await?;
        Ok(env
            .conversation_analytics
            .or_else(|| env.data.map(|data| ConversationAnalytics { data })))
    }

    /// Delivered-message volume and cost by pricing category/type:
    /// `?fields=pricing_analytics.…` (`analytics#pricing-analytics`).
    pub async fn pricing(&self, query: &PricingAnalyticsQuery) -> Result<Option<PricingAnalytics>> {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            pricing_analytics: Option<PricingAnalytics>,
        }
        let fields = Expansion::new("pricing_analytics", query.start, query.end)
            .scalar("granularity", &query.granularity)?
            .json_list("phone_numbers", &query.phone_numbers)?
            .json_list("country_codes", validate_countries(&query.country_codes)?)?
            .json_list("metric_types", &query.metric_types)?
            .json_list("pricing_types", &query.pricing_types)?
            .json_list("pricing_categories", &query.pricing_categories)?
            .json_list("dimensions", &query.dimensions)?
            .finish();
        let env: Envelope = self
            .expansion(fields, "pricing analytics response")
            .send()
            .await?;
        Ok(env.pricing_analytics)
    }

    /// Calls made and received: `?fields=call_analytics.…`
    /// (`analytics#call-analytics`).
    pub async fn calls(&self, query: &CallAnalyticsQuery) -> Result<Option<CallAnalytics>> {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            call_analytics: Option<CallAnalytics>,
        }
        let fields = Expansion::new("call_analytics", query.start, query.end)
            .scalar("granularity", &query.granularity)?
            .json_list("phone_numbers", &query.phone_numbers)?
            .json_list("country_codes", validate_countries(&query.country_codes)?)?
            .json_list("directions", &query.directions)?
            .json_list("dimensions", &query.dimensions)?
            .json_list("metric_types", &query.metric_types)?
            .finish();
        let env: Envelope = self
            .expansion(fields, "call analytics response")
            .send()
            .await?;
        Ok(env.call_analytics)
    }

    /// Sends, deliveries, reads, clicks and cost per template, daily:
    /// `GET /{waba-id}/template_analytics` (`analytics#template-analytics`).
    /// Template insights must be confirmed first, see
    /// [`Self::enable_template_insights`].
    pub async fn template(
        &self,
        query: &TemplateAnalyticsQuery,
    ) -> Result<Page<TemplateAnalytics>> {
        self.template_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every page of [`Self::template`].
    ///
    /// The stream manages the cursors itself: a query with `after` or
    /// `before` set is refused, like any other validation failure, as the
    /// stream's single item.
    pub fn template_stream(
        &self,
        query: &TemplateAnalyticsQuery,
    ) -> impl Stream<Item = Result<TemplateAnalytics>> + Send + 'static {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.template_request(query)),
        )
    }

    fn template_request(&self, query: &TemplateAnalyticsQuery) -> Result<GraphRequest> {
        validate_id_count("template_ids", query.template_ids.len(), MAX_TEMPLATE_IDS)?;
        validate_waba_timezone(query.use_waba_timezone, query.start, query.end)?;
        let ids = query.template_ids.iter().map(TemplateId::as_str);
        Ok(self
            .client
            .get_at(&[self.waba_id.as_str(), "template_analytics"])
            .query("start", query.start)
            .query("end", query.end)
            .query("granularity", "DAILY")
            .query_opt("metric_types", comma_list(&query.metric_types)?)
            .query("template_ids", bracketed(ids))
            .query_opt(
                "product_type",
                query.product_type.as_ref().map(wire).transpose()?,
            )
            .query_opt("use_waba_timezone", query.use_waba_timezone)
            .context("template analytics response"))
    }

    /// Per-template-group metrics, daily:
    /// `GET /{waba-id}/template_group_analytics`
    /// (`analytics#template-group-analytics`).
    pub async fn template_group(
        &self,
        query: &TemplateGroupAnalyticsQuery,
    ) -> Result<Page<TemplateGroupAnalytics>> {
        self.template_group_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every page of [`Self::template_group`].
    ///
    /// The stream manages the cursors itself: a query with `after` or
    /// `before` set is refused, like any other validation failure, as the
    /// stream's single item.
    pub fn template_group_stream(
        &self,
        query: &TemplateGroupAnalyticsQuery,
    ) -> impl Stream<Item = Result<TemplateGroupAnalytics>> + Send + 'static {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.template_group_request(query)),
        )
    }

    fn template_group_request(&self, query: &TemplateGroupAnalyticsQuery) -> Result<GraphRequest> {
        validate_id_count(
            "template_group_ids",
            query.template_group_ids.len(),
            MAX_TEMPLATE_GROUP_IDS,
        )?;
        validate_waba_timezone(query.use_waba_timezone, query.start, query.end)?;
        let ids = query.template_group_ids.iter().map(TemplateGroupId::as_str);
        Ok(self
            .client
            .get_at(&[self.waba_id.as_str(), "template_group_analytics"])
            .query("granularity", "daily")
            .query("start", query.start)
            .query("end", query.end)
            .query_opt("metric_types", comma_list(&query.metric_types)?)
            .query("template_group_ids", bracketed(ids))
            .query_opt("use_waba_timezone", query.use_waba_timezone)
            .context("template group analytics response"))
    }

    /// Messages and joins/leaves per group, daily:
    /// `GET /{waba-id}/group_analytics` (`analytics#group-analytics`).
    pub async fn groups(&self, query: &GroupAnalyticsQuery) -> Result<Page<GroupAnalytics>> {
        self.groups_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every page of [`Self::groups`].
    ///
    /// The stream manages the cursors itself: a query with `after` or
    /// `before` set is refused, like any other validation failure, as the
    /// stream's single item.
    pub fn groups_stream(
        &self,
        query: &GroupAnalyticsQuery,
    ) -> impl Stream<Item = Result<GroupAnalytics>> + Send + 'static {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.groups_request(query)),
        )
    }

    fn groups_request(&self, query: &GroupAnalyticsQuery) -> Result<GraphRequest> {
        if query.group_ids.len() != GROUP_IDS_PER_REQUEST {
            return Err(ValidationError::new(
                "group_ids",
                format!(
                    "exactly {GROUP_IDS_PER_REQUEST} group id is supported, got {}",
                    query.group_ids.len()
                ),
            )
            .into());
        }
        if query.metric_types.is_empty() {
            return Err(
                ValidationError::new("metric_types", "at least one metric is required").into(),
            );
        }
        Ok(self
            .client
            .get_at(&[self.waba_id.as_str(), "group_analytics"])
            .query("start", query.start.unix_timestamp())
            .query("end", query.end.unix_timestamp())
            .query("granularity", "DAILY")
            .query_json("group_ids", &query.group_ids)
            .query_json("metric_types", &query.metric_types)
            .context("group analytics response"))
    }

    /// Confirm template analytics for the WABA:
    /// `POST /{waba-id}?is_enabled_for_insights=true`
    /// (`analytics#confirming-template-analytics`). Returns the WABA id Meta
    /// echoes (as a JSON number in the docs).
    ///
    /// This cannot be undone, and it directs Meta to add link tracking and
    /// to collect and anonymize chat data — read the note on the page
    /// before calling it. Marked idempotent: confirming twice is harmless.
    pub async fn enable_template_insights(&self) -> Result<WabaId> {
        #[derive(Deserialize)]
        struct Confirmed {
            #[serde(deserialize_with = "string_or_number")]
            id: String,
        }
        let resp: Confirmed = self
            .client
            .post_at(&[self.waba_id.as_str()])
            .query("is_enabled_for_insights", true)
            .idempotent(true)
            .context("enable template insights response")
            .send()
            .await?;
        Ok(WabaId::new(resp.id))
    }

    /// Opt a template out of (`true`) or back into button click tracking:
    /// `POST /{template-id}?cta_url_link_tracking_opted_out=…&category=…`
    /// (`analytics#disabling-button-click-analytics`).
    ///
    /// `category` must be the template's *current* category (e.g.
    /// `marketing`); any other value moves the template back to `PENDING`
    /// review. This hangs off the template, not the WABA; it lives here
    /// because the analytics page documents it. Marked idempotent: it sets
    /// a flag.
    pub async fn set_button_click_tracking(
        &self,
        template_id: &TemplateId,
        opted_out: bool,
        category: &str,
    ) -> Result<()> {
        if category.trim().is_empty() {
            return Err(ValidationError::new("category", "must not be empty").into());
        }
        self.client
            .post_at(&[template_id.as_str()])
            .query("cta_url_link_tracking_opted_out", opted_out)
            .query("category", category)
            .idempotent(true)
            .context("set button click tracking response")
            .send_success()
            .await
    }
}

/// `field.start(S).end(E).name(value)…`, the WABA-node field expansion.
struct Expansion(String);

impl Expansion {
    fn new(field: &str, start: OffsetDateTime, end: OffsetDateTime) -> Self {
        Self(format!(
            "{field}.start({}).end({})",
            start.unix_timestamp(),
            end.unix_timestamp()
        ))
    }

    /// Append `.name(value)`.
    fn push_param(&mut self, name: &str, value: &str) {
        self.0.push('.');
        self.0.push_str(name);
        self.0.push('(');
        self.0.push_str(value);
        self.0.push(')');
    }

    fn scalar<T: Serialize>(mut self, name: &str, value: &T) -> Result<Self> {
        let value = wire(value)?;
        self.push_param(name, &value);
        Ok(self)
    }

    /// `.name(["a","b"])`; omitted when empty (Meta treats an absent and an
    /// empty filter the same: no filtering).
    fn json_list<T: Serialize>(mut self, name: &str, values: &[T]) -> Result<Self> {
        if values.is_empty() {
            return Ok(self);
        }
        let list = serde_json::to_string(values)
            .map_err(|e| ValidationError::new(name, format!("not serializable: {e}")))?;
        self.push_param(name, &list);
        Ok(self)
    }

    /// `.name([a,b])` for unquoted values.
    fn raw_list(mut self, name: &str, values: impl Iterator<Item = String>) -> Self {
        let values: Vec<String> = values.collect();
        if !values.is_empty() {
            self.push_param(name, &format!("[{}]", values.join(",")));
        }
        self
    }

    fn finish(self) -> String {
        self.0
    }
}

/// The wire string of a unit-like enum value.
fn wire<T: Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        Ok(other) => Ok(other.to_string()),
        Err(e) => Err(ValidationError::new("query", format!("not serializable: {e}")).into()),
    }
}

/// `a,b,c`, or `None` when empty.
fn comma_list<T: Serialize>(values: &[T]) -> Result<Option<String>> {
    if values.is_empty() {
        return Ok(None);
    }
    let parts = values.iter().map(wire).collect::<Result<Vec<_>>>()?;
    Ok(Some(parts.join(",")))
}

/// `[a,b]`, unquoted, as the id lists appear in the docs' examples.
fn bracketed<'a>(ids: impl Iterator<Item = &'a str>) -> String {
    format!("[{}]", ids.collect::<Vec<_>>().join(","))
}

fn validate_id_count(field: &str, len: usize, max: usize) -> Result<()> {
    if len == 0 || len > max {
        return Err(
            ValidationError::new(field, format!("must list 1 to {max} ids, got {len}")).into(),
        );
    }
    Ok(())
}

fn validate_countries(codes: &[String]) -> Result<&[String]> {
    if let Some((i, bad)) = codes
        .iter()
        .enumerate()
        .find(|(_, c)| c.len() != 2 || !c.bytes().all(|b| b.is_ascii_alphabetic()))
    {
        return Err(ValidationError::new(
            format!("country_codes[{i}]"),
            format!("must be a 2-letter country code, got `{bad}`"),
        )
        .into());
    }
    Ok(codes)
}

fn validate_waba_timezone(
    use_waba_timezone: Option<bool>,
    start: AnalyticsTime,
    end: AnalyticsTime,
) -> Result<()> {
    if use_waba_timezone != Some(true) {
        return Ok(());
    }
    for (field, bound) in [("start", start), ("end", end)] {
        if !matches!(bound, AnalyticsTime::Date(_)) {
            return Err(ValidationError::new(
                field,
                "must be a YYYY-MM-DD date when use_waba_timezone is true",
            )
            .into());
        }
    }
    Ok(())
}

fn string_or_number<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Str(String),
        Num(serde_json::Number),
    }
    Ok(match Raw::deserialize(d)? {
        Raw::Str(s) => s,
        Raw::Num(n) => n.to_string(),
    })
}

fn one_or_many<'de, D, T>(d: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw<T> {
        Many(Vec<T>),
        One(T),
    }
    Ok(match Option::<Raw<T>>::deserialize(d)? {
        None => Vec::new(),
        Some(Raw::Many(v)) => v,
        Some(Raw::One(t)) => vec![t],
    })
}

// ── Time bounds ──────────────────────────────────────────────────────────

/// A range bound for template and template group analytics, which accept a
/// unix timestamp or a `YYYY-MM-DD` date (the latter is required with
/// `use_waba_timezone`). Also how those endpoints' data points report
/// `start`/`end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalyticsTime {
    /// Unix seconds. Meta rounds down to 00:00 UTC.
    Timestamp(OffsetDateTime),
    /// A calendar day.
    Date(Date),
}

impl From<OffsetDateTime> for AnalyticsTime {
    fn from(t: OffsetDateTime) -> Self {
        Self::Timestamp(t)
    }
}

impl From<Date> for AnalyticsTime {
    fn from(d: Date) -> Self {
        Self::Date(d)
    }
}

impl fmt::Display for AnalyticsTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timestamp(t) => write!(f, "{}", t.unix_timestamp()),
            Self::Date(d) => write!(
                f,
                "{:04}-{:02}-{:02}",
                d.year(),
                u8::from(d.month()),
                d.day()
            ),
        }
    }
}

impl<'de> Deserialize<'de> for AnalyticsTime {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(i64),
            Str(String),
        }
        let unix = |secs: i64| {
            OffsetDateTime::from_unix_timestamp(secs)
                .map(Self::Timestamp)
                .map_err(D::Error::custom)
        };
        match Raw::deserialize(d)? {
            Raw::Int(secs) => unix(secs),
            Raw::Str(s) => match s.trim().parse::<i64>() {
                Ok(secs) => unix(secs),
                Err(_) => parse_date(&s)
                    .map(Self::Date)
                    .ok_or_else(|| D::Error::custom(format!("invalid analytics time `{s}`"))),
            },
        }
    }
}

fn parse_date(s: &str) -> Option<Date> {
    let mut parts = s.trim().splitn(3, '-');
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = Month::try_from(parts.next()?.parse::<u8>().ok()?).ok()?;
    let day = parts.next()?.parse::<u8>().ok()?;
    Date::from_calendar_date(year, month, day).ok()
}

// ── Enums ────────────────────────────────────────────────────────────────
//
// Every enum keeps unknown values in an untagged `Other(String)` so a value
// Meta adds never breaks parsing, and can be sent back unchanged.

/// Granularity of messaging analytics (`analytics#messaging-analytics`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MessagingGranularity {
    /// `HALF_HOUR`.
    HalfHour,
    /// `DAY`.
    Day,
    /// `MONTH`.
    Month,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// Granularity of conversation, pricing and call analytics; template
/// analytics report `DAILY`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Granularity {
    /// `HALF_HOUR`.
    HalfHour,
    /// `DAILY`.
    Daily,
    /// `MONTHLY`.
    Monthly,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `product_types` filter of messaging analytics. Sent as its numeric code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageProductType {
    /// `0`: template messages sent to users.
    Template,
    /// `2`: non-template messages sent to users.
    NonTemplate,
    /// `100`: messages users sent to you.
    Incoming,
    /// Any other documented code.
    Other(u16),
}

impl MessageProductType {
    /// The numeric code Meta expects.
    pub fn code(self) -> u16 {
        match self {
            Self::Template => 0,
            Self::NonTemplate => 2,
            Self::Incoming => 100,
            Self::Other(code) => code,
        }
    }
}

/// `metric_types` of conversation analytics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConversationMetric {
    /// `COST` (not returned for WABAs on a partner's credit line).
    Cost,
    /// `CONVERSATION`.
    Conversation,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// Conversation category.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConversationCategory {
    /// `AUTHENTICATION`.
    Authentication,
    /// `MARKETING`.
    Marketing,
    /// `SERVICE`.
    Service,
    /// `UTILITY`.
    Utility,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// Conversation type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConversationType {
    /// `FREE_ENTRY_POINT`.
    FreeEntryPoint,
    /// `FREE_TIER`.
    FreeTier,
    /// `REGULAR`.
    Regular,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// Conversation direction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConversationDirection {
    /// `BUSINESS_INITIATED`.
    BusinessInitiated,
    /// `USER_INITIATED`.
    UserInitiated,
    /// `UNKNOWN` — a documented value: Meta could not tell.
    Unknown,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `dimensions` (breakdowns) of conversation analytics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConversationDimension {
    /// `CONVERSATION_CATEGORY`.
    ConversationCategory,
    /// `CONVERSATION_DIRECTION`.
    ConversationDirection,
    /// `CONVERSATION_TYPE`.
    ConversationType,
    /// `COUNTRY`.
    Country,
    /// `PHONE`.
    Phone,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `metric_types` of pricing analytics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PricingMetric {
    /// `COST`.
    Cost,
    /// `VOLUME`.
    Volume,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// Pricing category of delivered messages.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PricingCategory {
    /// `AUTHENTICATION`.
    Authentication,
    /// `AUTHENTICATION_INTERNATIONAL`.
    AuthenticationInternational,
    /// `MARKETING`.
    Marketing,
    /// `MARKETING_LITE`.
    MarketingLite,
    /// `SERVICE` (not charged).
    Service,
    /// `UTILITY`.
    Utility,
    /// `REFERRAL_CONVERSION` (free entry point).
    ReferralConversion,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// Pricing type of delivered messages.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PricingType {
    /// `FREE_CUSTOMER_SERVICE`.
    FreeCustomerService,
    /// `FREE_ENTRY_POINT`.
    FreeEntryPoint,
    /// `REGULAR` (billable).
    Regular,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `dimensions` of pricing analytics. `Tier` together with
/// `PricingCategory` and `Country` adds volume tiers to data points.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PricingDimension {
    /// `COUNTRY`.
    Country,
    /// `PHONE`.
    Phone,
    /// `PRICING_CATEGORY`.
    PricingCategory,
    /// `PRICING_TYPE`.
    PricingType,
    /// `TIER`.
    Tier,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// Call direction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CallDirection {
    /// `USER_INITIATED`.
    UserInitiated,
    /// `BUSINESS_INITIATED`.
    BusinessInitiated,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `dimensions` of call analytics — lowercase on the wire, as documented.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallDimension {
    /// `phone`.
    Phone,
    /// `direction`.
    Direction,
    /// `country`.
    Country,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `metric_types` of call analytics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CallMetric {
    /// `COUNT`.
    Count,
    /// `COST`.
    Cost,
    /// `AVERAGE_DURATION`.
    AverageDuration,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `metric_types` of template analytics. The `App*`/`Website*` metrics are
/// for Marketing Messages API for WhatsApp businesses only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TemplateMetric {
    /// `COST`.
    Cost,
    /// `CLICKED`.
    Clicked,
    /// `DELIVERED`.
    Delivered,
    /// `READ`.
    Read,
    /// `SENT`.
    Sent,
    /// `APP_ACTIVATIONS`.
    AppActivations,
    /// `APP_ADD_TO_CART`.
    AppAddToCart,
    /// `APP_CHECKOUTS_INITIATED`.
    AppCheckoutsInitiated,
    /// `APP_PURCHASES`.
    AppPurchases,
    /// `APP_PURCHASES_CONVERSION_VALUE`.
    AppPurchasesConversionValue,
    /// `WEBSITE_ADD_TO_CART`.
    WebsiteAddToCart,
    /// `WEBSITE_CHECKOUTS_INITIATED`.
    WebsiteCheckoutsInitiated,
    /// `WEBSITE_PURCHASES`.
    WebsitePurchases,
    /// `WEBSITE_PURCHASES_CONVERSION_VALUE`.
    WebsitePurchasesConversionValue,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `product_type` filter of template analytics; without it only Cloud API
/// sends are counted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TemplateProductType {
    /// `CLOUD_API`.
    CloudApi,
    /// `MARKETING_MESSAGES_API_FOR_WHATSAPP`.
    MarketingMessagesApiForWhatsapp,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `metric_types` of template group analytics — lowercase on the wire, as
/// documented for this endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TemplateGroupMetric {
    /// `cost`.
    Cost,
    /// `clicked`.
    Clicked,
    /// `delivered`.
    Delivered,
    /// `read`.
    Read,
    /// `sent`.
    Sent,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `metric_types` of group analytics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GroupMetric {
    /// `SENT`: messages the business sent to the group.
    Sent,
    /// `DELIVERED`: deliveries to participants.
    Delivered,
    /// `READ`: reads by participants.
    Read,
    /// `PARTICIPANTS_JOINED`.
    ParticipantsJoined,
    /// `PARTICIPANTS_LEFT`.
    ParticipantsLeft,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `clicked[].type` of template analytics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClickType {
    /// `url_button`: total URL button clicks.
    UrlButton,
    /// `unique_url_button`: distinct users who clicked.
    UniqueUrlButton,
    /// `quick_reply_button` (appears in the example response).
    QuickReplyButton,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

/// `cost[].type` of template analytics.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostType {
    /// `amount_spent`.
    AmountSpent,
    /// `cost_per_delivered`.
    CostPerDelivered,
    /// `cost_per_url_button_click`.
    CostPerUrlButtonClick,
    /// Any other value, verbatim.
    #[serde(untagged)]
    Other(String),
}

// ── Queries ──────────────────────────────────────────────────────────────

/// Query of [`Analytics::messaging`]. Empty lists are not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagingAnalyticsQuery {
    /// Range start.
    pub start: OffsetDateTime,
    /// Range end.
    pub end: OffsetDateTime,
    /// `HALF_HOUR`, `DAY` or `MONTH`.
    pub granularity: MessagingGranularity,
    /// Only these business phone numbers.
    pub phone_numbers: Vec<String>,
    /// Only these message kinds.
    pub product_types: Vec<MessageProductType>,
    /// Only these countries (two-letter codes).
    pub country_codes: Vec<String>,
}

impl MessagingAnalyticsQuery {
    /// All numbers, all message kinds, all countries.
    pub fn new(
        start: OffsetDateTime,
        end: OffsetDateTime,
        granularity: MessagingGranularity,
    ) -> Self {
        Self {
            start,
            end,
            granularity,
            phone_numbers: Vec::new(),
            product_types: Vec::new(),
            country_codes: Vec::new(),
        }
    }
}

/// Query of [`Analytics::conversation`]. Empty lists are not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationAnalyticsQuery {
    /// Range start.
    pub start: OffsetDateTime,
    /// Range end.
    pub end: OffsetDateTime,
    /// `HALF_HOUR`, `DAILY` or `MONTHLY`.
    pub granularity: Granularity,
    /// Only these business phone numbers.
    pub phone_numbers: Vec<String>,
    /// Metrics to return.
    pub metric_types: Vec<ConversationMetric>,
    /// Only these categories.
    pub conversation_categories: Vec<ConversationCategory>,
    /// Only these types.
    pub conversation_types: Vec<ConversationType>,
    /// Only these directions.
    pub conversation_directions: Vec<ConversationDirection>,
    /// Breakdowns.
    pub dimensions: Vec<ConversationDimension>,
}

impl ConversationAnalyticsQuery {
    /// No filters, no breakdowns.
    pub fn new(start: OffsetDateTime, end: OffsetDateTime, granularity: Granularity) -> Self {
        Self {
            start,
            end,
            granularity,
            phone_numbers: Vec::new(),
            metric_types: Vec::new(),
            conversation_categories: Vec::new(),
            conversation_types: Vec::new(),
            conversation_directions: Vec::new(),
            dimensions: Vec::new(),
        }
    }
}

/// Query of [`Analytics::pricing`]. Empty lists are not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PricingAnalyticsQuery {
    /// Range start.
    pub start: OffsetDateTime,
    /// Range end.
    pub end: OffsetDateTime,
    /// `DAILY`, `HALF_HOUR` or `MONTHLY`.
    pub granularity: Granularity,
    /// Only these business phone numbers.
    pub phone_numbers: Vec<String>,
    /// Only these countries (two-letter codes).
    pub country_codes: Vec<String>,
    /// Metrics to return.
    pub metric_types: Vec<PricingMetric>,
    /// Only these pricing types.
    pub pricing_types: Vec<PricingType>,
    /// Only these pricing categories.
    pub pricing_categories: Vec<PricingCategory>,
    /// Breakdowns.
    pub dimensions: Vec<PricingDimension>,
}

impl PricingAnalyticsQuery {
    /// No filters, no breakdowns.
    pub fn new(start: OffsetDateTime, end: OffsetDateTime, granularity: Granularity) -> Self {
        Self {
            start,
            end,
            granularity,
            phone_numbers: Vec::new(),
            country_codes: Vec::new(),
            metric_types: Vec::new(),
            pricing_types: Vec::new(),
            pricing_categories: Vec::new(),
            dimensions: Vec::new(),
        }
    }
}

/// Query of [`Analytics::calls`]. Empty lists are not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallAnalyticsQuery {
    /// Range start.
    pub start: OffsetDateTime,
    /// Range end.
    pub end: OffsetDateTime,
    /// `HALF_HOUR`, `DAILY` or `MONTHLY`.
    pub granularity: Granularity,
    /// Only these business phone numbers.
    pub phone_numbers: Vec<String>,
    /// Only these countries (two-letter codes).
    pub country_codes: Vec<String>,
    /// Only these call directions.
    pub directions: Vec<CallDirection>,
    /// Breakdowns.
    pub dimensions: Vec<CallDimension>,
    /// Metrics to return.
    pub metric_types: Vec<CallMetric>,
}

impl CallAnalyticsQuery {
    /// No filters, no breakdowns.
    pub fn new(start: OffsetDateTime, end: OffsetDateTime, granularity: Granularity) -> Self {
        Self {
            start,
            end,
            granularity,
            phone_numbers: Vec::new(),
            country_codes: Vec::new(),
            directions: Vec::new(),
            dimensions: Vec::new(),
            metric_types: Vec::new(),
        }
    }
}

/// Query of [`Analytics::template`]. Granularity is always `DAILY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateAnalyticsQuery {
    /// Range start.
    pub start: AnalyticsTime,
    /// Range end.
    pub end: AnalyticsTime,
    /// 1–10 templates.
    pub template_ids: Vec<TemplateId>,
    /// Metrics to return; empty means all.
    pub metric_types: Vec<TemplateMetric>,
    /// Cloud API (Meta's default) or MM API for WhatsApp sends.
    pub product_type: Option<TemplateProductType>,
    /// Report in the WABA's timezone; `start`/`end` must then be dates.
    pub use_waba_timezone: Option<bool>,
    /// Cursor from a previous page's `paging.cursors.after`
    /// ([`Page::next_cursor`]); for the one-page method only, the stream
    /// manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl TemplateAnalyticsQuery {
    /// All metrics for `template_ids`, in UTC.
    pub fn new(
        start: impl Into<AnalyticsTime>,
        end: impl Into<AnalyticsTime>,
        template_ids: Vec<TemplateId>,
    ) -> Self {
        Self {
            start: start.into(),
            end: end.into(),
            template_ids,
            metric_types: Vec::new(),
            product_type: None,
            use_waba_timezone: None,
            after: None,
            before: None,
        }
    }
}

/// Query of [`Analytics::template_group`]. Granularity is always daily.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateGroupAnalyticsQuery {
    /// Range start.
    pub start: AnalyticsTime,
    /// Range end.
    pub end: AnalyticsTime,
    /// 1–10 template groups.
    pub template_group_ids: Vec<TemplateGroupId>,
    /// Metrics to return; empty means all.
    pub metric_types: Vec<TemplateGroupMetric>,
    /// Report in the WABA's timezone; `start`/`end` must then be dates.
    pub use_waba_timezone: Option<bool>,
    /// Cursor from a previous page's `paging.cursors.after`
    /// ([`Page::next_cursor`]); for the one-page method only, the stream
    /// manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl TemplateGroupAnalyticsQuery {
    /// All metrics for `template_group_ids`, in UTC.
    pub fn new(
        start: impl Into<AnalyticsTime>,
        end: impl Into<AnalyticsTime>,
        template_group_ids: Vec<TemplateGroupId>,
    ) -> Self {
        Self {
            start: start.into(),
            end: end.into(),
            template_group_ids,
            metric_types: Vec::new(),
            use_waba_timezone: None,
            after: None,
            before: None,
        }
    }
}

/// Query of [`Analytics::groups`]. Granularity is always `DAILY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupAnalyticsQuery {
    /// Range start; at most 90 days ago.
    pub start: OffsetDateTime,
    /// Range end.
    pub end: OffsetDateTime,
    /// Exactly one group, for now.
    pub group_ids: Vec<GroupId>,
    /// At least one metric.
    pub metric_types: Vec<GroupMetric>,
    /// Cursor from a previous page's `paging.cursors.after`
    /// ([`Page::next_cursor`]); for the one-page method only, the stream
    /// manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl GroupAnalyticsQuery {
    /// `metric_types` of `group_ids` (exactly one group, for now) between
    /// `start` and `end`, from the first page.
    pub fn new(
        start: OffsetDateTime,
        end: OffsetDateTime,
        group_ids: Vec<GroupId>,
        metric_types: Vec<GroupMetric>,
    ) -> Self {
        Self {
            start,
            end,
            group_ids,
            metric_types,
            after: None,
            before: None,
        }
    }
}

// ── Responses ────────────────────────────────────────────────────────────

/// `analytics` object of messaging analytics.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MessagingAnalytics {
    /// Phone numbers included.
    #[serde(default)]
    pub phone_numbers: Vec<String>,
    /// Countries included.
    #[serde(default)]
    pub country_codes: Vec<String>,
    /// Granularity of the data points.
    #[serde(default)]
    pub granularity: Option<MessagingGranularity>,
    /// Data points.
    #[serde(default)]
    pub data_points: Vec<MessagingDataPoint>,
}

/// A messaging analytics data point.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MessagingDataPoint {
    /// Interval start.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub start: OffsetDateTime,
    /// Interval end.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub end: OffsetDateTime,
    /// Messages sent.
    #[serde(default)]
    pub sent: Option<u64>,
    /// Messages delivered.
    #[serde(default)]
    pub delivered: Option<u64>,
}

/// `{"data_points": [...]}`, the element of a `data` array.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DataPointSet<T> {
    /// Data points.
    #[serde(default = "Vec::new")]
    pub data_points: Vec<T>,
}

/// `conversation_analytics` object.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConversationAnalytics {
    /// Data point sets (one in every documented example).
    #[serde(default)]
    pub data: Vec<DataPointSet<ConversationDataPoint>>,
}

impl ConversationAnalytics {
    /// Every data point of every set.
    pub fn data_points(&self) -> impl Iterator<Item = &ConversationDataPoint> {
        self.data.iter().flat_map(|set| set.data_points.iter())
    }
}

/// A conversation analytics data point. Breakdown fields are present only
/// for the requested `dimensions`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConversationDataPoint {
    /// Interval start.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub start: OffsetDateTime,
    /// Interval end.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub end: OffsetDateTime,
    /// Conversations.
    #[serde(default)]
    pub conversation: Option<u64>,
    /// Approximate cost in the WABA's currency.
    #[serde(default)]
    pub cost: Option<f64>,
    /// Business phone number.
    #[serde(default)]
    pub phone_number: Option<String>,
    /// Country code.
    #[serde(default)]
    pub country: Option<String>,
    /// Conversation type.
    #[serde(default)]
    pub conversation_type: Option<ConversationType>,
    /// Conversation direction.
    #[serde(default)]
    pub conversation_direction: Option<ConversationDirection>,
    /// Conversation category.
    #[serde(default)]
    pub conversation_category: Option<ConversationCategory>,
}

/// `pricing_analytics` object.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PricingAnalytics {
    /// Data point sets.
    #[serde(default)]
    pub data: Vec<DataPointSet<PricingDataPoint>>,
}

impl PricingAnalytics {
    /// Every data point of every set.
    pub fn data_points(&self) -> impl Iterator<Item = &PricingDataPoint> {
        self.data.iter().flat_map(|set| set.data_points.iter())
    }
}

/// A pricing analytics data point.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PricingDataPoint {
    /// Interval start.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub start: OffsetDateTime,
    /// Interval end.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub end: OffsetDateTime,
    /// Business phone number.
    #[serde(default)]
    pub phone_number: Option<String>,
    /// Country code.
    #[serde(default)]
    pub country: Option<String>,
    /// Volume tier `"<LOWER>:<UPPER>"` (upper may be `MAX`); absent for free
    /// messages.
    #[serde(default)]
    pub tier: Option<String>,
    /// Pricing type.
    #[serde(default)]
    pub pricing_type: Option<PricingType>,
    /// Pricing category.
    #[serde(default)]
    pub pricing_category: Option<PricingCategory>,
    /// Messages delivered.
    #[serde(default)]
    pub volume: Option<u64>,
    /// Approximate cost in the WABA's currency.
    #[serde(default)]
    pub cost: Option<f64>,
}

/// `call_analytics` object.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CallAnalytics {
    /// Granularity of the data points.
    #[serde(default)]
    pub granularity: Option<Granularity>,
    /// Echo of the `directions` filter (a string for one direction).
    #[serde(default, deserialize_with = "one_or_many")]
    pub directions: Vec<CallDirection>,
    /// Data points.
    #[serde(default)]
    pub data_points: Vec<CallDataPoint>,
}

/// A call analytics data point.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CallDataPoint {
    /// Interval start.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub start: OffsetDateTime,
    /// Interval end.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub end: OffsetDateTime,
    /// Calls.
    #[serde(default)]
    pub count: Option<u64>,
    /// Cost.
    #[serde(default)]
    pub cost: Option<f64>,
    /// Average duration, seconds.
    #[serde(default)]
    pub average_duration: Option<f64>,
    /// Business phone number (`phone` dimension).
    #[serde(default)]
    pub phone_number: Option<String>,
    /// Country code (`country` dimension).
    #[serde(default)]
    pub country: Option<String>,
    /// Direction (`direction` dimension).
    #[serde(default)]
    pub direction: Option<CallDirection>,
}

/// An element of the template analytics `data` array.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TemplateAnalytics {
    /// `DAILY`.
    #[serde(default)]
    pub granularity: Option<Granularity>,
    /// Product the metrics are for, e.g. `cloud_api` (lowercase in
    /// responses, unlike the request filter).
    #[serde(default)]
    pub product_type: Option<String>,
    /// The WABA's timezone, with `use_waba_timezone`.
    #[serde(default)]
    pub waba_timezone: Option<String>,
    /// Data points.
    #[serde(default)]
    pub data_points: Vec<TemplateDataPoint>,
}

/// A template analytics data point.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TemplateDataPoint {
    /// The template.
    pub template_id: TemplateId,
    /// Day start.
    pub start: AnalyticsTime,
    /// Day end.
    pub end: AnalyticsTime,
    /// Sends.
    #[serde(default)]
    pub sent: Option<u64>,
    /// Deliveries.
    #[serde(default)]
    pub delivered: Option<u64>,
    /// Reads (tracked for 7 days after sending).
    #[serde(default)]
    pub read: Option<u64>,
    /// Button clicks (marketing and utility templates).
    #[serde(default)]
    pub clicked: Vec<ClickMetric>,
    /// Cost metrics.
    #[serde(default)]
    pub cost: Vec<CostMetric>,
    /// Any other metric in the data point, notably the MM API for WhatsApp
    /// conversion metrics, whose response names the page does not show.
    #[serde(flatten)]
    pub other: BTreeMap<String, serde_json::Value>,
}

/// A click metric.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClickMetric {
    /// Kind of click.
    #[serde(rename = "type")]
    pub kind: ClickType,
    /// Button text.
    #[serde(default)]
    pub button_content: Option<String>,
    /// Clicks.
    #[serde(default)]
    pub count: Option<u64>,
}

/// A cost metric.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CostMetric {
    /// Kind of cost.
    #[serde(rename = "type")]
    pub kind: CostType,
    /// Amount in the WABA's currency.
    #[serde(default)]
    pub value: Option<f64>,
}

/// An element of the template group analytics `data` array.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TemplateGroupAnalytics {
    /// `DAILY`.
    #[serde(default)]
    pub granularity: Option<Granularity>,
    /// Product the metrics are for.
    #[serde(default)]
    pub product_type: Option<String>,
    /// The WABA's timezone, with `use_waba_timezone`.
    #[serde(default)]
    pub waba_timezone: Option<String>,
    /// Data points.
    #[serde(default)]
    pub data_points: Vec<TemplateGroupDataPoint>,
}

/// A template group analytics data point.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TemplateGroupDataPoint {
    /// The template group.
    pub template_group_id: TemplateGroupId,
    /// Day start.
    pub start: AnalyticsTime,
    /// Day end.
    pub end: AnalyticsTime,
    /// Sends.
    #[serde(default)]
    pub sent: Option<u64>,
    /// Deliveries.
    #[serde(default)]
    pub delivered: Option<u64>,
    /// Reads.
    #[serde(default)]
    pub read: Option<u64>,
    /// Button clicks.
    #[serde(default)]
    pub clicked: Vec<ClickMetric>,
    /// Cost metrics.
    #[serde(default)]
    pub cost: Vec<CostMetric>,
}

/// An element of the group analytics `data` array.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GroupAnalytics {
    /// `DAILY`.
    #[serde(default)]
    pub granularity: Option<Granularity>,
    /// Data points.
    #[serde(default)]
    pub data_points: Vec<GroupDataPoint>,
}

/// A group analytics data point.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GroupDataPoint {
    /// The group.
    pub group_id: GroupId,
    /// Day start.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub start: OffsetDateTime,
    /// Day end.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub end: OffsetDateTime,
    /// Messages the business sent.
    #[serde(default)]
    pub sent: Option<u64>,
    /// Deliveries to participants.
    #[serde(default)]
    pub delivered: Option<u64>,
    /// Reads by participants.
    #[serde(default)]
    pub read: Option<u64>,
    /// Participants who joined.
    #[serde(default)]
    pub joined: Option<u64>,
    /// Participants who left.
    #[serde(default)]
    pub left: Option<u64>,
}
