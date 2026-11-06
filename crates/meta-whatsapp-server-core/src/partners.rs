//! Partner APIs (docs/design/server.md, section 4.2; roadmap M5j), as
//! services an API adapter calls, over the library's partner modules.
//!
//! **Who calls what.** Two kinds of caller, two services:
//!
//! - [`Partners`], the **operator's** (an admin key): what the deployment
//!   does as a Tech Provider or Solution Partner, with the credentials the
//!   pool of pre-verified numbers already uses (`WA_PARTNER_BUSINESS_ID`
//!   and the system token, decision D204; without them every call is
//!   `409 signup_not_configured`): partner-led business verification
//!   (`client::business_verification`), Multi-Partner Solutions
//!   (`client::partners::solutions`) and the WABAs assigned to a system
//!   user (`client::waba::BusinessUser`).
//! - [`WabaPartners`], the **tenant's**, on a WABA of its own (an
//!   [`OwnedWaba`], its token, scope `partners`): the WABA's business
//!   verification status, its migration intents to a solution
//!   (`client::partners::migration`) and its billing currency or payment
//!   method migration (`client::waba::Waba::set_payment_method_migration_intent`).
//!
//! **Legal and irreversible acts** (AGENTS.md, Legal acts; design D192 and
//! D250). Seven calls submit a statement about a business, accept or
//! decline something that cannot be taken back, or move a business's
//! assets between partners or accounts. Each is taken only when the
//! deployment enables the legal acts (`WA_SERVER_LEGAL_ACTS`, off by
//! default): off, it is `403 feature_not_available` with nothing asked of
//! Meta; on, it is audited. Nothing here takes one automatically, and no
//! other route makes one. The three of a tenant's WABA are
//! [`crate::number_settings::is_legal_act`]'s arms
//! ([`Operation::CreateSolutionMigrationIntent`],
//! [`Operation::CreateBillingMigration`],
//! [`Operation::ResumeBillingMigration`]); the four of the operator,
//! which are no tenant operation, are arms of
//! [`is_legal_admin_act`] ([`AdminOperation`]).
//!
//! **Never offered:** the solution token (`GET /{solution}/access_token`).
//! It is a bearer credential for a client's assets; no answer, log or
//! audit event carries it, and the one place the service needs a client's
//! business token through a solution is hosted Embedded Signup
//! ([`crate::signup`]), which stores it in the vault. Moving a phone
//! number between partners is not offered either: Meta's page says it is
//! not supported on the new account model.
//!
//! Business verification documents and Meta's texts are business or
//! personal data: they are in no log, audit event or error, and Meta's
//! texts of a failed call are withheld on the operator's calls.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use meta_whatsapp_rs::client::business_verification::{
    ListVerificationSubmissions, MAX_DOCUMENTS, VerificationDocument,
};
use meta_whatsapp_rs::client::partners::solutions::{
    CreateSolution, ListAppSolutions, ListConnectedClientBusinesses, ListSolutionPartners,
    SolutionInfo, SolutionRole,
};
use meta_whatsapp_rs::client::waba::{ListAssignedWabas, PaymentMethodMigrationIntent, WabaTask};
use meta_whatsapp_rs::core::ids::{
    AppId, BusinessId, CreditLineId, MigrationIntentId, SolutionId, SystemUserId, WabaId,
};
use meta_whatsapp_rs::core::paging::Page;
use meta_whatsapp_rs::core::store::Expiry;
use meta_whatsapp_rs::{Client, Error};
use serde::{Deserialize, Serialize};

use crate::audit::Outcome;
use crate::authz::{AdminCaller, Authorizer, Caller, OwnedWaba, object_failed};
use crate::error::{ErrorCode, ServiceError};
use crate::number_settings::{is_legal_act, is_legal_admin_act};
use crate::observe::Observer;
use crate::operation::{AdminOperation, Operation};
use crate::signup::Signups;

/// The key/value namespace of the billing migrations a tenant started
/// (decision D256; a new stored name, pending the owner's confirmation
/// before the first release): a migration's id, the WABA and the tenant
/// it was started for.
pub const BILLING_NAMESPACE: &str = "wa.server.billing_migration";

/// The audit action of [`Partners::submit_business_verification`].
pub const BUSINESS_VERIFICATION_SUBMITTED: &str = "business_verification_submitted";
/// The audit action of [`Partners::create_solution`].
pub const SOLUTION_CREATED: &str = "partner_solution_created";
/// The audit action of [`Partners::accept_solution`].
pub const SOLUTION_ACCEPTED: &str = "partner_solution_accepted";
/// The audit action of [`Partners::reject_solution`].
pub const SOLUTION_REJECTED: &str = "partner_solution_rejected";
/// The audit action of [`Partners::request_solution_deactivation`].
pub const SOLUTION_DEACTIVATION_REQUESTED: &str = "partner_solution_deactivation_requested";
/// The audit action of [`Partners::accept_solution_deactivation`].
pub const SOLUTION_DEACTIVATION_ACCEPTED: &str = "partner_solution_deactivation_accepted";
/// The audit action of [`Partners::reject_solution_deactivation`].
pub const SOLUTION_DEACTIVATION_REJECTED: &str = "partner_solution_deactivation_rejected";
/// The audit action of [`WabaPartners::create_solution_migration_intent`].
pub const SOLUTION_MIGRATION_INTENT_CREATED: &str = "solution_migration_intent_created";
/// The audit action of [`WabaPartners::create_billing_migration`].
pub const BILLING_MIGRATION_STARTED: &str = "billing_migration_started";
/// The audit action of [`WabaPartners::resume_billing_migration`].
pub const BILLING_MIGRATION_RESUMED: &str = "billing_migration_resumed";

/// The most characters of a cursor accepted.
pub const MAX_CURSOR_CHARS: usize = 512;
/// The most characters of a solution's name (the accept-deactivation
/// reference gives 2 to 75).
pub const SOLUTION_NAME_MAX_CHARS: usize = 75;

/// An action of the operator's partner routes: what [`Partners`] does, by
/// name, so that [`is_legal_admin_act`] is the one place that says which
/// are legal acts (the operator's routes are no [`Operation`]s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartnerAction {
    /// `POST /v1/admin/partners/business-verifications`: a statement of a
    /// customer business's legal identity, with its documents.
    SubmitBusinessVerification,
    /// `GET /v1/admin/partners/business-verifications`.
    ListBusinessVerifications,
    /// `POST /v1/admin/partners/solutions`.
    CreateSolution,
    /// `GET /v1/admin/partners/solutions`.
    ListSolutions,
    /// `GET /v1/admin/partners/solutions/{id}`.
    GetSolution,
    /// `GET /v1/admin/partners/solutions/{id}/partners`.
    ListSolutionPartners,
    /// `GET /v1/admin/partners/client-businesses`.
    ListClientBusinesses,
    /// `POST /v1/admin/partners/solutions/{id}/accept`: cannot be undone.
    AcceptSolution,
    /// `POST /v1/admin/partners/solutions/{id}/reject`: permanent.
    RejectSolution,
    /// `POST /v1/admin/partners/solutions/{id}/deactivation-request`.
    RequestSolutionDeactivation,
    /// `POST /v1/admin/partners/solutions/{id}/deactivation-request/accept`:
    /// the solution becomes `DEACTIVATED`.
    AcceptSolutionDeactivation,
    /// `POST /v1/admin/partners/solutions/{id}/deactivation-request/reject`.
    RejectSolutionDeactivation,
    /// `GET /v1/admin/partners/users/{user_id}/wabas`.
    ListAssignedWabas,
}

impl PartnerAction {
    /// Every action.
    pub const ALL: [Self; 13] = [
        Self::SubmitBusinessVerification,
        Self::ListBusinessVerifications,
        Self::CreateSolution,
        Self::ListSolutions,
        Self::GetSolution,
        Self::ListSolutionPartners,
        Self::ListClientBusinesses,
        Self::AcceptSolution,
        Self::RejectSolution,
        Self::RequestSolutionDeactivation,
        Self::AcceptSolutionDeactivation,
        Self::RejectSolutionDeactivation,
        Self::ListAssignedWabas,
    ];
}

/// The admin operation `action` is, when it is a legal act (design D250):
/// the four of the operator, whose gate is
/// [`crate::number_settings::is_legal_admin_act`], the one admin predicate.
fn admin_act(action: PartnerAction) -> Option<AdminOperation> {
    match action {
        PartnerAction::SubmitBusinessVerification => {
            Some(AdminOperation::SubmitBusinessVerification)
        }
        PartnerAction::AcceptSolution => Some(AdminOperation::AcceptPartnerSolution),
        PartnerAction::RejectSolution => Some(AdminOperation::RejectPartnerSolution),
        PartnerAction::AcceptSolutionDeactivation => {
            Some(AdminOperation::AcceptPartnerSolutionDeactivation)
        }
        PartnerAction::ListBusinessVerifications
        | PartnerAction::CreateSolution
        | PartnerAction::ListSolutions
        | PartnerAction::GetSolution
        | PartnerAction::ListSolutionPartners
        | PartnerAction::ListClientBusinesses
        | PartnerAction::RequestSolutionDeactivation
        | PartnerAction::RejectSolutionDeactivation
        | PartnerAction::ListAssignedWabas => None,
    }
}

fn gate_partner(action: PartnerAction, enabled: bool) -> Result<(), ServiceError> {
    if enabled || !admin_act(action).is_some_and(is_legal_admin_act) {
        Ok(())
    } else {
        Err(ServiceError::new(ErrorCode::FEATURE_NOT_AVAILABLE))
    }
}

fn gate_operation(operation: Operation, enabled: bool) -> Result<(), ServiceError> {
    if enabled || !is_legal_act(operation) {
        Ok(())
    } else {
        Err(ServiceError::new(ErrorCode::FEATURE_NOT_AVAILABLE))
    }
}

// ─── The records ─────────────────────────────────────────────────────────

/// The billing migration a tenant started: whose it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BillingMigration {
    tenant_id: String,
    waba_id: String,
}

impl Signups {
    /// The operator's partner services, once `authz` admits `admin`.
    /// `legal_acts` is the deployment's setting (`WA_SERVER_LEGAL_ACTS`).
    ///
    /// # Errors
    ///
    /// `admin` another [`Authorizer`] made: `403 forbidden`. Nothing is
    /// read.
    pub fn partners<'a>(
        &'a self,
        authz: &'a Authorizer,
        admin: &'a AdminCaller,
        legal_acts: bool,
    ) -> Result<Partners<'a>, ServiceError> {
        authz.admit_admin(admin)?;
        Ok(Partners {
            signups: self,
            admin,
            observer: &(),
            legal_acts,
        })
    }

    /// The tenant's partner services on `waba`, once `authz` admits both
    /// ([`Authorizer::admit`], [`Authorizer::admit_waba`]).
    ///
    /// # Errors
    ///
    /// `caller` or `waba` another [`Authorizer`] made: `403 forbidden`;
    /// `waba` another tenant's: `404 not_found`, logged at `warn`. Before
    /// anything is asked of Meta.
    pub fn waba_partners<'a>(
        &'a self,
        authz: &'a Authorizer,
        caller: &'a Caller,
        waba: &'a OwnedWaba,
        legal_acts: bool,
    ) -> Result<WabaPartners<'a>, ServiceError> {
        authz.admit(caller)?;
        authz.admit_waba(waba)?;
        if waba.tenant() != caller.tenant() {
            tracing::warn!("refused the partner services on another tenant's WABA");
            return Err(ServiceError::not_found());
        }
        Ok(WabaPartners {
            signups: self,
            authz,
            caller,
            waba,
            observer: &(),
            legal_acts,
        })
    }
}

// ─── What the services answer ────────────────────────────────────────────

/// A documents' submission, as the operator's request carries it. No
/// `Debug`: it holds the documents.
pub struct VerificationRequest {
    /// The customer's business portfolio (`end_business_id`).
    pub client_business_id: String,
    /// One to three documents.
    pub documents: Vec<DocumentInput>,
}

/// One document: a name whose extension is its type, and its content in
/// base64. No `Debug`: it holds the content.
pub struct DocumentInput {
    /// The file name (`.pdf`, `.jpeg`, `.jpg` or `.png`).
    pub file_name: String,
    /// The content, base64 (standard alphabet).
    pub content_base64: String,
}

/// What Meta answered a submission: how many it has counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerificationReceipt {
    /// Submissions made for this customer, this one included.
    pub verification_attempts: Option<u32>,
    /// What is left of the three.
    pub attempts_left: Option<u32>,
}

/// One submission of the operator's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionView {
    /// Its id.
    pub id: String,
    /// `APPROVED`, `DISCARDED`, `FAILED`, `PENDING`, `REVOKED`, or another
    /// value as Meta sent it.
    pub status: Option<String>,
    /// Why it was rejected, as Meta's `rejection_reasons`, verbatim.
    pub rejection_reasons: Vec<String>,
    /// When it was submitted, as Meta sent it.
    pub submitted_time: Option<String>,
    /// When it last changed, as Meta sent it.
    pub update_time: Option<String>,
    /// The customer's business portfolio.
    pub client_business_id: Option<String>,
}

/// A submission list's filter.
#[derive(Debug, Clone, Default)]
pub struct SubmissionQuery {
    /// Only this customer's.
    pub client_business_id: Option<String>,
    /// The cursor of the page.
    pub after: Option<String>,
}

/// A new solution, as the operator's request carries it.
#[derive(Debug, Clone)]
pub struct NewSolution {
    /// Its name, 2 to 75 characters.
    pub name: String,
    /// The invited partner's app.
    pub partner_app_id: String,
    /// What clients grant the deployment's app (`MESSAGING` or nothing).
    pub owner_permissions: Vec<String>,
    /// What clients grant the partner's app.
    pub partner_permissions: Vec<String>,
}

/// A solution list's filter.
#[derive(Debug, Clone, Default)]
pub struct SolutionQuery {
    /// `OWNER` or `PARTNER`.
    pub role: Option<String>,
    /// The page size, 1 to 100.
    pub limit: Option<u32>,
    /// The cursor of the page.
    pub after: Option<String>,
}

/// A solution.
#[derive(Debug, Clone, PartialEq)]
pub struct SolutionView {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: Option<String>,
    /// `INITIATED`, `ACTIVE`, `DEACTIVATED`, … as Meta sent it.
    pub status: Option<String>,
    /// The transition waiting for the other partner.
    pub status_for_pending_request: Option<String>,
    /// The owner's app.
    pub owner_app_id: Option<String>,
    /// The owner's permissions.
    pub owner_permissions: Option<Vec<String>>,
    /// The partners, when asked for.
    pub partners: Option<Vec<SolutionPartnerView>>,
}

/// A partner of a solution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolutionPartnerView {
    /// The partner's record.
    pub id: Option<String>,
    /// The partner's app.
    pub partner_app_id: Option<String>,
    /// The partner's app name.
    pub partner_app_name: Option<String>,
    /// `NOTIFICATION_SENT` or `ACCEPTED`, as Meta sent it.
    pub status: Option<String>,
    /// What clients grant it.
    pub partner_permissions: Option<Vec<String>>,
}

/// A business onboarded through the deployment's app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientBusinessView {
    /// Its portfolio.
    pub id: String,
    /// Its name.
    pub name: Option<String>,
}

/// A page of a read, with the next page's cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct Paged<T> {
    /// The items.
    pub data: Vec<T>,
    /// The next page's cursor, when there is one.
    pub next_cursor: Option<String>,
}

/// A WABA assigned to a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedWabaView {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: Option<String>,
}

/// A WABA's business verification status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessVerificationView {
    /// Meta's `business_verification_status`, as sent (`verified`,
    /// `pending`, …); `None` when it did not say.
    pub status: Option<String>,
    /// Whether it is `verified`.
    pub verified: bool,
}

/// A migration intent, as the WABA's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationIntentView {
    /// Its id.
    pub id: String,
    /// `INITIATED`, `ACCEPTED`, … as Meta sent it.
    pub status: Option<String>,
    /// The destination solution.
    pub solution_id: Option<String>,
    /// The destination solution's name.
    pub solution_name: Option<String>,
    /// The WABA the migration lands on.
    pub destination_waba_id: Option<String>,
    /// The source WABA.
    pub waba_id: Option<String>,
}

/// A billing migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BillingMigrationView {
    /// Its id.
    pub id: String,
    /// `INITIATED`, `READY_TO_COMPLETE`, `COMPLETED`, … as Meta sent it.
    pub status: String,
    /// The clone Meta created, when it said.
    pub destination_waba_id: Option<String>,
    /// The clone's currency.
    pub destination_currency: Option<String>,
}

impl From<SolutionInfo> for SolutionView {
    fn from(solution: SolutionInfo) -> Self {
        Self {
            id: solution.id.as_str().to_owned(),
            name: solution.name,
            status: solution.status.map(|s| s.as_str().to_owned()),
            status_for_pending_request: solution
                .status_for_pending_request
                .map(|s| s.as_str().to_owned()),
            owner_app_id: solution
                .owner_app
                .and_then(|app| app.id)
                .map(|id| id.as_str().to_owned()),
            owner_permissions: solution
                .owner_permissions
                .map(|tasks| tasks.iter().map(|t| t.as_str().to_owned()).collect()),
            partners: solution
                .partners
                .map(|page| page.data.into_iter().map(partner_view).collect()),
        }
    }
}

fn partner_view(
    partner: meta_whatsapp_rs::client::partners::solutions::SolutionPartner,
) -> SolutionPartnerView {
    let (app_id, app_name) = partner.partner_app.map_or((None, None), |app| {
        (app.id.map(|id| id.as_str().to_owned()), app.name)
    });
    SolutionPartnerView {
        id: partner.id,
        partner_app_id: app_id,
        partner_app_name: app_name,
        status: partner.status.map(|s| s.as_str().to_owned()),
        partner_permissions: partner
            .partner_permissions
            .map(|tasks| tasks.iter().map(|t| t.as_str().to_owned()).collect()),
    }
}

fn paged<T, U>(page: Page<T>, convert: impl FnMut(T) -> U) -> Paged<U> {
    let next_cursor = page.next_cursor().map(str::to_owned);
    Paged {
        data: page.data.into_iter().map(convert).collect(),
        next_cursor,
    }
}

// ─── Checks ──────────────────────────────────────────────────────────────

fn cursor(after: Option<String>) -> Result<Option<String>, ServiceError> {
    match after {
        Some(cursor) if cursor.is_empty() || cursor.len() > MAX_CURSOR_CHARS => {
            Err(ServiceError::invalid("cursor"))
        }
        other => Ok(other),
    }
}

fn limit(limit: Option<u32>) -> Result<Option<u32>, ServiceError> {
    match limit {
        Some(0 | 101..) => Err(ServiceError::invalid("limit")),
        other => Ok(other),
    }
}

fn tasks(field: &'static str, values: &[String]) -> Result<Vec<WabaTask>, ServiceError> {
    values
        .iter()
        .map(|value| match WabaTask::from(value.as_str()) {
            WabaTask::Other(_) => Err(ServiceError::invalid(field)),
            task => Ok(task),
        })
        .collect()
}

/// Meta's refusal as the operator's calls answer it: its code, never its
/// text or details (they may quote a business's data); told to `observer`
/// when Meta answered.
fn refused(error: &Error, observer: &dyn Observer) -> ServiceError {
    let api = ServiceError::from_library_withholding_texts(error);
    if error.graph().is_some() {
        observer.graph_error(api.code());
    }
    api
}

fn outcome(error: &ServiceError) -> Outcome {
    if error.may_have_been_sent() {
        Outcome::Unanswered
    } else {
        Outcome::Answered
    }
}

/// An operator's change: ids only, never a document, a token or a Meta
/// text.
fn audit_admin(
    action: &'static str,
    admin: &AdminCaller,
    subject_id: Option<&str>,
    count: Option<usize>,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        admin_key_id = admin.key_id(),
        subject_id,
        count,
        outcome = outcome.as_str(),
        "admin change"
    );
}

/// A tenant's change on its WABA.
fn audit_tenant(
    action: &'static str,
    caller: &Caller,
    waba: &WabaId,
    subject_id: Option<&str>,
    outcome: Outcome,
) {
    tracing::info!(
        target: "audit",
        action,
        key_id = caller.key_id(),
        tenant = caller.tenant().as_str(),
        waba_id = waba.as_str(),
        subject_id,
        outcome = outcome.as_str(),
        "tenant change"
    );
}

// ─── The operator's services ─────────────────────────────────────────────

/// The operator's partner services ([`Signups::partners`]).
pub struct Partners<'a> {
    signups: &'a Signups,
    admin: &'a AdminCaller,
    observer: &'a dyn Observer,
    legal_acts: bool,
}

impl fmt::Debug for Partners<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Partners")
            .field("legal_acts", &self.legal_acts)
            .finish_non_exhaustive()
    }
}

impl<'a> Partners<'a> {
    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// The legal-act gate, then the deployment's partner credentials: a
    /// client acting with the system token, the portfolio and the app.
    fn open(&self, authz: &Authorizer, action: PartnerAction) -> Result<Session, ServiceError> {
        gate_partner(action, self.legal_acts)?;
        let deployment = self
            .signups
            .deployment()
            .ok_or_else(|| ServiceError::new(ErrorCode::SIGNUP_NOT_CONFIGURED))?;
        let pool = deployment
            .partner_pool()
            .ok_or_else(|| ServiceError::new(ErrorCode::SIGNUP_NOT_CONFIGURED))?;
        Ok(Session {
            client: authz.client().with_token(pool.token().clone()),
            business: pool.business_id().clone(),
            app: deployment.app_id().clone(),
        })
    }
}

/// The partner credentials, once opened.
struct Session {
    client: Client,
    business: BusinessId,
    app: AppId,
}

/// An id of Meta's in a path or a body: digits, bounded.
fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_digit())
}

fn solution_id(id: &str) -> Result<SolutionId, ServiceError> {
    SolutionId::new(id).map_err(|_| ServiceError::invalid("id"))
}

impl Partners<'_> {
    /// Submit a customer business for partner-led verification with its
    /// documents (`POST /{business}/self_certify_whatsapp_business`,
    /// multipart). A legal act ([`PartnerAction::SubmitBusinessVerification`]).
    /// Checked first: one to three documents, each a PDF, JPEG or PNG of at
    /// most 5 MiB whose content is its type's. Never replayed: each
    /// submission counts towards the customer's three.
    ///
    /// # Errors
    ///
    /// `403 feature_not_available` with the legal acts off; `409
    /// signup_not_configured`; `422` on `client_business_id` or
    /// `documents`; Meta's refusal, as its code says.
    pub async fn submit_business_verification(
        &self,
        authz: &Authorizer,
        request: VerificationRequest,
    ) -> Result<VerificationReceipt, ServiceError> {
        let session = self.open(authz, PartnerAction::SubmitBusinessVerification)?;
        let customer = BusinessId::new(request.client_business_id)
            .map_err(|_| ServiceError::invalid("client_business_id"))?;
        if request.documents.is_empty() || request.documents.len() > MAX_DOCUMENTS {
            return Err(ServiceError::invalid("documents"));
        }
        let mut documents = Vec::with_capacity(request.documents.len());
        for input in request.documents {
            let data = STANDARD
                .decode(input.content_base64.as_bytes())
                .map_err(|_| ServiceError::invalid("documents"))?;
            documents.push(
                VerificationDocument::from_file_name(input.file_name, data)
                    .map_err(|_| ServiceError::invalid("documents"))?,
            );
        }
        let count = documents.len();
        match session
            .client
            .business_verification()
            .submit(&session.business, &customer, &documents)
            .await
        {
            Ok(receipt) => {
                audit_admin(
                    BUSINESS_VERIFICATION_SUBMITTED,
                    self.admin,
                    Some(customer.as_str()),
                    Some(count),
                    Outcome::Answered,
                );
                Ok(VerificationReceipt {
                    verification_attempts: receipt.verification_attempts,
                    attempts_left: receipt.attempts_left(),
                })
            }
            Err(error) => {
                let api = refused(&error, self.observer);
                audit_admin(
                    BUSINESS_VERIFICATION_SUBMITTED,
                    self.admin,
                    Some(customer.as_str()),
                    Some(count),
                    outcome(&api),
                );
                Err(api)
            }
        }
    }

    /// One page of the submissions the deployment made
    /// (`GET /{business}/self_certified_whatsapp_business_submissions`).
    ///
    /// # Errors
    ///
    /// `409 signup_not_configured`; `422` on `client_business_id` or
    /// `cursor`; Meta's refusal.
    pub async fn business_verifications(
        &self,
        authz: &Authorizer,
        query: SubmissionQuery,
    ) -> Result<Paged<SubmissionView>, ServiceError> {
        let session = self.open(authz, PartnerAction::ListBusinessVerifications)?;
        let mut list = ListVerificationSubmissions::new();
        if let Some(customer) = query.client_business_id {
            list = list.end_business_id(
                BusinessId::new(customer)
                    .map_err(|_| ServiceError::invalid("client_business_id"))?,
            );
        }
        if let Some(after) = cursor(query.after)? {
            list = list.after(after);
        }
        let page = session
            .client
            .business_verification()
            .submissions(&session.business, &list)
            .await
            .map_err(|error| refused(&error, self.observer))?;
        Ok(paged(page, |submission| SubmissionView {
            id: submission.id.as_str().to_owned(),
            status: submission
                .verification_status
                .as_ref()
                .map(|s| s.as_str().to_owned()),
            rejection_reasons: submission.rejection_reasons,
            submitted_time: submission.submitted_time,
            update_time: submission.update_time,
            client_business_id: submission
                .client_business_id
                .map(|id| id.as_str().to_owned()),
        }))
    }

    /// Create a solution owned by the deployment's app, inviting another
    /// (`POST /{app}/whatsapp_business_solution`). It starts `INITIATED`;
    /// the partner accepts it. Not a legal act (the invitation binds
    /// nobody until accepted); audited. Never replayed.
    ///
    /// # Errors
    ///
    /// `409 signup_not_configured`; `422` on `name`, `partner_app_id`,
    /// `owner_permissions` or `partner_permissions`; Meta's refusal.
    pub async fn create_solution(
        &self,
        authz: &Authorizer,
        new: NewSolution,
    ) -> Result<String, ServiceError> {
        let session = self.open(authz, PartnerAction::CreateSolution)?;
        let name = new.name.trim();
        if name.chars().count() < 2 || name.chars().count() > SOLUTION_NAME_MAX_CHARS {
            return Err(ServiceError::invalid("name"));
        }
        let partner =
            AppId::new(new.partner_app_id).map_err(|_| ServiceError::invalid("partner_app_id"))?;
        let owner = tasks("owner_permissions", &new.owner_permissions)?;
        let partner_permissions = tasks("partner_permissions", &new.partner_permissions)?;
        let solution = CreateSolution::new(name, partner, owner, partner_permissions);
        solution
            .validate()
            .map_err(|error| ServiceError::invalid(error.field))?;
        match session
            .client
            .partner_solutions()
            .create(&session.app, &solution)
            .await
        {
            Ok(created) => {
                let id = created.solution_id.as_str().to_owned();
                audit_admin(
                    SOLUTION_CREATED,
                    self.admin,
                    Some(&id),
                    None,
                    Outcome::Answered,
                );
                Ok(id)
            }
            Err(error) => {
                let api = refused(&error, self.observer);
                audit_admin(SOLUTION_CREATED, self.admin, None, None, outcome(&api));
                Err(api)
            }
        }
    }

    /// One page of the deployment's app's solutions
    /// (`GET /{app}/whatsapp_business_solutions`).
    ///
    /// # Errors
    ///
    /// `409 signup_not_configured`; `422` on `role`, `limit` or `cursor`;
    /// Meta's refusal.
    pub async fn solutions(
        &self,
        authz: &Authorizer,
        query: SolutionQuery,
    ) -> Result<Paged<SolutionView>, ServiceError> {
        let session = self.open(authz, PartnerAction::ListSolutions)?;
        let mut list =
            ListAppSolutions::new().fields(["id", "name", "status", "status_for_pending_request"]);
        if let Some(role) = query.role {
            list = list.role(match role.as_str() {
                "OWNER" => SolutionRole::Owner,
                "PARTNER" => SolutionRole::Partner,
                _ => return Err(ServiceError::invalid("role")),
            });
        }
        if let Some(size) = limit(query.limit)? {
            list = list.limit(size);
        }
        if let Some(after) = cursor(query.after)? {
            list = list.after(after);
        }
        let page = session
            .client
            .partner_solutions()
            .app_solutions(&session.app, &list)
            .await
            .map_err(|error| refused(&error, self.observer))?;
        Ok(paged(page, SolutionView::from))
    }

    /// A solution with its owner, permissions and partners, as the guide
    /// asks before accepting (`GET /{solution}`).
    ///
    /// # Errors
    ///
    /// `409 signup_not_configured`; `422` on `id`; Meta's refusal.
    pub async fn solution(
        &self,
        authz: &Authorizer,
        id: &str,
    ) -> Result<SolutionView, ServiceError> {
        let session = self.open(authz, PartnerAction::GetSolution)?;
        let id = solution_id(id)?;
        session
            .client
            .partner_solutions()
            .get(
                &id,
                &[
                    "id",
                    "name",
                    "status",
                    "status_for_pending_request",
                    "owner_app",
                    "owner_permissions",
                    "partners{partner_permissions,partner_app}",
                ],
            )
            .await
            .map(SolutionView::from)
            .map_err(|error| refused(&error, self.observer))
    }

    /// One page of a solution's partners (`GET /{solution}/partners`).
    ///
    /// # Errors
    ///
    /// As [`Self::solution`]; `422` on `cursor`.
    pub async fn solution_partners(
        &self,
        authz: &Authorizer,
        id: &str,
        after: Option<String>,
    ) -> Result<Paged<SolutionPartnerView>, ServiceError> {
        let session = self.open(authz, PartnerAction::ListSolutionPartners)?;
        let id = solution_id(id)?;
        let mut list = ListSolutionPartners::new();
        if let Some(after) = cursor(after)? {
            list = list.after(after);
        }
        let page = session
            .client
            .partner_solutions()
            .partners(&id, &list)
            .await
            .map_err(|error| refused(&error, self.observer))?;
        Ok(paged(page, partner_view))
    }

    /// One page of the businesses onboarded through the deployment's app
    /// (`GET /{app}/connected_client_businesses`).
    ///
    /// # Errors
    ///
    /// `409 signup_not_configured`; `422` on `limit` or `cursor`; Meta's
    /// refusal.
    pub async fn client_businesses(
        &self,
        authz: &Authorizer,
        size: Option<u32>,
        after: Option<String>,
    ) -> Result<Paged<ClientBusinessView>, ServiceError> {
        let session = self.open(authz, PartnerAction::ListClientBusinesses)?;
        let mut list = ListConnectedClientBusinesses::new();
        if let Some(size) = limit(size)? {
            list = list.limit(size);
        }
        if let Some(after) = cursor(after)? {
            list = list.after(after);
        }
        let page = session
            .client
            .partner_solutions()
            .connected_client_businesses(&session.app, &list)
            .await
            .map_err(|error| refused(&error, self.observer))?;
        Ok(paged(page, |business| ClientBusinessView {
            id: business.id.as_str().to_owned(),
            name: business.name,
        }))
    }

    /// Accept a solution's invitation (`POST /{solution}/accept`). A legal
    /// act: **it cannot be undone**, a solution cannot be declined once
    /// accepted. Read it ([`Self::solution`]) first. Audited.
    ///
    /// # Errors
    ///
    /// `403 feature_not_available` with the legal acts off; as
    /// [`Self::solution`].
    pub async fn accept_solution(&self, authz: &Authorizer, id: &str) -> Result<(), ServiceError> {
        self.state_change(authz, PartnerAction::AcceptSolution, SOLUTION_ACCEPTED, id)
            .await
    }

    /// Decline a solution (`POST /{solution}/reject`). A legal act: it is
    /// permanent. Audited.
    ///
    /// # Errors
    ///
    /// As [`Self::accept_solution`].
    pub async fn reject_solution(&self, authz: &Authorizer, id: &str) -> Result<(), ServiceError> {
        self.state_change(authz, PartnerAction::RejectSolution, SOLUTION_REJECTED, id)
            .await
    }

    /// Ask the partner to deactivate a solution
    /// (`POST /{solution}/send_deactivation_request`). Not a legal act (the
    /// solution stays usable until the partner accepts); audited.
    ///
    /// # Errors
    ///
    /// As [`Self::solution`].
    pub async fn request_solution_deactivation(
        &self,
        authz: &Authorizer,
        id: &str,
    ) -> Result<(), ServiceError> {
        self.state_change(
            authz,
            PartnerAction::RequestSolutionDeactivation,
            SOLUTION_DEACTIVATION_REQUESTED,
            id,
        )
        .await
    }

    /// Accept a pending deactivation request
    /// (`POST /{solution}/accept_deactivation_request`). A legal act: the
    /// solution becomes `DEACTIVATED` and onboards no client. Audited.
    ///
    /// # Errors
    ///
    /// As [`Self::accept_solution`].
    pub async fn accept_solution_deactivation(
        &self,
        authz: &Authorizer,
        id: &str,
    ) -> Result<(), ServiceError> {
        let session = self.open(authz, PartnerAction::AcceptSolutionDeactivation)?;
        let id = solution_id(id)?;
        let result = session
            .client
            .partner_solutions()
            .accept_deactivation_request(&id)
            .await
            .map(drop);
        self.settled(SOLUTION_DEACTIVATION_ACCEPTED, &id, result)
    }

    /// Reject a pending deactivation request
    /// (`POST /{solution}/reject_deactivation_request`); the solution stays
    /// `ACTIVE`. Not a legal act; audited.
    ///
    /// # Errors
    ///
    /// As [`Self::solution`].
    pub async fn reject_solution_deactivation(
        &self,
        authz: &Authorizer,
        id: &str,
    ) -> Result<(), ServiceError> {
        let session = self.open(authz, PartnerAction::RejectSolutionDeactivation)?;
        let id = solution_id(id)?;
        let result = session
            .client
            .partner_solutions()
            .reject_deactivation_request(&id)
            .await
            .map(drop);
        self.settled(SOLUTION_DEACTIVATION_REJECTED, &id, result)
    }

    async fn state_change(
        &self,
        authz: &Authorizer,
        action: PartnerAction,
        audit: &'static str,
        id: &str,
    ) -> Result<(), ServiceError> {
        let session = self.open(authz, action)?;
        let id = solution_id(id)?;
        let solutions = session.client.partner_solutions();
        let result = match action {
            PartnerAction::AcceptSolution => solutions.accept(&id).await,
            PartnerAction::RejectSolution => solutions.reject(&id).await,
            _ => solutions.send_deactivation_request(&id).await,
        };
        self.settled(audit, &id, result)
    }

    fn settled(
        &self,
        audit: &'static str,
        id: &SolutionId,
        result: Result<(), Error>,
    ) -> Result<(), ServiceError> {
        match result {
            Ok(()) => {
                audit_admin(
                    audit,
                    self.admin,
                    Some(id.as_str()),
                    None,
                    Outcome::Answered,
                );
                Ok(())
            }
            Err(error) => {
                let api = refused(&error, self.observer);
                audit_admin(audit, self.admin, Some(id.as_str()), None, outcome(&api));
                Err(api)
            }
        }
    }

    /// One page of the WABAs assigned to a user of the deployment's
    /// business (`GET /{user}/assigned_whatsapp_business_accounts`).
    ///
    /// # Errors
    ///
    /// `409 signup_not_configured`; `422` on `user_id`, `limit` or
    /// `cursor`; Meta's refusal.
    pub async fn assigned_wabas(
        &self,
        authz: &Authorizer,
        user_id: &str,
        size: Option<u32>,
        after: Option<String>,
    ) -> Result<Paged<AssignedWabaView>, ServiceError> {
        let session = self.open(authz, PartnerAction::ListAssignedWabas)?;
        if !plain_id(user_id) {
            return Err(ServiceError::invalid("user_id"));
        }
        let user = SystemUserId::new(user_id);
        let mut list = ListAssignedWabas::new().fields(["id", "name"]);
        if let Some(size) = limit(size)? {
            list = list.limit(size);
        }
        if let Some(after) = cursor(after)? {
            list = list.after(after);
        }
        let page = session
            .client
            .business_user(user)
            .assigned_whatsapp_business_accounts(&list)
            .await
            .map_err(|error| refused(&error, self.observer))?;
        Ok(paged(page, |waba| AssignedWabaView {
            id: waba.id.as_str().to_owned(),
            name: waba.name,
        }))
    }
}

// ─── The tenant's services ───────────────────────────────────────────────

/// The tenant's partner services on one of its WABAs
/// ([`Signups::waba_partners`]).
pub struct WabaPartners<'a> {
    signups: &'a Signups,
    authz: &'a Authorizer,
    caller: &'a Caller,
    waba: &'a OwnedWaba,
    observer: &'a dyn Observer,
    legal_acts: bool,
}

impl fmt::Debug for WabaPartners<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WabaPartners")
            .field("waba", self.waba)
            .field("legal_acts", &self.legal_acts)
            .finish_non_exhaustive()
    }
}

/// A migration's id as a path carries it: what Meta's ids look like
/// (`mig_01HZ…`), bounded. Anything else is no migration of ours.
fn migration_key(id: &str) -> Option<&str> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    ok.then_some(id)
}

impl<'a> WabaPartners<'a> {
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

    fn waba_id(&self) -> &WabaId {
        self.waba.waba_id()
    }

    /// The WABA's business verification status
    /// (`GET /{waba}?fields=business_verification_status`).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn business_verification(&self) -> Result<BusinessVerificationView, ServiceError> {
        match self
            .waba
            .client()
            .waba(self.waba_id().clone())
            .get(&["business_verification_status"])
            .await
        {
            Ok(info) => {
                let verified = info
                    .business_verification_status
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("verified"));
                Ok(BusinessVerificationView {
                    status: info.business_verification_status,
                    verified,
                })
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Tag the WABA for migration to `solution`
    /// (`POST /{waba}/set_solution_migration_intent`), which asks its
    /// client to accept in Meta Business Suite. A legal act
    /// ([`Operation::CreateSolutionMigrationIntent`]): a request left
    /// unanswered for 90 days is accepted for the client. Audited; never
    /// replayed.
    ///
    /// # Errors
    ///
    /// `403 feature_not_available` with the legal acts off; `422
    /// invalid_request` on `solution_id`; Meta's refusal.
    pub async fn create_solution_migration_intent(
        &self,
        solution: &str,
    ) -> Result<String, ServiceError> {
        gate_operation(Operation::CreateSolutionMigrationIntent, self.legal_acts)?;
        let solution =
            SolutionId::new(solution).map_err(|_| ServiceError::invalid("solution_id"))?;
        match self
            .waba
            .client()
            .migration_intents()
            .create(self.waba_id(), &solution)
            .await
        {
            Ok(created) => {
                audit_tenant(
                    SOLUTION_MIGRATION_INTENT_CREATED,
                    self.caller,
                    self.waba_id(),
                    Some(created.id.as_str()),
                    Outcome::Answered,
                );
                Ok(created.id.as_str().to_owned())
            }
            Err(error) => {
                let api = self.failed(&error).await;
                audit_tenant(
                    SOLUTION_MIGRATION_INTENT_CREATED,
                    self.caller,
                    self.waba_id(),
                    Some(solution.as_str()),
                    outcome(&api),
                );
                Err(api)
            }
        }
    }

    /// A migration intent of the WABA (`GET /{intent}`): read with the
    /// WABA's token, and answered only if it names this WABA as its
    /// source or its destination. An intent that names another, or none,
    /// is `404 not_found`, and nothing of it is answered.
    ///
    /// # Errors
    ///
    /// `404 not_found` for an id that is not an intent's or is not this
    /// WABA's; Meta's refusal.
    pub async fn solution_migration_intent(
        &self,
        id: &str,
    ) -> Result<MigrationIntentView, ServiceError> {
        if !plain_id(id) {
            return Err(ServiceError::not_found());
        }
        let id = MigrationIntentId::new(id);
        let intent = match self
            .waba
            .client()
            .migration_intents()
            .get(
                &id,
                &["id", "status", "solution", "destination_waba", "waba"],
            )
            .await
        {
            Ok(intent) => intent,
            Err(error) => {
                // Meta refusing the id is an intent that is not this
                // WABA's: `404`, as an object named by id.
                let api = self.waba.failed(self.authz, &error).await;
                if error.graph().is_some() {
                    self.observer.graph_error(api.code());
                }
                return Err(object_failed(api, &error));
            }
        };
        let ours = |waba: &Option<meta_whatsapp_rs::client::partners::migration::MigrationWaba>| {
            waba.as_ref().and_then(|w| w.id.as_ref()) == Some(self.waba_id())
        };
        if !(ours(&intent.waba) || ours(&intent.destination_waba)) {
            return Err(ServiceError::not_found());
        }
        Ok(MigrationIntentView {
            id: intent.id.as_str().to_owned(),
            status: intent.status.as_ref().map(|s| s.as_str().to_owned()),
            solution_id: intent.solution.as_ref().map(|s| s.id.as_str().to_owned()),
            solution_name: intent.solution.and_then(|s| s.name),
            destination_waba_id: intent
                .destination_waba
                .and_then(|w| w.id)
                .map(|id| id.as_str().to_owned()),
            waba_id: intent
                .waba
                .and_then(|w| w.id)
                .map(|id| id.as_str().to_owned()),
        })
    }

    /// Start moving the WABA to another billing currency or payment method
    /// (`POST /{waba}/set_payment_method_migration_intent`): Meta clones
    /// the WABA with its templates. A legal act
    /// ([`Operation::CreateBillingMigration`]); the migration's id is
    /// remembered as this WABA's and this tenant's, so that it alone reads
    /// and resumes it. Audited; never replayed.
    ///
    /// # Errors
    ///
    /// `403 feature_not_available` with the legal acts off; `422` on
    /// `currency` or `extended_credit_id`; Meta's refusal.
    pub async fn create_billing_migration(
        &self,
        currency: &str,
        extended_credit_id: Option<&str>,
    ) -> Result<BillingMigrationView, ServiceError> {
        gate_operation(Operation::CreateBillingMigration, self.legal_acts)?;
        let mut intent = PaymentMethodMigrationIntent::new(currency);
        if let Some(credit) = extended_credit_id {
            intent = intent.extended_credit_id({
                if !plain_id(credit) {
                    return Err(ServiceError::invalid("extended_credit_id"));
                }
                CreditLineId::new(credit)
            });
        }
        intent
            .validate()
            .map_err(|error| ServiceError::invalid(error.field))?;
        let started = match self
            .waba
            .client()
            .waba(self.waba_id().clone())
            .set_payment_method_migration_intent(&intent)
            .await
        {
            Ok(started) => started,
            Err(error) => {
                let api = self.failed(&error).await;
                audit_tenant(
                    BILLING_MIGRATION_STARTED,
                    self.caller,
                    self.waba_id(),
                    None,
                    outcome(&api),
                );
                return Err(api);
            }
        };
        audit_tenant(
            BILLING_MIGRATION_STARTED,
            self.caller,
            self.waba_id(),
            Some(&started.migration_id),
            Outcome::Answered,
        );
        // The migration exists at Meta now: a failure to remember it must
        // not invite a second one, so it says the request went through.
        if let Some(key) = migration_key(&started.migration_id) {
            self.signups
                .billing_migrations
                .put(
                    key,
                    &BillingMigration {
                        tenant_id: self.caller.tenant().as_str().to_owned(),
                        waba_id: self.waba_id().as_str().to_owned(),
                    },
                    Expiry::Never,
                )
                .await
                .map_err(|error| ServiceError::from(error).with_may_have_been_sent(true))?;
        }
        Ok(BillingMigrationView {
            id: started.migration_id,
            status: started.migration_status.as_str().to_owned(),
            destination_waba_id: None,
            destination_currency: None,
        })
    }

    /// The migration's id when it is this WABA's and this tenant's, else
    /// `404 not_found` and Meta is not asked.
    async fn own_migration<'i>(&self, id: &'i str) -> Result<&'i str, ServiceError> {
        let key = migration_key(id).ok_or_else(ServiceError::not_found)?;
        let record = self
            .signups
            .billing_migrations
            .get(key)
            .await?
            .map(|(record, ..)| record)
            .filter(|record| {
                record.tenant_id == self.caller.tenant().as_str()
                    && record.waba_id == self.waba_id().as_str()
            });
        record.map(|_| key).ok_or_else(ServiceError::not_found)
    }

    /// Where a billing migration of the WABA stands (`GET /{migration}`).
    ///
    /// # Errors
    ///
    /// `404 not_found` for a migration that is not this WABA's (Meta is not
    /// asked); Meta's refusal.
    pub async fn billing_migration(&self, id: &str) -> Result<BillingMigrationView, ServiceError> {
        let id = self.own_migration(id).await?;
        match self
            .waba
            .client()
            .payment_method_migration(id)
            .status()
            .await
        {
            Ok(state) => Ok(migration_view(state)),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Complete a billing migration of the WABA
    /// (`POST /{migration}/resume_migration`): the clone becomes the
    /// billing account of the numbers and the old WABA can no longer send.
    /// A legal act ([`Operation::ResumeBillingMigration`]). Audited; never
    /// replayed.
    ///
    /// # Errors
    ///
    /// `403 feature_not_available` with the legal acts off; `404
    /// not_found` for a migration that is not this WABA's (Meta is not
    /// asked); Meta's refusal.
    pub async fn resume_billing_migration(
        &self,
        id: &str,
    ) -> Result<BillingMigrationView, ServiceError> {
        gate_operation(Operation::ResumeBillingMigration, self.legal_acts)?;
        let id = self.own_migration(id).await?;
        match self
            .waba
            .client()
            .payment_method_migration(id)
            .resume_migration()
            .await
        {
            Ok(state) => {
                audit_tenant(
                    BILLING_MIGRATION_RESUMED,
                    self.caller,
                    self.waba_id(),
                    Some(id),
                    Outcome::Answered,
                );
                Ok(migration_view(state))
            }
            Err(error) => {
                let api = self.failed(&error).await;
                audit_tenant(
                    BILLING_MIGRATION_RESUMED,
                    self.caller,
                    self.waba_id(),
                    Some(id),
                    outcome(&api),
                );
                Err(api)
            }
        }
    }
}

fn migration_view(state: meta_whatsapp_rs::client::waba::MigrationState) -> BillingMigrationView {
    let (destination_waba_id, destination_currency) =
        state.destination_waba.map_or((None, None), |waba| {
            (Some(waba.id.as_str().to_owned()), waba.currency)
        });
    BillingMigrationView {
        id: state.id,
        status: state.status.as_str().to_owned(),
        destination_waba_id,
        destination_currency,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D250: four of the operator's thirteen actions are legal acts; the
    /// reads, the creation of a solution and the two "soft" deactivation
    /// calls are not. Decisive: an arm of [`is_legal_admin_act`] removed
    /// or added.
    #[test]
    fn four_of_the_operators_actions_are_legal_acts() {
        let gated: Vec<_> = PartnerAction::ALL
            .into_iter()
            .filter(|action| admin_act(*action).is_some_and(is_legal_admin_act))
            .collect();
        assert_eq!(
            gated,
            [
                PartnerAction::SubmitBusinessVerification,
                PartnerAction::AcceptSolution,
                PartnerAction::RejectSolution,
                PartnerAction::AcceptSolutionDeactivation,
            ]
        );
        for action in gated {
            assert_eq!(
                gate_partner(action, false).unwrap_err().code(),
                "feature_not_available"
            );
            assert!(gate_partner(action, true).is_ok());
        }
    }

    /// The stored name is pinned: it is in stored data (roadmap M5j; pending
    /// the owner's confirmation before the first release).
    #[test]
    fn the_billing_namespace_is_pinned() {
        assert_eq!(BILLING_NAMESPACE, "wa.server.billing_migration");
    }

    /// A migration's id in a path is bounded and plain.
    #[test]
    fn a_migration_key_is_plain_and_bounded() {
        assert_eq!(
            migration_key("mig_01HZYK3ABCDEF4567890"),
            Some("mig_01HZYK3ABCDEF4567890")
        );
        for bad in ["", "a b", "a/b", "../x", &"m".repeat(129)] {
            assert_eq!(migration_key(bad), None, "{bad}");
        }
    }
}
