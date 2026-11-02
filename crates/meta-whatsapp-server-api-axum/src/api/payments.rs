//! Payments (scope `payments`; docs/design/server.md, section 4.2;
//! roadmap P2): India's payment configurations, a payment's lookup and its
//! refund. The payment **messages** (`order_details`, `order_status`) and
//! the payment templates are sends (`POST /v1/numbers/{pn}/messages`,
//! scope `send`); the payment webhooks arrive as events
//! (`payment_configuration_updated`; a status of type `payment` in
//! `status_updated`; Brazil's `payment_method` reply in
//! `message_received`).
//!
//! | Route | Graph call, with the tenant's WABA token |
//! | --- | --- |
//! | `GET /v1/wabas/{waba_id}/payment-configurations` | `GET /{waba_id}/payment_configurations` |
//! | `POST /v1/wabas/{waba_id}/payment-configurations` | `POST /{waba_id}/payment_configuration` |
//! | `GET /v1/wabas/{waba_id}/payment-configurations/{configuration_name}` | `GET /{waba_id}/payment_configuration/{configuration_name}` |
//! | `DELETE /v1/wabas/{waba_id}/payment-configurations/{configuration_name}` | `DELETE /{waba_id}/payment_configuration` |
//! | `POST /v1/wabas/{waba_id}/payment-configurations/{configuration_name}/oauth-link` | `POST /{waba_id}/generate_payment_configuration_oauth_link` |
//! | `GET /v1/numbers/{pn}/payments/{configuration_name}/{reference_id}` | `GET /{waba_id}/payment_configuration/{configuration_name}` (the number's WABA's, or `404`), then `GET /{pn}/payments/{configuration_name}/{reference_id}` |
//! | `POST /v1/numbers/{pn}/payments/refunds` | the same check of `payment_config_id`, then `POST /{pn}/payments_refund` |
//!
//! Pages: `payments/payments-in/onboarding-apis`, `payments/payments-in/pg`.
//!
//! The rules are the core's ([`meta_whatsapp_server_core::payments`]: the
//! checks, the configuration looked for in the number's own WABA, the
//! audit events). This module keeps the routes, the request and answer
//! types with their OpenAPI schema (Meta's field names; times as RFC 3339
//! strings, as every time of the service), converts them, and renders the
//! answers. A creation, a new OAuth link and a refund take an
//! `Idempotency-Key`; the answer a key keeps for a creation or a new link
//! has no `oauth_url` (the link is a credential the service does not
//! store: a repeat answers `null`; ask for a new link).

use std::collections::BTreeMap;

use meta_whatsapp_rs::client::payments::{
    CodeDescription, Payment, PaymentConfiguration, PaymentOauthLink, PaymentRefund, RefundCreated,
    Transaction,
};
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::{HeaderValue, StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::payments::{self as domain, PaymentConfigurations, Payments};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use utoipa::ToSchema;

use super::common::{ApiJson, rfc3339};
use super::messages::OptionalJson;
use crate::auth::{Caller, OwnedNumber, OwnedWaba};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

// ─── The answers ─────────────────────────────────────────────────────────

/// A code with its description (a merchant category or purpose code).
#[derive(Debug, Serialize, ToSchema)]
pub struct PaymentCode {
    /// The code (`0000`).
    pub code: String,
    /// What it means.
    pub description: String,
}

/// A payment configuration of a WABA (`payments/payments-in/onboarding-apis`).
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = PaymentConfiguration)]
pub struct PaymentConfigurationView {
    /// Its name: what an `order_details` message's
    /// `payment_configuration` names.
    pub configuration_name: String,
    /// Merchant category code.
    #[schema(required = true)]
    pub merchant_category_code: Option<PaymentCode>,
    /// Purpose code.
    #[schema(required = true)]
    pub purpose_code: Option<PaymentCode>,
    /// `Active`, `Needs_Connecting` (link the gateway: a new OAuth link)
    /// or `Needs_Testing`, as Meta's tables write them; another value as
    /// Meta wrote it.
    pub status: String,
    /// The gateway's merchant id.
    #[schema(required = true)]
    pub provider_mid: Option<String>,
    /// The provider (`upi_vpa`, `razorpay`, `payu`, `zaakpay`), lower
    /// case; another value as Meta wrote it.
    #[schema(required = true)]
    pub provider_name: Option<String>,
    /// The merchant's UPI handle.
    #[schema(required = true)]
    pub merchant_vpa: Option<String>,
    /// Created at (RFC 3339).
    #[schema(required = true, format = DateTime)]
    pub created_timestamp: Option<String>,
    /// Last updated at (RFC 3339).
    #[schema(required = true, format = DateTime)]
    pub updated_timestamp: Option<String>,
}

/// A WABA's payment configurations (Meta documents no pages).
#[derive(Debug, Serialize, ToSchema)]
pub struct PaymentConfigurationList {
    /// The configurations.
    pub data: Vec<PaymentConfigurationView>,
}

/// The OAuth link that links a payment configuration to the merchant's
/// gateway account. A credential: give it to the merchant only.
#[derive(Serialize, ToSchema)]
#[schema(as = PaymentOauthLink)]
pub struct PaymentOauthLinkView {
    /// The link (`null` for a `upi_vpa` configuration, and in an answer
    /// replayed for an `Idempotency-Key`: the service does not keep it).
    #[schema(required = true)]
    pub oauth_url: Option<String>,
    /// When the link expires (RFC 3339).
    #[schema(required = true, format = DateTime)]
    pub expiration: Option<String>,
}

impl std::fmt::Debug for PaymentOauthLinkView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaymentOauthLinkView")
            .field("oauth_url", &self.oauth_url.as_ref().map(|_| "(withheld)"))
            .field("expiration", &self.expiration)
            .finish()
    }
}

/// An amount: `value` is the amount times `offset` (₹12.34 is `value`
/// 1234, `offset` 100).
#[derive(Debug, Serialize, ToSchema)]
pub struct PaymentAmount {
    /// The amount times `offset`.
    pub value: u64,
    /// `100` for INR.
    pub offset: u32,
}

/// How the buyer paid.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = PaymentMethod)]
pub struct PaymentMethodView {
    /// `upi`, `card`, `wallet` or `netbanking`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// Why an attempt failed, as the gateway says.
#[derive(Debug, Serialize, ToSchema)]
pub struct PaymentTransactionError {
    /// The gateway's code.
    pub code: String,
    /// The gateway's reason.
    pub reason: String,
}

/// A refund of a transaction.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = PaymentRefund)]
pub struct PaymentRefundView {
    /// The refund's id.
    pub id: String,
    /// How much.
    pub amount: PaymentAmount,
    /// `instant` or `normal`.
    pub speed_processed: String,
    /// `pending`, `success` or `failed`.
    pub status: String,
    /// Created at (RFC 3339).
    #[schema(format = DateTime)]
    pub created_timestamp: String,
    /// Last updated at (RFC 3339).
    #[schema(format = DateTime)]
    pub updated_timestamp: String,
}

/// One payment attempt.
#[derive(Debug, Serialize, ToSchema)]
pub struct PaymentTransaction {
    /// The gateway's order id.
    pub id: String,
    /// The gateway's payment id.
    #[schema(required = true)]
    pub pg_transaction_id: Option<String>,
    /// The gateway: `billdesk`, `razorpay`, `payu` or `zaakpay`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `pending`, `success` or `failed`.
    pub status: String,
    /// Created at (RFC 3339).
    #[schema(format = DateTime)]
    pub created_timestamp: String,
    /// Last updated at (RFC 3339).
    #[schema(format = DateTime)]
    pub updated_timestamp: String,
    /// The amount paid, when Meta gives it.
    #[schema(required = true)]
    pub amount: Option<PaymentAmount>,
    /// The order's amount, when Meta gives it.
    #[schema(required = true)]
    pub order_amount: Option<PaymentAmount>,
    /// The currency, when Meta gives it.
    #[schema(required = true)]
    pub currency: Option<String>,
    /// How the buyer paid; may be absent for a failed attempt.
    #[schema(required = true)]
    pub method: Option<PaymentMethodView>,
    /// Why it failed; not always given.
    #[schema(required = true)]
    pub error: Option<PaymentTransactionError>,
    /// Its refunds.
    pub refunds: Vec<PaymentRefundView>,
}

/// The payment of an order (`payments/payments-in/pg`, "Step 5"), with
/// the gateway fields the `order_details` message carried, as it carried
/// them (only those it set).
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Payment)]
pub struct PaymentView {
    /// The order's `reference_id`.
    pub reference_id: String,
    /// `pending` (no attempt yet, or only failed ones) or `captured`.
    pub status: String,
    /// `INR`.
    pub currency: String,
    /// The amount to pay.
    pub amount: PaymentAmount,
    /// The attempts; empty before the first.
    pub transactions: Vec<PaymentTransaction>,
    /// Razorpay's `receipt`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    /// Razorpay's `notes`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, String>,
    /// `BillDesk`'s `additional_info1` to `additional_info7`, `PayU`'s
    /// `udf1` to `udf4`, Zaakpay's `extra1` and `extra2`, by name.
    #[serde(flatten)]
    #[schema(value_type = HashMap<String, String>)]
    pub fields: BTreeMap<String, String>,
}

/// A payment lookup's answer: Meta's `payments`.
#[derive(Debug, Serialize, ToSchema)]
pub struct PaymentList {
    /// The payments of the order (one, as Meta's example shows).
    pub data: Vec<PaymentView>,
}

/// A refund asked for (`payments/payments-in/pg`, "Step 8").
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = RefundCreated)]
pub struct RefundCreatedView {
    /// The refund's id.
    pub id: String,
    /// `pending`, `failed` or `completed`.
    pub status: String,
    /// `instant` or `normal`: the gateway decides.
    pub speed_processed: String,
}

// ─── The requests ────────────────────────────────────────────────────────

/// A payment configuration to create, as Meta's page writes it
/// (`payments/payments-in/onboarding-apis`, "Create a payment
/// configuration").
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CreatePaymentConfiguration)]
pub struct CreatePaymentConfigurationBody {
    /// Its name, what orders name it by: 1 to 60 characters, one path
    /// segment (no `/` or `\`).
    pub configuration_name: String,
    /// `upi_vpa`, `razorpay`, `payu` or `zaakpay`.
    pub provider_name: String,
    /// Merchant category code (`0000`).
    pub merchant_category_code: Option<String>,
    /// Purpose code (`00`).
    pub purpose_code: Option<String>,
    /// The merchant's UPI handle (`upi_vpa`).
    pub merchant_vpa: Option<String>,
    /// Where the merchant lands once the gateway is linked.
    pub redirect_url: Option<String>,
}

/// A new OAuth link's request: optional.
#[derive(Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = PaymentOauthLinkRequest)]
pub struct OauthLinkRequest {
    /// Where the merchant lands once the gateway is linked.
    pub redirect_url: Option<String>,
}

/// An amount's `value` or `offset`: a string of digits, as the refund's
/// page writes it (`"100"`), or a JSON number.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum RefundAmountPart {
    /// A JSON number.
    Number(u64),
    /// A string of digits, as Meta's page writes it.
    Text(String),
}

/// A refund's amount.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RefundAmount {
    /// `INR`.
    pub currency: String,
    /// A positive integer: the amount times `offset`.
    pub value: RefundAmountPart,
    /// `100` for INR.
    pub offset: RefundAmountPart,
}

/// A refund to ask for, as Meta's page writes it
/// (`payments/payments-in/pg`, "Step 8: Refunds").
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = PaymentRefundRequest)]
pub struct RefundBody {
    /// The order's `reference_id`.
    pub reference_id: String,
    /// `instant` or `normal`; the gateway decides.
    pub speed: Option<String>,
    /// The configuration the order was paid through, by name (it must be
    /// one of the number's WABA's).
    pub payment_config_id: String,
    /// How much to refund.
    pub amount: RefundAmount,
}

// ─── To and from the core's types ────────────────────────────────────────

fn time(at: Option<OffsetDateTime>) -> Option<String> {
    at.map(rfc3339)
}

fn code(code: Option<CodeDescription>) -> Option<PaymentCode> {
    code.map(|c| PaymentCode {
        code: c.code,
        description: c.description,
    })
}

impl From<PaymentConfiguration> for PaymentConfigurationView {
    fn from(c: PaymentConfiguration) -> Self {
        Self {
            configuration_name: c.configuration_name,
            merchant_category_code: code(c.merchant_category_code),
            purpose_code: code(c.purpose_code),
            status: c.status.as_str().to_owned(),
            provider_mid: c.provider_mid,
            provider_name: c.provider_name.map(|p| p.as_str().to_owned()),
            merchant_vpa: c.merchant_vpa,
            created_timestamp: time(c.created_timestamp),
            updated_timestamp: time(c.updated_timestamp),
        }
    }
}

impl From<PaymentOauthLink> for PaymentOauthLinkView {
    fn from(link: PaymentOauthLink) -> Self {
        Self {
            oauth_url: link.oauth_url.map(|url| url.expose_secret().to_owned()),
            expiration: time(link.expiration),
        }
    }
}

impl From<meta_whatsapp_rs::client::payments::Amount> for PaymentAmount {
    fn from(amount: meta_whatsapp_rs::client::payments::Amount) -> Self {
        Self {
            value: amount.value,
            offset: amount.offset,
        }
    }
}

impl From<PaymentRefund> for PaymentRefundView {
    fn from(r: PaymentRefund) -> Self {
        Self {
            id: r.id,
            amount: r.amount.into(),
            speed_processed: r.speed_processed.as_str().to_owned(),
            status: r.status.as_str().to_owned(),
            created_timestamp: rfc3339(r.created_timestamp),
            updated_timestamp: rfc3339(r.updated_timestamp),
        }
    }
}

impl From<Transaction> for PaymentTransaction {
    fn from(t: Transaction) -> Self {
        Self {
            id: t.id,
            pg_transaction_id: t.pg_transaction_id,
            kind: t.gateway.as_str().to_owned(),
            status: t.status.as_str().to_owned(),
            created_timestamp: rfc3339(t.created_timestamp),
            updated_timestamp: rfc3339(t.updated_timestamp),
            amount: t.amount.map(Into::into),
            order_amount: t.order_amount.map(Into::into),
            currency: t.currency,
            method: t.method.map(|m| PaymentMethodView {
                kind: m.kind.as_str().to_owned(),
            }),
            error: t.error.map(|e| PaymentTransactionError {
                code: e.code,
                reason: e.reason,
            }),
            refunds: t.refunds.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<Payment> for PaymentView {
    fn from(p: Payment) -> Self {
        let mut fields = BTreeMap::new();
        let billdesk = &p.billdesk;
        let payu = &p.payu;
        let zaakpay = &p.zaakpay;
        for (name, value) in [
            ("additional_info1", &billdesk.additional_info1),
            ("additional_info2", &billdesk.additional_info2),
            ("additional_info3", &billdesk.additional_info3),
            ("additional_info4", &billdesk.additional_info4),
            ("additional_info5", &billdesk.additional_info5),
            ("additional_info6", &billdesk.additional_info6),
            ("additional_info7", &billdesk.additional_info7),
            ("udf1", &payu.udf1),
            ("udf2", &payu.udf2),
            ("udf3", &payu.udf3),
            ("udf4", &payu.udf4),
            ("extra1", &zaakpay.extra1),
            ("extra2", &zaakpay.extra2),
        ] {
            if let Some(value) = value {
                fields.insert(name.to_owned(), value.clone());
            }
        }
        Self {
            reference_id: p.reference_id,
            status: p.status.as_str().to_owned(),
            currency: p.currency,
            amount: p.amount.into(),
            transactions: p.transactions.into_iter().map(Into::into).collect(),
            receipt: p.razorpay.receipt,
            notes: p.razorpay.notes,
            fields,
        }
    }
}

impl From<RefundCreated> for RefundCreatedView {
    fn from(r: RefundCreated) -> Self {
        Self {
            id: r.id,
            status: r.status.as_str().to_owned(),
            speed_processed: r.speed_processed.as_str().to_owned(),
        }
    }
}

impl From<CreatePaymentConfigurationBody> for domain::NewPaymentConfiguration {
    fn from(body: CreatePaymentConfigurationBody) -> Self {
        let CreatePaymentConfigurationBody {
            configuration_name,
            provider_name,
            merchant_category_code,
            purpose_code,
            merchant_vpa,
            redirect_url,
        } = body;
        Self {
            configuration_name,
            provider_name,
            merchant_category_code,
            purpose_code,
            merchant_vpa,
            redirect_url,
        }
    }
}

impl From<RefundAmountPart> for domain::AmountPart {
    fn from(part: RefundAmountPart) -> Self {
        match part {
            RefundAmountPart::Number(n) => Self::Number(n),
            RefundAmountPart::Text(s) => Self::Text(s),
        }
    }
}

impl From<RefundBody> for domain::RefundOrder {
    fn from(body: RefundBody) -> Self {
        let RefundBody {
            reference_id,
            speed,
            payment_config_id,
            amount:
                RefundAmount {
                    currency,
                    value,
                    offset,
                },
        } = body;
        Self {
            reference_id,
            speed,
            payment_config_id,
            amount: domain::RefundAmount {
                currency,
                value: value.into(),
                offset: offset.into(),
            },
        }
    }
}

/// The core's configuration operations on `owned`, for `caller`, counted
/// in `state`'s metrics (both the state's own: the extractors made them
/// with `state.authz()`).
fn configurations<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedWaba,
) -> Result<PaymentConfigurations<'a>, ApiError> {
    Ok(
        PaymentConfigurations::new(state.authz(), caller, owned.capability(state))?
            .observed_by(state.metrics()),
    )
}

/// The core's payment operations on `owned`, for `caller`.
fn payments<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedNumber,
) -> Result<Payments<'a>, ApiError> {
    Ok(Payments::new(state.authz(), caller, owned.capability(state))?.observed_by(state.metrics()))
}

/// `answer` with `Cache-Control: no-store`: an OAuth link is a credential,
/// which no cache on the way may keep (its replay included).
fn no_store(mut answer: Response) -> Response {
    answer
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    answer
}

/// The answer of a creation or a new link: the link to the caller, and
/// the same answer without it to keep for a repeat.
fn link_answer(status: StatusCode, link: PaymentOauthLink) -> Result<Success, ApiError> {
    let answered = PaymentOauthLinkView::from(link);
    let kept = PaymentOauthLinkView {
        oauth_url: None,
        expiration: answered.expiration.clone(),
    };
    Success::json_keeping(status, &answered, &kept)
}

// ─── Configurations ──────────────────────────────────────────────────────

/// `GET /v1/wabas/{waba_id}/payment-configurations`: the WABA's payment
/// configurations.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/payment-configurations",
    tag = "payments",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "Every payment configuration of the WABA", body = PaymentConfigurationList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`permission`: the WhatsApp Payments terms of service not accepted yet, their link in `details`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_payment_configurations(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
) -> Result<Json<PaymentConfigurationList>, ApiError> {
    let listed = configurations(&state, &caller, &owned)?.list().await?;
    Ok(Json(PaymentConfigurationList {
        data: listed.into_iter().map(Into::into).collect(),
    }))
}

/// Create the configuration in the WABA: `201` with its OAuth link.
async fn create(
    configurations: &PaymentConfigurations<'_>,
    configuration: &meta_whatsapp_rs::client::payments::CreatePaymentConfiguration,
) -> Result<Success, ApiError> {
    let link = configurations.create(configuration).await?;
    link_answer(StatusCode::CREATED, link)
}

/// `POST /v1/wabas/{waba_id}/payment-configurations`: create a payment
/// configuration, checked first.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/payment-configurations",
    tag = "payments",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never create twice (a repeat's answer has no `oauth_url`)"),
    ),
    request_body = CreatePaymentConfigurationBody,
    responses(
        (status = 201, description = "Created: a gateway's carries the OAuth link the merchant opens to link their account (a credential: to the merchant only); `upi_vpa`'s has none", body = PaymentOauthLinkView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`permission`: the terms of service, their link in `details`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (with `field`: `configuration_name`, `provider_name`, …), Meta's `invalid_parameter`, `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the configuration may have been created", body = ErrorBody),
    )
)]
pub(crate) async fn create_payment_configuration(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_payment_configuration(owned.waba_id(), &body);
    let configuration = serde_json::from_value::<CreatePaymentConfigurationBody>(body)
        .map_err(|_| ApiError::invalid("body"))
        .and_then(|body| Ok(domain::new_configuration(body.into())?));
    let configuration = match configuration {
        Ok(configuration) => configuration,
        Err(error) => return error.into_response(),
    };
    let configurations = match configurations(&state, &caller, &owned) {
        Ok(configurations) => configurations,
        Err(error) => return error.into_response(),
    };
    let answer = state
        .idempotent(
            &caller,
            key,
            fingerprint,
            create(&configurations, &configuration),
        )
        .await;
    no_store(answer)
}

/// `GET /v1/wabas/{waba_id}/payment-configurations/{configuration_name}`:
/// one configuration.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/payment-configurations/{configuration_name}",
    tag = "payments",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("configuration_name" = String, Path, description = "The configuration's name, percent-encoded: one path segment (no `/` or `\\`)"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The configuration", body = PaymentConfigurationView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no configuration of that name in it", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `configuration_name`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_payment_configuration(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, name)): Path<(String, String)>,
) -> Result<Json<PaymentConfigurationView>, ApiError> {
    let configuration = configurations(&state, &caller, &owned)?.get(&name).await?;
    Ok(Json(configuration.into()))
}

/// `DELETE /v1/wabas/{waba_id}/payment-configurations/{configuration_name}`:
/// remove a configuration.
#[utoipa::path(
    delete,
    path = "/v1/wabas/{waba_id}/payment-configurations/{configuration_name}",
    tag = "payments",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("configuration_name" = String, Path, description = "The configuration's name, percent-encoded: one path segment (no `/` or `\\`)"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Removed: send no more orders naming it (their buyers could not pay)"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `configuration_name`, or Meta's `invalid_parameter`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn delete_payment_configuration(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, name)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    configurations(&state, &caller, &owned)?
        .delete(&name)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A new OAuth link: `200` with it.
async fn regenerate(
    configurations: &PaymentConfigurations<'_>,
    name: &str,
    request: &OauthLinkRequest,
) -> Result<Success, ApiError> {
    let link = configurations
        .regenerate_oauth_link(name, request.redirect_url.as_deref())
        .await?;
    link_answer(StatusCode::OK, link)
}

/// `POST /v1/wabas/{waba_id}/payment-configurations/{configuration_name}/oauth-link`:
/// a new OAuth link for a gateway configuration.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/payment-configurations/{configuration_name}/oauth-link",
    tag = "payments",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("configuration_name" = String, Path, description = "The configuration's name, percent-encoded: one path segment (no `/` or `\\`)"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never ask twice (a repeat's answer has no `oauth_url`)"),
    ),
    request_body(content = Option<OauthLinkRequest>, description = "Optional; `redirect_url` is where the merchant lands once linked"),
    responses(
        (status = 200, description = "A new link (a credential: to the merchant only)", body = PaymentOauthLinkView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (`configuration_name`, `redirect_url`, `body`), Meta's `invalid_parameter`, `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: a link may have been issued", body = ErrorBody),
    )
)]
pub(crate) async fn regenerate_payment_oauth_link(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    KeyHeader(key): KeyHeader,
    Path((_, name)): Path<(String, String)>,
    OptionalJson(request): OptionalJson<OauthLinkRequest>,
) -> Response {
    // The canonical input: the WABA, the name decoded, and the request as
    // read (an empty body and `{}` are one request).
    let body = serde_json::to_value(&request).unwrap_or(Value::Null);
    let fingerprint = Fingerprint::regenerate_payment_oauth_link(owned.waba_id(), &name, &body);
    let configurations = match configurations(&state, &caller, &owned) {
        Ok(configurations) => configurations,
        Err(error) => return error.into_response(),
    };
    let answer = state
        .idempotent(
            &caller,
            key,
            fingerprint,
            regenerate(&configurations, &name, &request),
        )
        .await;
    no_store(answer)
}

// ─── Lookup and refunds ──────────────────────────────────────────────────

/// `GET /v1/numbers/{pn}/payments/{configuration_name}/{reference_id}`:
/// the payment of an order sent through a payment gateway.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/payments/{configuration_name}/{reference_id}",
    tag = "payments",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "The phone number id that sent the `order_details` message"),
        ("configuration_name" = String, Path, description = "The order's payment configuration, percent-encoded: one of the number's WABA's"),
        ("reference_id" = String, Path, description = "The order's `reference_id`"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The order's payment, its attempts and refunds: read it here rather than trust the webhook alone", body = PaymentList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no configuration of that name in its WABA (the payment is not asked for)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `configuration_name` or `reference_id`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed (its lookup errors are not Graph errors: `upstream`)", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_payment(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    Path((_, name, reference)): Path<(String, String, String)>,
) -> Result<Json<PaymentList>, ApiError> {
    let found = payments(&state, &caller, &owned)?
        .lookup(&name, &reference)
        .await?;
    Ok(Json(PaymentList {
        data: found.into_iter().map(Into::into).collect(),
    }))
}

/// Refund: `201` with Meta's answer.
async fn refund(
    payments: &Payments<'_>,
    request: &meta_whatsapp_rs::client::payments::RefundRequest,
) -> Result<Success, ApiError> {
    let created = payments.refund(request).await?;
    Success::json(StatusCode::CREATED, &RefundCreatedView::from(created))
}

/// `POST /v1/numbers/{pn}/payments/refunds`: refund an order paid through a
/// payment gateway, in full or in part.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/payments/refunds",
    tag = "payments",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "The phone number id that sent the `order_details` message"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = String, Header, description = "Required: 1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never refund twice (repeat a refund under the same key, never a new one)"),
    ),
    request_body = RefundBody,
    responses(
        (status = 201, description = "Asked of the gateway; its status may be `pending`", body = RefundCreatedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or `payment_config_id` names no configuration of its WABA (nothing refunded)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` (with `field`: `reference_id`, `amount.value`, `amount.offset`, …), Meta's `invalid_parameter`, `idempotency_key_reused`: nothing refunded", body = ErrorBody),
        (status = 428, description = "`idempotency_key_required`: no `Idempotency-Key`, nothing refunded", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the refund may have been made (`may_have_been_sent: true`): look the payment up, or repeat with the same key", body = ErrorBody),
    )
)]
pub(crate) async fn refund_payment(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    // Money moves only under a key (D56): refused before the body is checked.
    let key = match domain::refund_key(key) {
        Ok(key) => key,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let fingerprint = Fingerprint::refund_payment(owned.phone_number_id(), &body);
    let request = serde_json::from_value::<RefundBody>(body)
        .map_err(|_| ApiError::invalid("body"))
        .and_then(|body| Ok(domain::refund_request(body.into())?));
    let request = match request {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let payments = match payments(&state, &caller, &owned) {
        Ok(payments) => payments,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, Some(key), fingerprint, refund(&payments, &request))
        .await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// `payments-in/onboarding-apis`' configuration and `payments-in/pg`'s
    /// payment, as the library reads them, are answered with every field,
    /// times in RFC 3339; the link's `Debug` hides it. Decisive: each
    /// field of the conversions.
    #[test]
    fn every_field_of_metas_answers_is_answered() {
        let configuration: PaymentConfiguration = serde_json::from_value(json!({
            "configuration_name": "test-payment-configuration",
            "merchant_category_code": {"code": "0000", "description": "Test MCC Code"},
            "purpose_code": {"code": "00", "description": "Test Purpose Code"},
            "status": "Active",
            "provider_mid": "test-payment-gateway-mid",
            "provider_name": "RazorPay",
            "created_timestamp": 1_720_203_204,
            "updated_timestamp": 1_721_088_316
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(PaymentConfigurationView::from(configuration)).unwrap(),
            json!({
                "configuration_name": "test-payment-configuration",
                "merchant_category_code": {"code": "0000", "description": "Test MCC Code"},
                "purpose_code": {"code": "00", "description": "Test Purpose Code"},
                "status": "Active",
                "provider_mid": "test-payment-gateway-mid",
                "provider_name": "razorpay",
                "merchant_vpa": null,
                "created_timestamp": "2024-07-05T18:13:24Z",
                "updated_timestamp": "2024-07-16T00:05:16Z"
            })
        );
        let payment: Payment = serde_json::from_value(json!({
            "reference_id": "BM345A-12",
            "status": "captured",
            "currency": "INR",
            "amount": {"value": 21000, "offset": 100},
            "transactions": [{
                "id": "order_PG1",
                "pg_transaction_id": "pay_PG1",
                "type": "razorpay",
                "status": "success",
                "created_timestamp": 1_720_203_204,
                "updated_timestamp": 1_720_203_300,
                "method": {"type": "upi"},
                "refunds": [{
                    "id": "rfnd_1", "amount": {"value": 100, "offset": 100},
                    "speed_processed": "instant", "status": "success",
                    "created_timestamp": 1_720_203_400, "updated_timestamp": 1_720_203_500
                }]
            }],
            "receipt": "receipt-value",
            "notes": {"key1": "value1"},
            "udf1": "udf1-value",
            "additional_info7": "additional_info7-value",
            "extra2": "extra2-value"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(PaymentView::from(payment)).unwrap(),
            json!({
                "reference_id": "BM345A-12",
                "status": "captured",
                "currency": "INR",
                "amount": {"value": 21000, "offset": 100},
                "transactions": [{
                    "id": "order_PG1",
                    "pg_transaction_id": "pay_PG1",
                    "type": "razorpay",
                    "status": "success",
                    "created_timestamp": "2024-07-05T18:13:24Z",
                    "updated_timestamp": "2024-07-05T18:15:00Z",
                    "amount": null,
                    "order_amount": null,
                    "currency": null,
                    "method": {"type": "upi"},
                    "error": null,
                    "refunds": [{
                        "id": "rfnd_1", "amount": {"value": 100, "offset": 100},
                        "speed_processed": "instant", "status": "success",
                        "created_timestamp": "2024-07-05T18:16:40Z",
                        "updated_timestamp": "2024-07-05T18:18:20Z"
                    }]
                }],
                "receipt": "receipt-value",
                "notes": {"key1": "value1"},
                "udf1": "udf1-value",
                "additional_info7": "additional_info7-value",
                "extra2": "extra2-value"
            })
        );
        let link: PaymentOauthLink = serde_json::from_value(json!({
            "oauth_url": "https://www.facebook.com/payment/onboarding/init/?secret=LINK-SECRET",
            "expiration": 1_721_687_287
        }))
        .unwrap();
        let view = PaymentOauthLinkView::from(link);
        assert!(!format!("{view:?}").contains("LINK-SECRET"));
        assert_eq!(
            serde_json::to_value(&view).unwrap(),
            json!({
                "oauth_url": "https://www.facebook.com/payment/onboarding/init/?secret=LINK-SECRET",
                "expiration": "2024-07-22T22:28:07Z"
            })
        );
    }
}
