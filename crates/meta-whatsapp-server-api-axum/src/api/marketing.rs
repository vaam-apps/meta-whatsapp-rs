//! The Marketing Messages API, max price and Click-to-WhatsApp welcome
//! message sequences (scope `marketing`; docs/design/server.md, section
//! 4.2; roadmap M5h), and the operator's signing of the max price
//! agreement (an admin key).
//!
//! | Route | Graph calls, with the tenant's WABA token | `Idempotency-Key` |
//! | --- | --- | --- |
//! | `POST /v1/numbers/{pn}/marketing-messages` | `POST /{pn}/marketing_messages` | yes |
//! | `GET /v1/wabas/{waba_id}/marketing/onboarding` | `GET /{waba_id}?fields=marketing_messages_onboarding_status`, `…owner_business_info` | no |
//! | `GET`, `PUT /v1/wabas/{waba_id}/marketing/cloud-api-marketing` | `GET /{waba_id}?fields=disable_marketing_messages_on_cloud_api`, `POST /{waba_id}` | no (sets a value) |
//! | `POST /v1/wabas/{waba_id}/marketing/template-duplicates` | the source's lookup in the WABA's templates, `POST /{waba_id}/message_templates` | yes |
//! | `GET /v1/wabas/{waba_id}/marketing/reach-estimates` | `GET /{waba_id}/reachestimate` | no |
//! | `GET`, `PUT`, `DELETE /v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price` | the template's lookup in the WABA's templates, `GET`, `POST /{template_id}` | no (sets a value) |
//! | `POST`, `GET /v1/wabas/{waba_id}/welcome-sequences` | `POST`, `GET /{waba_id}/welcome_message_sequences` | yes (the creation) |
//! | `GET`, `PATCH`, `DELETE /v1/wabas/{waba_id}/welcome-sequences/{sequence_id}` | the sequence's lookup (`sequence_id` filter), then `POST`, `DELETE /{waba_id}/welcome_message_sequences` | no |
//! | `POST /v1/admin/businesses/{business_id}/max-price-agreement` | `POST /{business_id}/max_price_agreements` | no; **a legal act** |
//!
//! The rules are the core's ([`meta_whatsapp_server_core::marketing`]):
//! a template or a sequence is the WABA's or it does not exist (`404`,
//! nothing changed); the agreement is the operator's explicit act, off
//! until the deployment sets `WA_SERVER_LEGAL_ACTS=on`, audited, and no
//! other route sends it; the signer's name and email, a send's recipient
//! and parameters, a welcome message and Meta's texts are in no log. This
//! module keeps the routes, the request and answer types with their
//! OpenAPI schema.

use std::collections::{BTreeMap, HashMap};

use meta_whatsapp_rs::client::marketing::{WelcomeMessage, WelcomeSequenceUpdate};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::marketing::{
    self as domain, DuplicationRequest, Marketing, MarketingSend, MarketingSends, MaxPriceAgreements,
    MaxPriceRequest, Signer,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::ApiJson;
use super::listing::csv;
use super::messages::{MessageAccepted, RecipientObject};
use super::templates::ApiQuery;
use crate::auth::{AdminCaller, Caller, OwnedNumber, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

fn marketing<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedWaba,
) -> Result<Marketing<'a>, ApiError> {
    Ok(Marketing::new(
        state.authz(),
        caller,
        owned.capability(state),
        state.template_cache(),
    )?
    .observed_by(state.metrics()))
}

// ─── The marketing send ──────────────────────────────────────────────────

/// What happens when the business has not finished Marketing Messages API
/// onboarding.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
pub enum ProductPolicy {
    /// Deliver through Cloud API instead (Meta's default).
    #[serde(rename = "CLOUD_API_FALLBACK")]
    CloudApiFallback,
    /// Fail rather than fall back to Cloud API.
    #[serde(rename = "STRICT")]
    Strict,
}

/// The per-message max price multiplier (max price beta).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BidSpec {
    /// Scales the template's max price for this message only: `1.5` is
    /// +50 %. Positive. Meta marks it subject to change during the beta.
    pub per_message_bid_multiplier: f64,
}

/// A marketing template to send through the Marketing Messages API.
// No `Debug`: its template holds a customer's data (the core's
// `MarketingSend` shows keys only).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = SendMarketingMessage)]
pub struct SendMarketingMessageBody {
    /// Who receives it.
    pub to: RecipientObject,
    /// Meta's template object (`name`, `language`, `components`), as the
    /// send-template pages write it: an approved `MARKETING` template.
    #[schema(value_type = HashMap<String, Value>)]
    pub template: Value,
    /// `CLOUD_API_FALLBACK` (Meta's default) or `STRICT`.
    pub product_policy: Option<ProductPolicy>,
    /// Share this message's activity (reads, clicks) with Meta for
    /// optimization; absent is the WABA's setting.
    pub message_activity_sharing: Option<bool>,
    /// The max price multiplier for this message (max price beta).
    pub bid_spec: Option<BidSpec>,
}

impl From<SendMarketingMessageBody> for MarketingSend {
    fn from(body: SendMarketingMessageBody) -> Self {
        let SendMarketingMessageBody {
            to,
            template,
            product_policy,
            message_activity_sharing,
            bid_spec,
        } = body;
        Self {
            to: to.into(),
            template,
            product_policy: product_policy.map(|policy| {
                match policy {
                    ProductPolicy::CloudApiFallback => "CLOUD_API_FALLBACK",
                    ProductPolicy::Strict => "STRICT",
                }
                .to_owned()
            }),
            message_activity_sharing,
            per_message_bid_multiplier: bid_spec.map(|b| b.per_message_bid_multiplier),
        }
    }
}

/// Send `message`: `202` with the accepted message, or Meta's refusal.
async fn send(
    state: &Grant,
    owned: &OwnedNumber,
    message: &domain::MarketingMessage,
) -> Result<Success, ApiError> {
    let accepted = MarketingSends::new(state.authz(), owned.capability(state))?
        .observed_by(state.metrics())
        .send(message)
        .await?;
    Success::json(StatusCode::ACCEPTED, &MessageAccepted::from(accepted))
}

/// `POST /v1/numbers/{pn}/marketing-messages`: send a marketing template
/// through the Marketing Messages API.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/marketing-messages",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never send twice (a repeat gets the kept answer, with `Idempotent-Replayed: true`)"),
    ),
    request_body = SendMarketingMessageBody,
    responses(
        (status = 202, description = "Accepted by Meta; delivery arrives as status events", body = MessageAccepted),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`marketing_not_allowed`, …)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or a catalog the template names that is not the number's WABA's; nothing was sent", body = ErrorBody),
        (status = 409, description = "`marketing_opted_out`, `number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`, …", body = ErrorBody),
        (status = 422, description = "`invalid_request` (with `field`: `to.phone` without `+`, `template.*`, `product_policy`, `bid_spec.per_message_bid_multiplier`, …), `idempotency_key_reused`: nothing was sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests` (the service's limits, `Retry-After`) or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the message may have been sent (`may_have_been_sent: true`)", body = ErrorBody),
    )
)]
pub(crate) async fn send_marketing_message(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::send_marketing_message(owned.phone_number_id(), &body);
    let message = serde_json::from_value::<SendMarketingMessageBody>(body)
        .map_err(|_| ApiError::invalid("body"))
        .and_then(|body| Ok(domain::marketing_message(MarketingSend::from(body))?));
    let message = match message {
        Ok(message) => message,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, send(&state, &owned, &message))
        .await
}

// ─── Onboarding and the Cloud API switch ─────────────────────────────────

/// The Terms of Service state of the business that owns the WABA.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = MarketingOwnerBusiness)]
pub struct OwnerBusinessView {
    /// The business portfolio's id.
    #[schema(required = true)]
    pub id: Option<String>,
    /// Its name.
    #[schema(required = true)]
    pub name: Option<String>,
    /// `TERM_OF_SERVICE_SIGNED`, `REQUEST_SENT` or `NOT_STARTED`, as Meta
    /// writes it.
    #[schema(required = true)]
    pub terms_status: Option<String>,
    /// When it was set, as Meta writes it.
    #[schema(required = true)]
    pub terms_time: Option<String>,
}

/// A WABA's Marketing Messages API onboarding.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = MarketingOnboarding)]
pub struct OnboardingView {
    /// The WABA's eligibility (`ELIGIBLE`, `ONBOARDED`, …), as Meta writes
    /// it; `null` when Meta answers none.
    #[schema(required = true)]
    pub onboarding_status: Option<String>,
    /// The owning business and its Terms of Service state; `null` when
    /// Meta answers none. The terms are accepted in WhatsApp Manager, not
    /// through this service.
    #[schema(required = true)]
    pub owner_business: Option<OwnerBusinessView>,
}

impl From<domain::Onboarding> for OnboardingView {
    fn from(onboarding: domain::Onboarding) -> Self {
        let domain::Onboarding {
            onboarding_status,
            owner_business,
        } = onboarding;
        Self {
            onboarding_status,
            owner_business: owner_business.map(|owner| OwnerBusinessView {
                id: owner.id,
                name: owner.name,
                terms_status: owner.terms_status,
                terms_time: owner.terms_time,
            }),
        }
    }
}

/// `GET /v1/wabas/{waba_id}/marketing/onboarding`: the WABA's Marketing
/// Messages API onboarding status and its owner business's Terms of
/// Service state (two reads of Meta).
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/marketing/onboarding",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The onboarding state", body = OnboardingView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_marketing_onboarding(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
) -> Result<Json<OnboardingView>, ApiError> {
    let onboarding = marketing(&state, &caller, &owned)?.onboarding().await?;
    Ok(Json(OnboardingView::from(onboarding)))
}

/// Whether marketing templates are blocked on Cloud API `/messages`.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CloudApiMarketing)]
pub struct CloudApiMarketingBody {
    /// `true` blocks marketing templates on Cloud API `/messages` (Meta
    /// answers them with `131063`, `marketing_not_allowed`); `false`
    /// allows them (Meta's default). Effective once the WABA is fully
    /// onboarded.
    pub disabled: bool,
}

/// The switch as Meta answers it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CloudApiMarketingState)]
pub struct CloudApiMarketingView {
    /// The switch; `null` when Meta answers none (its default is `false`).
    #[schema(required = true)]
    pub disabled: Option<bool>,
}

/// `GET /v1/wabas/{waba_id}/marketing/cloud-api-marketing`: whether
/// marketing templates are blocked on Cloud API.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/marketing/cloud-api-marketing",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The switch", body = CloudApiMarketingView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_cloud_api_marketing(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
) -> Result<Json<CloudApiMarketingView>, ApiError> {
    let disabled = marketing(&state, &caller, &owned)?
        .cloud_api_marketing()
        .await?;
    Ok(Json(CloudApiMarketingView { disabled }))
}

/// `PUT /v1/wabas/{waba_id}/marketing/cloud-api-marketing`: block or allow
/// marketing templates on Cloud API `/messages`. Audited.
#[utoipa::path(
    put,
    path = "/v1/wabas/{waba_id}/marketing/cloud-api-marketing",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = CloudApiMarketingBody,
    responses(
        (status = 204, description = "Set"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `body`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: read the switch", body = ErrorBody),
    )
)]
pub(crate) async fn set_cloud_api_marketing(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiJson(body): ApiJson<CloudApiMarketingBody>,
) -> Result<StatusCode, ApiError> {
    marketing(&state, &caller, &owned)?
        .set_cloud_api_marketing(body.disabled)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Max price ───────────────────────────────────────────────────────────

/// A max price (Meta's `optimization_spec`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = OptimizationSpec)]
pub struct OptimizationSpecBody {
    /// The most Meta charges per 1,000 deliveries, in the smallest unit of
    /// the WABA's currency: 1 to 2147483647. A price of ₹0.87 per delivery
    /// is `87000`.
    pub bid_amount: u64,
    /// `LOWEST_COST_WITH_BID_CAP`, the only strategy Meta supports (the
    /// default).
    pub bid_strategy: Option<String>,
    /// Destination country (two capital letters) to the multiplier applied
    /// to `bid_amount` for sends there: 0 (exclusive) to 10, at most 240
    /// entries. Absent keeps the template's map on an update; an empty map
    /// removes every override; a non-empty one replaces the map whole.
    #[schema(value_type = Option<HashMap<String, f64>>)]
    pub bid_country_multiplier_overrides: Option<BTreeMap<String, f64>>,
}

impl From<OptimizationSpecBody> for MaxPriceRequest {
    fn from(body: OptimizationSpecBody) -> Self {
        Self {
            bid_amount: body.bid_amount,
            bid_strategy: body.bid_strategy,
            bid_country_multiplier_overrides: body.bid_country_multiplier_overrides,
        }
    }
}

/// A template to duplicate at another max price.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = DuplicateMarketingTemplate)]
pub struct DuplicateMarketingTemplateBody {
    /// The approved `MARKETING` template to clone: one of the WABA's own.
    pub source_template_id: String,
    /// The duplicate's name: unique in the WABA; lowercase letters, digits
    /// and `_`, at most 512 characters.
    pub name: String,
    /// The duplicate's max price.
    pub optimization_spec: OptimizationSpecBody,
}

/// The duplicate Meta made.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = DuplicatedTemplate)]
pub struct DuplicatedView {
    /// The duplicate's template id.
    pub id: String,
    /// Its status (`APPROVED` at once for an approved source).
    #[schema(required = true)]
    pub status: Option<String>,
    /// Its category, cloned from the source.
    #[schema(required = true)]
    pub category: Option<String>,
    /// The template it was cloned from.
    #[schema(required = true)]
    pub source_template_id: Option<String>,
}

impl From<domain::Duplicated> for DuplicatedView {
    fn from(duplicated: domain::Duplicated) -> Self {
        let domain::Duplicated {
            id,
            status,
            category,
            source_template_id,
        } = duplicated;
        Self {
            id,
            status,
            category,
            source_template_id,
        }
    }
}

async fn duplicate(
    marketing: &Marketing<'_>,
    request: &meta_whatsapp_rs::client::marketing::DuplicateTemplate,
) -> Result<Success, ApiError> {
    let duplicated = marketing.duplicate_template(request).await?;
    Success::json(StatusCode::CREATED, &DuplicatedView::from(duplicated))
}

/// `POST /v1/wabas/{waba_id}/marketing/template-duplicates`: duplicate one
/// of the WABA's approved `MARKETING` templates at another max price. Does
/// not sign the max price agreement: a WABA that has not accepted it gets
/// Meta's refusal.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/marketing/template-duplicates",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never create twice"),
    ),
    request_body = DuplicateMarketingTemplateBody,
    responses(
        (status = 201, description = "Created", body = DuplicatedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (the WABA is not enrolled in max price)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or a source template that is not this WABA's; nothing was created", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `source_template_id`, `name`, `optimization_spec.*` or `body`; `invalid_parameter` (a name in use, a source not approved or at its duplicate cap), `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the duplicate may have been created", body = ErrorBody),
    )
)]
pub(crate) async fn duplicate_marketing_template(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::duplicate_marketing_template(owned.waba_id(), &body);
    let request = serde_json::from_value::<DuplicateMarketingTemplateBody>(body)
        .map_err(|_| ApiError::invalid("body"))
        .and_then(|body| {
            Ok(domain::duplication(&DuplicationRequest {
                source_template_id: body.source_template_id,
                name: body.name,
                optimization_spec: body.optimization_spec.into(),
            })?)
        });
    let request = match request {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let service = match marketing(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, duplicate(&service, &request))
        .await
}

/// `?countries=` and `?date_interval=`, as the OpenAPI document declares
/// them.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ReachEstimateQuery {
    /// Destination countries, two capital letters each, comma-separated:
    /// at least one, at most 240.
    pub countries: String,
    /// The history the estimate is computed from: `L1D`, `L7D`, `L14D` or
    /// `L28D`.
    pub date_interval: String,
}

/// One max price of a reach estimate.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ReachEstimatePoint)]
pub struct ReachPointView {
    /// Max price per 1,000 deliveries, in the currency's smallest unit.
    #[schema(required = true)]
    pub bid_amount: Option<u64>,
    /// Targeted users (fixed at 1,000 during the beta).
    #[schema(required = true)]
    pub users: Option<u64>,
    /// Lower bound of the estimated deliveries.
    #[schema(required = true)]
    pub deliveries_lower_bound: Option<u64>,
    /// Upper bound of the estimated deliveries.
    #[schema(required = true)]
    pub deliveries_upper_bound: Option<u64>,
    /// Lower bound of the estimated average cost per 1,000 deliveries.
    #[schema(required = true)]
    pub cost_lower_bound: Option<f64>,
    /// Upper bound of the estimated average cost per 1,000 deliveries.
    #[schema(required = true)]
    pub cost_upper_bound: Option<f64>,
}

/// Estimated deliveries and costs at several max prices: for planning
/// only, a guarantee of nothing.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ReachEstimate)]
pub struct ReachView {
    /// The WABA's currency: the unit of every amount is its smallest unit.
    #[schema(required = true)]
    pub waba_currency: Option<String>,
    /// One entry per max price.
    pub estimates: Vec<ReachPointView>,
}

impl From<domain::Reach> for ReachView {
    fn from(reach: domain::Reach) -> Self {
        Self {
            waba_currency: reach.waba_currency,
            estimates: reach
                .estimates
                .into_iter()
                .map(|p| ReachPointView {
                    bid_amount: p.bid_amount,
                    users: p.users,
                    deliveries_lower_bound: p.deliveries_lower_bound,
                    deliveries_upper_bound: p.deliveries_upper_bound,
                    cost_lower_bound: p.cost_lower_bound,
                    cost_upper_bound: p.cost_upper_bound,
                })
                .collect(),
        }
    }
}

/// `GET /v1/wabas/{waba_id}/marketing/reach-estimates`: estimated
/// deliveries and costs at several max prices.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/marketing/reach-estimates",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ReachEstimateQuery,
    ),
    responses(
        (status = 200, description = "The estimates", body = ReachView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `countries`, `date_interval` or `query`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_reach_estimate(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(query): ApiQuery<ReachEstimateQuery>,
) -> Result<Json<ReachView>, ApiError> {
    let countries = csv(Some(&query.countries));
    let query = domain::reach_query(&countries, &query.date_interval)?;
    let reach = marketing(&state, &caller, &owned)?
        .reach_estimate(&query)
        .await?;
    Ok(Json(ReachView::from(reach)))
}

/// A template's max price, as Meta describes it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = TemplateMaxPrice)]
pub struct TemplateMaxPriceView {
    /// Max price per 1,000 deliveries; `null` on a rate-card template.
    #[schema(required = true)]
    pub bid_amount: Option<u64>,
    /// `LOWEST_COST_WITH_BID_CAP`; `null` on a rate-card template.
    #[schema(required = true)]
    pub bid_strategy: Option<String>,
    /// Country to multiplier.
    #[schema(value_type = HashMap<String, f64>)]
    pub bid_country_multiplier_overrides: BTreeMap<String, f64>,
}

/// `GET /v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price`:
/// a template's max price (a rate-card template has none).
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("template_id" = String, Path, description = "Template id: one of the WABA's"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The template's max price", body = TemplateMaxPriceView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such template in this WABA (another WABA's looks like a missing one)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `template_id`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_template_max_price(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, template_id)): Path<(String, String)>,
) -> Result<Json<TemplateMaxPriceView>, ApiError> {
    let id = domain::template_id("template_id", &template_id)?;
    let price = marketing(&state, &caller, &owned)?
        .template_max_price(&id)
        .await?;
    Ok(Json(price.map_or_else(
        || TemplateMaxPriceView {
            bid_amount: None,
            bid_strategy: None,
            bid_country_multiplier_overrides: BTreeMap::new(),
        },
        |p| TemplateMaxPriceView {
            bid_amount: p.bid_amount,
            bid_strategy: p.bid_strategy,
            bid_country_multiplier_overrides: p.bid_country_multiplier_overrides,
        },
    )))
}

/// `PUT /v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price`:
/// set a template's max price (it applies within an hour). Audited. Does
/// not sign the max price agreement.
#[utoipa::path(
    put,
    path = "/v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("template_id" = String, Path, description = "Template id: one of the WABA's"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = OptimizationSpecBody,
    responses(
        (status = 204, description = "Set"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (the WABA is not enrolled in max price)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such template in this WABA; nothing changed", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `template_id`, `bid_amount`, `bid_strategy`, `bid_country_multiplier_overrides` or `body`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: read the template's max price", body = ErrorBody),
    )
)]
pub(crate) async fn set_template_max_price(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, template_id)): Path<(String, String)>,
    ApiJson(body): ApiJson<OptimizationSpecBody>,
) -> Result<StatusCode, ApiError> {
    let id = domain::template_id("template_id", &template_id)?;
    let price = domain::max_price("", &body.into())?;
    marketing(&state, &caller, &owned)?
        .set_template_max_price(&id, &price)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price`:
/// return a template to rate-card pricing (its country multipliers go
/// too). Audited.
#[utoipa::path(
    delete,
    path = "/v1/wabas/{waba_id}/marketing/templates/{template_id}/max-price",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("template_id" = String, Path, description = "Template id: one of the WABA's"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Cleared"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such template in this WABA; nothing changed", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `template_id`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: read the template's max price", body = ErrorBody),
    )
)]
pub(crate) async fn clear_template_max_price(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, template_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let id = domain::template_id("template_id", &template_id)?;
    marketing(&state, &caller, &owned)?
        .clear_template_max_price(&id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── Welcome message sequences ───────────────────────────────────────────

/// A new welcome message sequence.
// No `Debug`: a welcome message is the business's text, kept out of logs.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CreateWelcomeSequence)]
pub struct CreateWelcomeSequenceBody {
    /// The sequence's name, 1 to 128 characters.
    pub name: String,
    /// Meta's `welcome_message_sequence`: `text`, `autofill_message`
    /// (`{"content"}`) and `ice_breakers` (`[{"title"}]`), at least one;
    /// the texts at most 1,024 characters, at most 20 ice breakers.
    #[schema(value_type = HashMap<String, Value>)]
    pub welcome_message_sequence: Value,
}

/// What to change in a sequence: at least one.
// No `Debug`, as `CreateWelcomeSequenceBody`.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = UpdateWelcomeSequence)]
pub struct UpdateWelcomeSequenceBody {
    /// A new name, 1 to 128 characters.
    pub name: Option<String>,
    /// A new welcome message, as at creation.
    #[schema(value_type = Option<HashMap<String, Value>>)]
    pub welcome_message_sequence: Option<Value>,
}

/// A created sequence.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = WelcomeSequenceCreated)]
pub struct SequenceCreated {
    /// Its id: for the ad's creative and the calls on it.
    pub sequence_id: String,
}

/// One quick reply of a welcome message.
#[derive(Debug, Serialize, ToSchema)]
pub struct IceBreakerView {
    /// The reply's title.
    pub title: String,
}

/// The message prefilled in the user's composer.
#[derive(Debug, Serialize, ToSchema)]
pub struct AutofillMessageView {
    /// The prefilled text.
    pub content: String,
}

/// A welcome message, as the service reads Meta's.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = WelcomeMessage)]
pub struct WelcomeMessageView {
    /// The welcome text.
    #[schema(required = true)]
    pub text: Option<String>,
    /// The message prefilled for the user to send.
    #[schema(required = true)]
    pub autofill_message: Option<AutofillMessageView>,
    /// Quick replies offered to the user.
    pub ice_breakers: Vec<IceBreakerView>,
}

impl From<WelcomeMessage> for WelcomeMessageView {
    fn from(message: WelcomeMessage) -> Self {
        Self {
            text: message.text,
            autofill_message: message
                .autofill_message
                .map(|a| AutofillMessageView { content: a.content }),
            ice_breakers: message
                .ice_breakers
                .into_iter()
                .map(|b| IceBreakerView { title: b.title })
                .collect(),
        }
    }
}

/// A welcome message sequence.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = WelcomeSequence)]
pub struct SequenceView {
    /// Its id.
    pub sequence_id: String,
    /// Its name.
    #[schema(required = true)]
    pub name: Option<String>,
    /// Its welcome message; `null` when Meta's is in a shape the service
    /// does not read.
    #[schema(required = true)]
    pub welcome_message_sequence: Option<WelcomeMessageView>,
    /// Whether an ad uses it (then Meta refuses to change or delete it
    /// while the ad is active).
    #[schema(required = true)]
    pub is_used_in_ad: Option<bool>,
}

impl From<domain::Sequence> for SequenceView {
    fn from(sequence: domain::Sequence) -> Self {
        let domain::Sequence {
            sequence_id,
            name,
            welcome_message,
            is_used_in_ad,
        } = sequence;
        Self {
            sequence_id,
            name,
            welcome_message_sequence: welcome_message.map(WelcomeMessageView::from),
            is_used_in_ad,
        }
    }
}

/// The WABA's sequences.
#[derive(Debug, Serialize, ToSchema)]
pub struct SequenceList {
    /// The sequences (Meta documents no cursor).
    pub data: Vec<SequenceView>,
}

/// `?limit=`, as the OpenAPI document declares it.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct SequencesQuery {
    /// How many sequences to fetch, 1 to 100 (Meta's default without).
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u32>,
}

async fn create(
    marketing: &Marketing<'_>,
    name: &str,
    message: &WelcomeMessage,
) -> Result<Success, ApiError> {
    let sequence_id = marketing.create_sequence(name, message).await?;
    Success::json(StatusCode::CREATED, &SequenceCreated { sequence_id })
}

/// `POST /v1/wabas/{waba_id}/welcome-sequences`: create a Click-to-WhatsApp
/// welcome message sequence.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/welcome-sequences",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never create twice"),
    ),
    request_body = CreateWelcomeSequenceBody,
    responses(
        (status = 201, description = "Created", body = SequenceCreated),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `name`, `welcome_message_sequence.*` or `body`; `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the sequence may have been created", body = ErrorBody),
    )
)]
pub(crate) async fn create_welcome_sequence(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_welcome_sequence(owned.waba_id(), &body);
    let parsed = serde_json::from_value::<CreateWelcomeSequenceBody>(body)
        .map_err(|_| ApiError::invalid("body"))
        .and_then(|body| {
            domain::sequence_name(&body.name)?;
            let message = domain::welcome_message(&body.welcome_message_sequence)?;
            Ok((body.name, message))
        });
    let (name, message) = match parsed {
        Ok(parsed) => parsed,
        Err(error) => return error.into_response(),
    };
    let service = match marketing(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, create(&service, &name, &message))
        .await
}

/// `GET /v1/wabas/{waba_id}/welcome-sequences`: the WABA's welcome message
/// sequences.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/welcome-sequences",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        SequencesQuery,
    ),
    responses(
        (status = 200, description = "The sequences", body = SequenceList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit` or `query`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_welcome_sequences(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    ApiQuery(query): ApiQuery<SequencesQuery>,
) -> Result<Json<SequenceList>, ApiError> {
    let sequences = marketing(&state, &caller, &owned)?
        .sequences(query.limit)
        .await?;
    Ok(Json(SequenceList {
        data: sequences.into_iter().map(SequenceView::from).collect(),
    }))
}

/// `GET /v1/wabas/{waba_id}/welcome-sequences/{sequence_id}`: one of the
/// WABA's sequences.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/welcome-sequences/{sequence_id}",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("sequence_id" = String, Path, description = "Sequence id: one of the WABA's"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The sequence", body = SequenceView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such sequence in this WABA (another WABA's looks like a missing one)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `sequence_id`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_welcome_sequence(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, sequence_id)): Path<(String, String)>,
) -> Result<Json<SequenceView>, ApiError> {
    let id = domain::sequence_id("sequence_id", &sequence_id)?;
    let sequence = marketing(&state, &caller, &owned)?.sequence(&id).await?;
    Ok(Json(SequenceView::from(sequence)))
}

/// `PATCH /v1/wabas/{waba_id}/welcome-sequences/{sequence_id}`: change a
/// sequence's name or message (Meta refuses one an active ad uses).
/// Audited.
#[utoipa::path(
    patch,
    path = "/v1/wabas/{waba_id}/welcome-sequences/{sequence_id}",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("sequence_id" = String, Path, description = "Sequence id: one of the WABA's"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = UpdateWelcomeSequenceBody,
    responses(
        (status = 204, description = "Changed"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such sequence in this WABA; nothing changed", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `sequence_id`, `update` (nothing to change), `name`, `welcome_message_sequence.*` or `body`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the sequence may have been changed", body = ErrorBody),
    )
)]
pub(crate) async fn update_welcome_sequence(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, sequence_id)): Path<(String, String)>,
    ApiJson(body): ApiJson<UpdateWelcomeSequenceBody>,
) -> Result<StatusCode, ApiError> {
    let id = domain::sequence_id("sequence_id", &sequence_id)?;
    let update: WelcomeSequenceUpdate =
        domain::sequence_update(body.name.as_deref(), body.welcome_message_sequence.as_ref())?;
    marketing(&state, &caller, &owned)?
        .update_sequence(&id, &update)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/wabas/{waba_id}/welcome-sequences/{sequence_id}`: delete a
/// sequence (Meta refuses one an active ad uses). Audited.
#[utoipa::path(
    delete,
    path = "/v1/wabas/{waba_id}/welcome-sequences/{sequence_id}",
    tag = "marketing",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("sequence_id" = String, Path, description = "Sequence id: one of the WABA's"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such sequence in this WABA; nothing deleted", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `sequence_id`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the sequence may have been deleted", body = ErrorBody),
    )
)]
pub(crate) async fn delete_welcome_sequence(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, sequence_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let id = domain::sequence_id("sequence_id", &sequence_id)?;
    marketing(&state, &caller, &owned)?.delete_sequence(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ─── The operator's agreement ────────────────────────────────────────────

/// Who signs Meta's max price beta agreement, and the token to sign with.
// No `Debug`: the name and the email are personal data, the token a
// secret.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = SignMaxPriceAgreement)]
pub struct SignMaxPriceAgreementBody {
    /// The person signing, printed on the agreement.
    pub signer_name: String,
    /// Where Meta emails the acceptance link.
    pub signer_email: String,
    /// A system user or business token with `business_management`, an
    /// admin of the business portfolio's. Used for this call only; never
    /// stored.
    pub token: String,
}

/// What Meta answered, without the signer.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = MaxPriceAgreementSigned)]
pub struct AgreementSignedView {
    /// `pending` (Meta emailed the acceptance link), as Meta writes it.
    #[schema(required = true)]
    pub status: Option<String>,
}

/// `POST /v1/admin/businesses/{business_id}/max-price-agreement`: sign
/// Meta's max price beta agreement for a business portfolio. **A legal
/// act**: the operator's explicit decision, off until the deployment sets
/// `WA_SERVER_LEGAL_ACTS=on`, audited under the admin key; no other route
/// signs it. Meta emails the signer an acceptance link; the business
/// accepts there.
#[utoipa::path(
    post,
    path = "/v1/admin/businesses/{business_id}/max-price-agreement",
    tag = "admin",
    security(("api_key" = [])),
    params(("business_id" = String, Path, description = "Business portfolio id")),
    request_body = SignMaxPriceAgreementBody,
    responses(
        (status = 200, description = "Meta emailed the signer: the agreement is `pending`; audited `max_price_agreement_signed`", body = AgreementSignedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`feature_not_available` (the deployment has not enabled the legal acts: nothing was asked of Meta), not an admin key, or Meta's refusal (the token lacks `business_management`, or its holder is no admin of the portfolio)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `business_id`, `signer_name`, `signer_email`, `token` or `body`; Meta's `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: Meta answers `429` within 5 minutes of the last email (`retry_after`)", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent` (the agreement is never sent again by the service: read its state with Meta, or wait)", body = ErrorBody),
        (status = 504, description = "`timeout`: the email may have been sent", body = ErrorBody),
    )
)]
pub(crate) async fn sign_max_price_agreement(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(business_id): Path<String>,
    ApiJson(body): ApiJson<SignMaxPriceAgreementBody>,
) -> Result<Json<AgreementSignedView>, ApiError> {
    let SignMaxPriceAgreementBody {
        signer_name,
        signer_email,
        token,
    } = body;
    let signed = MaxPriceAgreements::new(state.authz(), &admin)?
        .observed_by(state.metrics())
        .with_legal_acts(state.settings().legal_acts)
        .sign(
            &business_id,
            &Signer {
                name: signer_name,
                email: signer_email,
            },
            AccessToken::new(token),
        )
        .await?;
    Ok(Json(AgreementSignedView {
        status: signed.status,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Every field of a request reaches the core's send, and the policy
    /// keeps Meta's spelling. Decisive: each field of the conversion.
    #[test]
    fn every_field_of_the_send_reaches_the_core() {
        let body: SendMarketingMessageBody = serde_json::from_value(json!({
            "to": {"phone": "+16505551234"},
            "template": {"name": "seasonal_sale_promo", "language": {"code": "en_US"}},
            "product_policy": "STRICT",
            "message_activity_sharing": true,
            "bid_spec": {"per_message_bid_multiplier": 1.5},
        }))
        .unwrap();
        let send = MarketingSend::from(body);
        assert_eq!(send.to.phone.as_deref(), Some("+16505551234"));
        assert_eq!(send.template["name"], "seasonal_sale_promo");
        assert_eq!(send.product_policy.as_deref(), Some("STRICT"));
        assert_eq!(send.message_activity_sharing, Some(true));
        assert_eq!(send.per_message_bid_multiplier, Some(1.5));
    }

    /// Every field of the core's answers reaches the service's.
    #[test]
    fn every_field_of_the_cores_answers_is_answered() {
        let sequence = domain::Sequence {
            sequence_id: "0139138".to_owned(),
            name: Some("Driver Sign up".to_owned()),
            welcome_message: Some(WelcomeMessage::new().text("Hi").autofill("Hello").ice_breaker("One")),
            is_used_in_ad: Some(false),
        };
        assert_eq!(
            serde_json::to_value(SequenceView::from(sequence)).unwrap(),
            json!({"sequence_id": "0139138", "name": "Driver Sign up",
                   "welcome_message_sequence": {"text": "Hi",
                       "autofill_message": {"content": "Hello"},
                       "ice_breakers": [{"title": "One"}]},
                   "is_used_in_ad": false})
        );
        let reach = domain::Reach {
            waba_currency: Some("USD".to_owned()),
            estimates: vec![domain::ReachPoint {
                bid_amount: Some(5000),
                users: Some(1000),
                deliveries_lower_bound: Some(500),
                deliveries_upper_bound: Some(900),
                cost_lower_bound: Some(4000.0),
                cost_upper_bound: Some(5000.0),
            }],
        };
        assert_eq!(
            serde_json::to_value(ReachView::from(reach)).unwrap(),
            json!({"waba_currency": "USD", "estimates": [{"bid_amount": 5000, "users": 1000,
                "deliveries_lower_bound": 500, "deliveries_upper_bound": 900,
                "cost_lower_bound": 4000.0, "cost_upper_bound": 5000.0}]})
        );
        let duplicated = domain::Duplicated {
            id: "2".to_owned(),
            status: Some("APPROVED".to_owned()),
            category: Some("MARKETING".to_owned()),
            source_template_id: Some("1".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(DuplicatedView::from(duplicated)).unwrap(),
            json!({"id": "2", "status": "APPROVED", "category": "MARKETING",
                   "source_template_id": "1"})
        );
        let onboarding = domain::Onboarding {
            onboarding_status: Some("ELIGIBLE".to_owned()),
            owner_business: Some(domain::OwnerBusiness {
                id: Some("1".to_owned()),
                name: Some("N".to_owned()),
                terms_status: Some("NOT_STARTED".to_owned()),
                terms_time: Some("T".to_owned()),
            }),
        };
        assert_eq!(
            serde_json::to_value(OnboardingView::from(onboarding)).unwrap(),
            json!({"onboarding_status": "ELIGIBLE", "owner_business": {"id": "1",
                "name": "N", "terms_status": "NOT_STARTED", "terms_time": "T"}})
        );
    }
}
