//! Partner APIs (docs/design/server.md, section 4.2; roadmap M5j): what a
//! Tech Provider or Solution Partner does beyond onboarding, and what a
//! tenant does to its own WABA with a partner.
//!
//! **The operator's routes** (admin key; the deployment's partner
//! credentials, `WA_PARTNER_BUSINESS_ID` and the system token, as the pool
//! of pre-verified numbers uses, else `409 signup_not_configured`):
//!
//! | Route | Graph call |
//! | --- | --- |
//! | `POST`, `GET /v1/admin/partners/business-verifications` | `POST /{business}/self_certify_whatsapp_business` (**legal act**, takes an `Idempotency-Key` scoped to the admin key: roadmap M5j2), `GET /{business}/self_certified_whatsapp_business_submissions` |
//! | `POST`, `GET /v1/admin/partners/solutions` | `POST /{app}/whatsapp_business_solution` (takes an `Idempotency-Key`, as above), `GET /{app}/whatsapp_business_solutions` |
//! | `GET /v1/admin/partners/solutions/{id}`, `…/partners` | `GET /{solution}`, `GET /{solution}/partners` |
//! | `GET /v1/admin/partners/client-businesses` | `GET /{app}/connected_client_businesses` |
//! | `POST /v1/admin/partners/solutions/{id}/accept`, `…/reject` | `POST /{solution}/accept` (**legal act**), `…/reject` (**legal act**) |
//! | `POST /v1/admin/partners/solutions/{id}/deactivation-request`, `…/accept`, `…/reject` | `POST /{solution}/send_deactivation_request`, `…/accept_deactivation_request` (**legal act**), `…/reject_deactivation_request` |
//! | `GET /v1/admin/partners/users/{user_id}/wabas` | `GET /{user}/assigned_whatsapp_business_accounts` |
//!
//! **The tenant's routes** (scope `partners`, a WABA of its own, its
//! token):
//!
//! | Route | Graph call |
//! | --- | --- |
//! | `GET /v1/wabas/{waba_id}/business-verification` | `GET /{waba}?fields=business_verification_status` |
//! | `POST /v1/wabas/{waba_id}/solution-migration-intents` | `POST /{waba}/set_solution_migration_intent` (**legal act**) |
//! | `GET /v1/wabas/{waba_id}/solution-migration-intents/{intent_id}` | `GET /{intent}`, answered only if it names the WABA |
//! | `POST /v1/wabas/{waba_id}/billing-migrations` | `POST /{waba}/set_payment_method_migration_intent` (**legal act**) |
//! | `GET /v1/wabas/{waba_id}/billing-migrations/{migration_id}` | `GET /{migration}`, for a migration the WABA started |
//! | `POST /v1/wabas/{waba_id}/billing-migrations/{migration_id}/resume` | `POST /{migration}/resume_migration` (**legal act**) |
//!
//! A **legal act** is taken only when the deployment sets
//! `WA_SERVER_LEGAL_ACTS=on` (off: `403 feature_not_available`, nothing
//! asked of Meta); every one is audited. The solution token is never
//! served, and documents and Meta's texts are in no answer, log or audit
//! event (the core's [`meta_whatsapp_server_core::partners`]).

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{FromRequestParts, Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::http::request::Parts;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::partners::{
    AssignedWabaView, BillingMigrationView, BusinessVerificationView, ClientBusinessView,
    DocumentInput, MigrationIntentView, NewSolution, Paged, Partners, SolutionPartnerView,
    SolutionQuery, SolutionView, SubmissionQuery, SubmissionView, VerificationReceipt,
    VerificationRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::ApiJson;
use super::templates::ApiQuery;
use crate::auth::{AdminCaller, Authorizer, Caller, OwnedWaba};
use crate::error::{ApiError, ErrorBody, ErrorCode};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// The largest body of a business verification submission: three documents
/// of 5 MiB, in base64, with their names.
pub const MAX_VERIFICATION_BODY_BYTES: usize = 21 * 1024 * 1024;

/// The legal-act gate as an extractor, placed **before** the body's: with
/// `WA_SERVER_LEGAL_ACTS` off, an act that carries a body is `403
/// feature_not_available` whatever the body is (empty, not JSON, a field
/// missing or unknown), never `422` from parsing a body the deployment
/// will not act on (M5h's review found that order wrong). The services
/// gate again, as the one place that says which acts are legal acts
/// (`is_legal_admin_act`, `is_legal_act`).
pub(crate) struct LegalActsOn;

impl FromRequestParts<Grant> for LegalActsOn {
    type Rejection = ApiError;

    fn from_request_parts(
        _: &mut Parts,
        state: &Grant,
    ) -> impl Future<Output = Result<Self, ApiError>> + Send {
        std::future::ready(if state.settings().legal_acts {
            Ok(Self)
        } else {
            Err(ApiError::new(ErrorCode::FEATURE_NOT_AVAILABLE))
        })
    }
}

// ─── The views ───────────────────────────────────────────────────────────

/// A page of a partner read: `next_cursor` is Meta's, opaque.
macro_rules! page_view {
    ($name:ident, $schema:ident, $item:ty, $what:literal) => {
        #[doc = $what]
        #[derive(Debug, Serialize, ToSchema)]
        #[schema(as = $schema)]
        pub struct $name {
            /// The items.
            pub data: Vec<$item>,
            /// Pass as `cursor` for the next page; `null` on the last one.
            #[schema(required = true)]
            pub next_cursor: Option<String>,
        }
    };
}

fn page<T, U>(page: Paged<T>, convert: impl FnMut(T) -> U) -> (Vec<U>, Option<String>) {
    (
        page.data.into_iter().map(convert).collect(),
        page.next_cursor,
    )
}

/// A submission of business documents for partner-led verification.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BusinessVerificationSubmission)]
pub struct SubmissionBody {
    /// The submission's id.
    pub id: String,
    /// `APPROVED`, `DISCARDED`, `FAILED`, `PENDING`, `REVOKED`, or another
    /// value as Meta sent it.
    #[schema(required = true)]
    pub status: Option<String>,
    /// Why it was rejected, as Meta's `rejection_reasons`.
    pub rejection_reasons: Vec<String>,
    /// When it was submitted, as Meta sent it.
    #[schema(required = true)]
    pub submitted_time: Option<String>,
    /// When it last changed, as Meta sent it.
    #[schema(required = true)]
    pub update_time: Option<String>,
    /// The customer's business portfolio.
    #[schema(required = true)]
    pub client_business_id: Option<String>,
}

impl From<SubmissionView> for SubmissionBody {
    fn from(view: SubmissionView) -> Self {
        Self {
            id: view.id,
            status: view.status,
            rejection_reasons: view.rejection_reasons,
            submitted_time: view.submitted_time,
            update_time: view.update_time,
            client_business_id: view.client_business_id,
        }
    }
}

page_view!(
    SubmissionList,
    BusinessVerificationSubmissionList,
    SubmissionBody,
    "A page of business verification submissions."
);

/// What Meta counted for a submission.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BusinessVerificationReceipt)]
pub struct ReceiptBody {
    /// Submissions made for this customer, this one included.
    #[schema(required = true)]
    pub verification_attempts: Option<u32>,
    /// What is left of the three (after three rejections the customer
    /// verifies on their own).
    #[schema(required = true)]
    pub attempts_left: Option<u32>,
}

impl From<VerificationReceipt> for ReceiptBody {
    fn from(receipt: VerificationReceipt) -> Self {
        Self {
            verification_attempts: receipt.verification_attempts,
            attempts_left: receipt.attempts_left,
        }
    }
}

/// A partner of a solution.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = SolutionPartner)]
pub struct SolutionPartnerBody {
    /// The partner record's id (not an app id).
    #[schema(required = true)]
    pub id: Option<String>,
    /// The partner's app.
    #[schema(required = true)]
    pub partner_app_id: Option<String>,
    /// The partner's app name.
    #[schema(required = true)]
    pub partner_app_name: Option<String>,
    /// `NOTIFICATION_SENT` or `ACCEPTED`, as Meta sent it.
    #[schema(required = true)]
    pub status: Option<String>,
    /// What clients grant the partner's app (`MESSAGING`, …).
    #[schema(required = true)]
    pub partner_permissions: Option<Vec<String>>,
}

impl From<SolutionPartnerView> for SolutionPartnerBody {
    fn from(view: SolutionPartnerView) -> Self {
        Self {
            id: view.id,
            partner_app_id: view.partner_app_id,
            partner_app_name: view.partner_app_name,
            status: view.status,
            partner_permissions: view.partner_permissions,
        }
    }
}

page_view!(
    SolutionPartnerList,
    SolutionPartnerList,
    SolutionPartnerBody,
    "A page of a solution's partners."
);

/// A Multi-Partner Solution.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Solution)]
pub struct SolutionBody {
    /// The solution's id.
    pub id: String,
    /// Its name.
    #[schema(required = true)]
    pub name: Option<String>,
    /// `INITIATED`, `ACTIVE`, `DEACTIVATED`, … as Meta sent it.
    #[schema(required = true)]
    pub status: Option<String>,
    /// The transition waiting for the other partner, if any.
    #[schema(required = true)]
    pub status_for_pending_request: Option<String>,
    /// The owner's app (a single solution's read only).
    #[schema(required = true)]
    pub owner_app_id: Option<String>,
    /// What clients grant the owner's app (a single solution's read only).
    #[schema(required = true)]
    pub owner_permissions: Option<Vec<String>>,
    /// The partners (a single solution's read only).
    #[schema(required = true)]
    pub partners: Option<Vec<SolutionPartnerBody>>,
}

impl From<SolutionView> for SolutionBody {
    fn from(view: SolutionView) -> Self {
        Self {
            id: view.id,
            name: view.name,
            status: view.status,
            status_for_pending_request: view.status_for_pending_request,
            owner_app_id: view.owner_app_id,
            owner_permissions: view.owner_permissions,
            partners: view
                .partners
                .map(|partners| partners.into_iter().map(Into::into).collect()),
        }
    }
}

page_view!(
    SolutionList,
    SolutionList,
    SolutionBody,
    "A page of the deployment's app's solutions."
);

/// A business onboarded through the deployment's app.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = ClientBusiness)]
pub struct ClientBusinessBody {
    /// Its business portfolio id.
    pub id: String,
    /// Its name.
    #[schema(required = true)]
    pub name: Option<String>,
}

page_view!(
    ClientBusinessList,
    ClientBusinessList,
    ClientBusinessBody,
    "A page of the businesses onboarded through the deployment's app."
);

/// A WABA assigned to a user.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = AssignedWaba)]
pub struct AssignedWabaBody {
    /// The WABA's id.
    pub id: String,
    /// Its name.
    #[schema(required = true)]
    pub name: Option<String>,
}

page_view!(
    AssignedWabaList,
    AssignedWabaList,
    AssignedWabaBody,
    "A page of the WABAs assigned to a user."
);

/// A solution just created.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CreatedSolution)]
pub struct CreatedSolutionBody {
    /// The new solution, `INITIATED` until the partner accepts it.
    pub solution_id: String,
}

/// A WABA's business verification status.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BusinessVerification)]
pub struct BusinessVerificationBody {
    /// Meta's `business_verification_status`, as sent (`verified`,
    /// `pending`, …); `null` when Meta did not say.
    #[schema(required = true)]
    pub status: Option<String>,
    /// Whether it is `verified`.
    pub verified: bool,
}

impl From<BusinessVerificationView> for BusinessVerificationBody {
    fn from(view: BusinessVerificationView) -> Self {
        Self {
            status: view.status,
            verified: view.verified,
        }
    }
}

/// A migration intent of the WABA.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = SolutionMigrationIntent)]
pub struct MigrationIntentBody {
    /// The intent's id.
    pub id: String,
    /// `INITIATED`, `ACCEPTED`, `REJECTED`, … as Meta sent it. The client
    /// accepts in Meta Business Suite.
    #[schema(required = true)]
    pub status: Option<String>,
    /// The destination solution.
    #[schema(required = true)]
    pub solution_id: Option<String>,
    /// The destination solution's name.
    #[schema(required = true)]
    pub solution_name: Option<String>,
    /// The WABA the migration lands on.
    #[schema(required = true)]
    pub destination_waba_id: Option<String>,
    /// The source WABA.
    #[schema(required = true)]
    pub waba_id: Option<String>,
}

impl From<MigrationIntentView> for MigrationIntentBody {
    fn from(view: MigrationIntentView) -> Self {
        Self {
            id: view.id,
            status: view.status,
            solution_id: view.solution_id,
            solution_name: view.solution_name,
            destination_waba_id: view.destination_waba_id,
            waba_id: view.waba_id,
        }
    }
}

/// A solution migration intent just created.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = CreatedSolutionMigrationIntent)]
pub struct CreatedIntentBody {
    /// The intent's id: send it to the destination solution's partner.
    pub id: String,
}

/// A billing currency or payment method migration.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BillingMigration)]
pub struct BillingMigrationBody {
    /// The migration's id.
    pub id: String,
    /// `INITIATED`, `ACCEPTED`, `IN_PROGRESS`, `READY_TO_COMPLETE`,
    /// `COMPLETED`, `FAILED`, `FAILED_TO_COMPLETE`, `REJECTED`, or another
    /// value as Meta sent it.
    pub status: String,
    /// The clone Meta created, once it said.
    #[schema(required = true)]
    pub destination_waba_id: Option<String>,
    /// The clone's currency, once Meta said.
    #[schema(required = true)]
    pub destination_currency: Option<String>,
}

impl From<BillingMigrationView> for BillingMigrationBody {
    fn from(view: BillingMigrationView) -> Self {
        Self {
            id: view.id,
            status: view.status,
            destination_waba_id: view.destination_waba_id,
            destination_currency: view.destination_currency,
        }
    }
}

// ─── The requests ────────────────────────────────────────────────────────

/// One document of a submission.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = BusinessVerificationDocument)]
pub struct DocumentBody {
    /// The file name; its extension is its type: `.pdf`, `.jpeg`, `.jpg`
    /// or `.png`.
    pub file_name: String,
    /// The content in base64 (standard alphabet), at most 5 MiB decoded,
    /// and a file of the type its name says.
    pub content_base64: String,
}

/// `POST /v1/admin/partners/business-verifications`. No `Debug`: it holds
/// the documents.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = SubmitBusinessVerification)]
pub struct SubmitBody {
    /// The customer's business portfolio, onboarded through the
    /// deployment's app.
    pub client_business_id: String,
    /// One to three documents.
    pub documents: Vec<DocumentBody>,
}

/// `?client_business_id=` and `?cursor=`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct SubmissionsQuery {
    /// Only this customer's submissions.
    pub client_business_id: Option<String>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// `POST /v1/admin/partners/solutions`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CreateSolution)]
pub struct CreateSolutionBody {
    /// The solution's name, 2 to 75 characters, as both partners see it in
    /// the App Dashboard.
    pub name: String,
    /// The invited partner's app.
    pub partner_app_id: String,
    /// What clients grant the deployment's app: `MESSAGING` or nothing
    /// (Meta's reference allows no other).
    #[serde(default)]
    pub owner_permissions: Vec<String>,
    /// What clients grant the partner's app: Meta's task names
    /// (`MESSAGING`, `MANAGE_TEMPLATES`, …). `MESSAGING` must be granted
    /// to one of the two.
    #[serde(default)]
    pub partner_permissions: Vec<String>,
}

/// `?role=`, `?limit=` and `?cursor=`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct SolutionsQuery {
    /// `OWNER` or `PARTNER`: only the solutions the app owns, or is a
    /// partner of.
    pub role: Option<String>,
    /// Page size, 1 to 100.
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// `?limit=` and `?cursor=`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct PartnersPageQuery {
    /// Page size, 1 to 100.
    #[param(minimum = 1, maximum = 100)]
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// `?cursor=`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CursorQuery {
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// `POST /v1/wabas/{waba_id}/solution-migration-intents`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CreateSolutionMigrationIntent)]
pub struct CreateIntentBody {
    /// The destination solution (active), whose partner will read the
    /// intent once the client accepted it.
    pub solution_id: String,
}

/// `POST /v1/wabas/{waba_id}/billing-migrations`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CreateBillingMigration)]
pub struct CreateBillingMigrationBody {
    /// The new WABA's billing currency, an ISO 4217 code (`INR`).
    pub currency: String,
    /// The Line of Credit to attach to the new WABA; only when moving to
    /// Line of Credit billing.
    pub extended_credit_id: Option<String>,
}

// ─── The operator's routes ───────────────────────────────────────────────

/// The verification submission as the operation a key keeps: the receipt,
/// never the request.
async fn submit_verification(
    service: &Partners<'_>,
    authz: &Authorizer,
    request: VerificationRequest,
) -> Result<Success, ApiError> {
    let receipt = service.submit_business_verification(authz, request).await?;
    Success::json(StatusCode::CREATED, &ReceiptBody::from(receipt))
}

/// `POST /v1/admin/partners/business-verifications`: submit a customer
/// business for partner-led verification, with its documents. **A legal
/// act** (a statement of the business's legal identity): off until the
/// deployment sets `WA_SERVER_LEGAL_ACTS=on`. Each submission counts
/// towards the customer's three, so it is never replayed; it takes an
/// `Idempotency-Key`, scoped to the admin key, so that a client's retry
/// after a `504` is not a second submission.
#[utoipa::path(
    post,
    path = "/v1/admin/partners/business-verifications",
    tag = "partners",
    security(("api_key" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the admin key: the same key and body never submit twice. The answer is kept, never the documents"),
    ),
    request_body = SubmitBody,
    responses(
        (status = 201, description = "Meta received it; audited `business_verification_submitted` (by count: never a document). The outcome comes by the `account_update` webhook", body = ReceiptBody),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`feature_not_available` (the deployment has not enabled the legal acts: nothing was asked of Meta) or `forbidden` (not an admin key)", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`: no partner business (`WA_PARTNER_BUSINESS_ID`); `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 413, description = "`payload_too_large`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `client_business_id`, `documents` (one to three, PDF, JPEG or PNG, 5 MiB each, content of the type its name says), `body` or `Idempotency-Key`: nothing was sent; `idempotency_key_reused`", body = ErrorBody),
        (status = 502, description = "Meta refused or failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: list the submissions before submitting again, or repeat the request with the same `Idempotency-Key` for this answer", body = ErrorBody),
    )
)]
pub(crate) async fn submit_business_verification(
    State(state): State<Grant>,
    admin: AdminCaller,
    _gate: LegalActsOn,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::submit_business_verification(&body);
    let Ok(body) = serde_json::from_value::<SubmitBody>(body) else {
        return ApiError::invalid("body").into_response();
    };
    let service = match state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)
    {
        Ok(service) => service.observed_by(state.metrics()),
        Err(error) => return ApiError::from(error).into_response(),
    };
    let request = VerificationRequest {
        client_business_id: body.client_business_id,
        documents: body
            .documents
            .into_iter()
            .map(|document| DocumentInput {
                file_name: document.file_name,
                content_base64: document.content_base64,
            })
            .collect(),
    };
    state
        .idempotent_admin(
            &admin,
            key,
            fingerprint,
            submit_verification(&service, state.authz(), request),
        )
        .await
}

/// `GET /v1/admin/partners/business-verifications`: one page of the
/// submissions the deployment made.
#[utoipa::path(
    get,
    path = "/v1/admin/partners/business-verifications",
    tag = "partners",
    security(("api_key" = [])),
    params(SubmissionsQuery),
    responses(
        (status = 200, description = "One page of submissions", body = SubmissionList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `client_business_id`, `cursor` or `query`", body = ErrorBody),
        (status = 502, description = "Meta refused or failed", body = ErrorBody),
    )
)]
pub(crate) async fn list_business_verifications(
    State(state): State<Grant>,
    admin: AdminCaller,
    ApiQuery(query): ApiQuery<SubmissionsQuery>,
) -> Result<Json<SubmissionList>, ApiError> {
    let submissions = state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .business_verifications(
            state.authz(),
            SubmissionQuery {
                client_business_id: query.client_business_id,
                after: query.cursor,
            },
        )
        .await?;
    let (data, next_cursor) = page(submissions, SubmissionBody::from);
    Ok(Json(SubmissionList { data, next_cursor }))
}

/// The solution creation as the operation a key keeps.
async fn create_solution(
    service: &Partners<'_>,
    authz: &Authorizer,
    solution: NewSolution,
) -> Result<Success, ApiError> {
    let solution_id = service.create_solution(authz, solution).await?;
    Success::json(StatusCode::CREATED, &CreatedSolutionBody { solution_id })
}

/// `POST /v1/admin/partners/solutions`: create a Multi-Partner Solution
/// owned by the deployment's app, inviting another. Not a legal act (it
/// binds nobody until the partner accepts); audited, never replayed; it
/// takes an `Idempotency-Key`, scoped to the admin key, so that a retry
/// does not invite the partner twice.
#[utoipa::path(
    post,
    path = "/v1/admin/partners/solutions",
    tag = "partners",
    security(("api_key" = [])),
    params(
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the admin key: the same key and body never create twice"),
    ),
    request_body = CreateSolutionBody,
    responses(
        (status = 201, description = "Created `INITIATED`; audited `partner_solution_created`", body = CreatedSolutionBody),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`; `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `name`, `partner_app_id`, `owner_permissions`, `partner_permissions`, `body` or `Idempotency-Key`: nothing was sent; `idempotency_key_reused`", body = ErrorBody),
        (status = 502, description = "Meta refused or failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: list the solutions before creating again, or repeat the request with the same `Idempotency-Key` for this answer", body = ErrorBody),
    )
)]
pub(crate) async fn create_partner_solution(
    State(state): State<Grant>,
    admin: AdminCaller,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_partner_solution(&body);
    let Ok(body) = serde_json::from_value::<CreateSolutionBody>(body) else {
        return ApiError::invalid("body").into_response();
    };
    let service = match state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)
    {
        Ok(service) => service.observed_by(state.metrics()),
        Err(error) => return ApiError::from(error).into_response(),
    };
    let solution = NewSolution {
        name: body.name,
        partner_app_id: body.partner_app_id,
        owner_permissions: body.owner_permissions,
        partner_permissions: body.partner_permissions,
    };
    state
        .idempotent_admin(
            &admin,
            key,
            fingerprint,
            create_solution(&service, state.authz(), solution),
        )
        .await
}

/// `GET /v1/admin/partners/solutions`: one page of the deployment's
/// app's solutions.
#[utoipa::path(
    get,
    path = "/v1/admin/partners/solutions",
    tag = "partners",
    security(("api_key" = [])),
    params(SolutionsQuery),
    responses(
        (status = 200, description = "One page of solutions (id, name and status)", body = SolutionList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `role`, `limit`, `cursor` or `query`", body = ErrorBody),
        (status = 502, description = "Meta refused or failed", body = ErrorBody),
    )
)]
pub(crate) async fn list_partner_solutions(
    State(state): State<Grant>,
    admin: AdminCaller,
    ApiQuery(query): ApiQuery<SolutionsQuery>,
) -> Result<Json<SolutionList>, ApiError> {
    let solutions = state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .solutions(
            state.authz(),
            SolutionQuery {
                role: query.role,
                limit: query.limit,
                after: query.cursor,
            },
        )
        .await?;
    let (data, next_cursor) = page(solutions, SolutionBody::from);
    Ok(Json(SolutionList { data, next_cursor }))
}

/// `GET /v1/admin/partners/solutions/{id}`: a solution with its owner,
/// permissions and partners: what to read before accepting it.
#[utoipa::path(
    get,
    path = "/v1/admin/partners/solutions/{id}",
    tag = "partners",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The solution's id")),
    responses(
        (status = 200, description = "The solution", body = SolutionBody),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`", body = ErrorBody),
        (status = 502, description = "Meta refused or failed", body = ErrorBody),
    )
)]
pub(crate) async fn get_partner_solution(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<Json<SolutionBody>, ApiError> {
    let solution = state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .solution(state.authz(), &id)
        .await?;
    Ok(Json(solution.into()))
}

/// `GET /v1/admin/partners/solutions/{id}/partners`: one page of a
/// solution's partners.
#[utoipa::path(
    get,
    path = "/v1/admin/partners/solutions/{id}/partners",
    tag = "partners",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The solution's id"), CursorQuery),
    responses(
        (status = 200, description = "One page of partners", body = SolutionPartnerList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`, `cursor` or `query`", body = ErrorBody),
        (status = 502, description = "Meta refused or failed", body = ErrorBody),
    )
)]
pub(crate) async fn list_partner_solution_partners(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
    ApiQuery(query): ApiQuery<CursorQuery>,
) -> Result<Json<SolutionPartnerList>, ApiError> {
    let partners = state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .solution_partners(state.authz(), &id, query.cursor)
        .await?;
    let (data, next_cursor) = page(partners, SolutionPartnerBody::from);
    Ok(Json(SolutionPartnerList { data, next_cursor }))
}

/// `GET /v1/admin/partners/client-businesses`: one page of the businesses
/// onboarded through the deployment's app.
#[utoipa::path(
    get,
    path = "/v1/admin/partners/client-businesses",
    tag = "partners",
    security(("api_key" = [])),
    params(PartnersPageQuery),
    responses(
        (status = 200, description = "One page of client businesses", body = ClientBusinessList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit`, `cursor` or `query`", body = ErrorBody),
        (status = 502, description = "Meta refused or failed", body = ErrorBody),
    )
)]
pub(crate) async fn list_client_businesses(
    State(state): State<Grant>,
    admin: AdminCaller,
    ApiQuery(query): ApiQuery<PartnersPageQuery>,
) -> Result<Json<ClientBusinessList>, ApiError> {
    let businesses = state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .client_businesses(state.authz(), query.limit, query.cursor)
        .await?;
    let (data, next_cursor) = page(businesses, |business: ClientBusinessView| {
        ClientBusinessBody {
            id: business.id,
            name: business.name,
        }
    });
    Ok(Json(ClientBusinessList { data, next_cursor }))
}

/// `POST /v1/admin/partners/solutions/{id}/accept`: accept a solution's
/// invitation. **A legal act, and it cannot be undone**: a solution cannot
/// be declined once accepted, so read it first. Off until the deployment
/// sets `WA_SERVER_LEGAL_ACTS=on`; audited; never replayed.
#[utoipa::path(
    post,
    path = "/v1/admin/partners/solutions/{id}/accept",
    tag = "partners",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The solution's id")),
    responses(
        (status = 204, description = "Accepted; audited `partner_solution_accepted`"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`feature_not_available` (the legal acts are off: nothing was asked of Meta) or `forbidden`", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`, or Meta's refusal (not `INITIATED`)", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: read the solution", body = ErrorBody),
    )
)]
pub(crate) async fn accept_partner_solution(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .accept_solution(state.authz(), &id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/admin/partners/solutions/{id}/reject`: decline a solution's
/// invitation. **A legal act, and permanent**: the other partner has to
/// send a new solution. Off until the deployment sets
/// `WA_SERVER_LEGAL_ACTS=on`; audited; never replayed.
#[utoipa::path(
    post,
    path = "/v1/admin/partners/solutions/{id}/reject",
    tag = "partners",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The solution's id")),
    responses(
        (status = 204, description = "Rejected; audited `partner_solution_rejected`"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`feature_not_available` (the legal acts are off: nothing was asked of Meta) or `forbidden`", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`, or Meta's refusal", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: read the solution", body = ErrorBody),
    )
)]
pub(crate) async fn reject_partner_solution(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .reject_solution(state.authz(), &id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/admin/partners/solutions/{id}/deactivation-request`: ask the
/// partner to deactivate a solution. The solution stays usable until the
/// partner accepts. Not a legal act; audited; never replayed.
#[utoipa::path(
    post,
    path = "/v1/admin/partners/solutions/{id}/deactivation-request",
    tag = "partners",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The solution's id")),
    responses(
        (status = 204, description = "Asked; audited `partner_solution_deactivation_requested`"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`, or Meta's refusal", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: read the solution", body = ErrorBody),
    )
)]
pub(crate) async fn request_partner_solution_deactivation(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .request_solution_deactivation(state.authz(), &id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/admin/partners/solutions/{id}/deactivation-request/accept`:
/// accept a pending deactivation request: the solution becomes
/// `DEACTIVATED` and onboards no client. **A legal act**: off until the
/// deployment sets `WA_SERVER_LEGAL_ACTS=on`; audited; never replayed.
#[utoipa::path(
    post,
    path = "/v1/admin/partners/solutions/{id}/deactivation-request/accept",
    tag = "partners",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The solution's id")),
    responses(
        (status = 204, description = "Accepted; audited `partner_solution_deactivation_accepted`"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`feature_not_available` (the legal acts are off: nothing was asked of Meta) or `forbidden`", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`, or Meta's refusal (outstanding payments, no pending request)", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: read the solution", body = ErrorBody),
    )
)]
pub(crate) async fn accept_partner_solution_deactivation(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .accept_solution_deactivation(state.authz(), &id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/admin/partners/solutions/{id}/deactivation-request/reject`:
/// reject a pending deactivation request; the solution stays `ACTIVE`.
/// Not a legal act; audited; never replayed.
#[utoipa::path(
    post,
    path = "/v1/admin/partners/solutions/{id}/deactivation-request/reject",
    tag = "partners",
    security(("api_key" = [])),
    params(("id" = String, Path, description = "The solution's id")),
    responses(
        (status = 204, description = "Rejected; audited `partner_solution_deactivation_rejected`"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `id`, or Meta's refusal", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: read the solution", body = ErrorBody),
    )
)]
pub(crate) async fn reject_partner_solution_deactivation(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .reject_solution_deactivation(state.authz(), &id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/admin/partners/users/{user_id}/wabas`: one page of the WABAs
/// assigned to a system user of the deployment's business.
#[utoipa::path(
    get,
    path = "/v1/admin/partners/users/{user_id}/wabas",
    tag = "partners",
    security(("api_key" = [])),
    params(("user_id" = String, Path, description = "The system user's id (digits)"), PartnersPageQuery),
    responses(
        (status = 200, description = "One page of WABAs (id and name)", body = AssignedWabaList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`: not an admin key", body = ErrorBody),
        (status = 409, description = "`signup_not_configured`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `user_id`, `limit`, `cursor` or `query`", body = ErrorBody),
        (status = 502, description = "Meta refused or failed", body = ErrorBody),
    )
)]
pub(crate) async fn list_assigned_wabas(
    State(state): State<Grant>,
    admin: AdminCaller,
    Path(user_id): Path<String>,
    ApiQuery(query): ApiQuery<PartnersPageQuery>,
) -> Result<Json<AssignedWabaList>, ApiError> {
    let wabas = state
        .signups()
        .partners(state.authz(), &admin, state.settings().legal_acts)?
        .observed_by(state.metrics())
        .assigned_wabas(state.authz(), &user_id, query.limit, query.cursor)
        .await?;
    let (data, next_cursor) = page(wabas, |waba: AssignedWabaView| AssignedWabaBody {
        id: waba.id,
        name: waba.name,
    });
    Ok(Json(AssignedWabaList { data, next_cursor }))
}

// ─── The tenant's routes ─────────────────────────────────────────────────

fn waba_partners<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    owned: &'a OwnedWaba,
) -> Result<meta_whatsapp_server_core::partners::WabaPartners<'a>, ApiError> {
    Ok(state
        .signups()
        .waba_partners(
            state.authz(),
            caller,
            owned.capability(state),
            state.settings().legal_acts,
        )?
        .observed_by(state.metrics()))
}

/// `GET /v1/wabas/{waba_id}/business-verification`: the WABA's business
/// verification status.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/business-verification",
    tag = "partners",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The status", body = BusinessVerificationBody),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_business_verification(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
) -> Result<Json<BusinessVerificationBody>, ApiError> {
    let status = waba_partners(&state, &caller, &owned)?
        .business_verification()
        .await?;
    Ok(Json(status.into()))
}

async fn create_intent(
    service: &meta_whatsapp_server_core::partners::WabaPartners<'_>,
    solution_id: &str,
) -> Result<Success, ApiError> {
    let id = service
        .create_solution_migration_intent(solution_id)
        .await?;
    Success::json(StatusCode::CREATED, &CreatedIntentBody { id })
}

/// `POST /v1/wabas/{waba_id}/solution-migration-intents`: tag the WABA for
/// migration to a Multi-Partner Solution; Meta asks the WABA's client to
/// accept in Meta Business Suite (and accepts for them after 90 days).
/// **A legal act**: off until the deployment sets
/// `WA_SERVER_LEGAL_ACTS=on`; audited; takes an `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/solution-migration-intents",
    tag = "partners",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never create twice"),
    ),
    request_body = CreateIntentBody,
    responses(
        (status = 201, description = "Created; audited `solution_migration_intent_created`", body = CreatedIntentBody),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`feature_not_available` (the legal acts are off: nothing was asked of Meta), `forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `solution_id` or `body`; `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the intent may have been created", body = ErrorBody),
    )
)]
pub(crate) async fn create_solution_migration_intent(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    _gate: LegalActsOn,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_solution_migration_intent(owned.waba_id(), &body);
    let Ok(body) = serde_json::from_value::<CreateIntentBody>(body) else {
        return ApiError::invalid("body").into_response();
    };
    let service = match waba_partners(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(
            &caller,
            key,
            fingerprint,
            create_intent(&service, &body.solution_id),
        )
        .await
}

/// `GET /v1/wabas/{waba_id}/solution-migration-intents/{intent_id}`: a
/// migration intent of the WABA. An intent that does not name this WABA is
/// `404`, and nothing of it is answered.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/solution-migration-intents/{intent_id}",
    tag = "partners",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("intent_id" = String, Path, description = "The intent's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The intent", body = MigrationIntentBody),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such intent of it (another WABA's looks like a missing one)", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_solution_migration_intent(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, intent_id)): Path<(String, String)>,
) -> Result<Json<MigrationIntentBody>, ApiError> {
    let intent = waba_partners(&state, &caller, &owned)?
        .solution_migration_intent(&intent_id)
        .await?;
    Ok(Json(intent.into()))
}

async fn create_migration(
    service: &meta_whatsapp_server_core::partners::WabaPartners<'_>,
    body: &CreateBillingMigrationBody,
) -> Result<Success, ApiError> {
    let started = service
        .create_billing_migration(&body.currency, body.extended_credit_id.as_deref())
        .await?;
    Success::json(StatusCode::CREATED, &BillingMigrationBody::from(started))
}

/// `POST /v1/wabas/{waba_id}/billing-migrations`: start moving the WABA to
/// another billing currency or payment method: Meta clones it with its
/// templates. **A legal act** (financial): off until the deployment sets
/// `WA_SERVER_LEGAL_ACTS=on`; audited; takes an `Idempotency-Key`.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/billing-migrations",
    tag = "partners",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never create twice"),
    ),
    request_body = CreateBillingMigrationBody,
    responses(
        (status = 201, description = "Started; audited `billing_migration_started`. Poll it until `READY_TO_COMPLETE`", body = BillingMigrationBody),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`feature_not_available` (the legal acts are off: nothing was asked of Meta), `forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `currency`, `extended_credit_id` or `body`: nothing was sent; `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: the migration may have started", body = ErrorBody),
    )
)]
pub(crate) async fn create_billing_migration(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    _gate: LegalActsOn,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_billing_migration(owned.waba_id(), &body);
    let Ok(body) = serde_json::from_value::<CreateBillingMigrationBody>(body) else {
        return ApiError::invalid("body").into_response();
    };
    let service = match waba_partners(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, create_migration(&service, &body))
        .await
}

/// `GET /v1/wabas/{waba_id}/billing-migrations/{migration_id}`: where a
/// billing migration of the WABA stands. A migration the WABA did not
/// start through this service is `404`, and Meta is not asked.
#[utoipa::path(
    get,
    path = "/v1/wabas/{waba_id}/billing-migrations/{migration_id}",
    tag = "partners",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("migration_id" = String, Path, description = "The migration's id (`mig_…`)"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The migration", body = BillingMigrationBody),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such migration of it", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_billing_migration(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, migration_id)): Path<(String, String)>,
) -> Result<Json<BillingMigrationBody>, ApiError> {
    let migration = waba_partners(&state, &caller, &owned)?
        .billing_migration(&migration_id)
        .await?;
    Ok(Json(migration.into()))
}

/// `POST /v1/wabas/{waba_id}/billing-migrations/{migration_id}/resume`:
/// complete a billing migration: the clone becomes the billing account of
/// the numbers, and the old WABA can no longer send. **A legal act**
/// (financial, not undone by the API): off until the deployment sets
/// `WA_SERVER_LEGAL_ACTS=on`; audited; never replayed.
#[utoipa::path(
    post,
    path = "/v1/wabas/{waba_id}/billing-migrations/{migration_id}/resume",
    tag = "partners",
    security(("api_key" = [])),
    params(
        ("waba_id" = String, Path, description = "WhatsApp Business Account id"),
        ("migration_id" = String, Path, description = "The migration's id (`mig_…`)"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "Completed or completing; audited `billing_migration_resumed`", body = BillingMigrationBody),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`feature_not_available` (the legal acts are off: nothing was asked of Meta), `forbidden`, `tenant_suspended`, or Meta's refusal", body = ErrorBody),
        (status = 404, description = "`not_found`: no such WABA for this tenant, or no such migration of it; Meta was not asked", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling", body = ErrorBody),
        (status = 502, description = "Meta failed: see `may_have_been_sent`", body = ErrorBody),
        (status = 504, description = "`timeout`: read the migration before resuming again", body = ErrorBody),
    )
)]
pub(crate) async fn resume_billing_migration(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedWaba,
    Path((_, migration_id)): Path<(String, String)>,
) -> Result<Json<BillingMigrationBody>, ApiError> {
    let migration = waba_partners(&state, &caller, &owned)?
        .resume_billing_migration(&migration_id)
        .await?;
    Ok(Json(migration.into()))
}
