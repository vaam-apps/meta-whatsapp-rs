//! Multi-Partner Solutions: create one, accept or reject it, deactivate it
//! (request, accept, reject), read it and its partners, list an app's
//! solutions and its connected client businesses, and read
//! an onboarded client's business token through a solution.
//!
//! Docs: `solution-providers/multi-partner-solutions` (the guide, with
//! the examples the tests copy), `solution-providers/multi-partner-solution-embedded-creation`
//! (creation by API) and the references `reference/whatsapp-business-solution/*`
//! (`solution-details-api`, `solution-accept-api`, `solution-reject-api`,
//! `send-deactivation-request-api`, `accept-deactivation-request-api`,
//! `reject-deactivation-request-api`, `access-token-api`),
//! `reference/application/solution-creation-api`,
//! `reference/application/application-solutions-api`,
//! and `reference/application/application-connected-client-businesses`.
//!
//! A WABA's solutions (`GET /{WABA_ID}/solutions`) are roadmap L10b's,
//! in [`crate::waba`]'s family, not here.
//!
//! | Meta path | Method |
//! | --- | --- |
//! | `POST /{APP_ID}/whatsapp_business_solution` | [`PartnerSolutions::create`] |
//! | `GET /{APP_ID}/whatsapp_business_solutions` | [`PartnerSolutions::app_solutions`], [`PartnerSolutions::app_solutions_stream`] |
//! | `GET /{APP_ID}/connected_client_businesses` | [`PartnerSolutions::connected_client_businesses`], [`PartnerSolutions::connected_client_businesses_stream`] |
//! | `GET /{SOLUTION_ID}` | [`PartnerSolutions::get`] |
//! | `GET /{SOLUTION_ID}/partners` | [`PartnerSolutions::partners`], [`PartnerSolutions::partners_stream`] |
//! | `GET /{SOLUTION_ID}/access_token?business_id=…` | [`PartnerSolutions::access_token`] |
//! | `POST /{SOLUTION_ID}/accept` | [`PartnerSolutions::accept`] |
//! | `POST /{SOLUTION_ID}/reject` | [`PartnerSolutions::reject`] |
//! | `POST /{SOLUTION_ID}/send_deactivation_request` | [`PartnerSolutions::send_deactivation_request`] |
//! | `POST /{SOLUTION_ID}/accept_deactivation_request` | [`PartnerSolutions::accept_deactivation_request`] |
//! | `POST /{SOLUTION_ID}/reject_deactivation_request` | [`PartnerSolutions::reject_deactivation_request`] |
//!
//! ```no_run
//! # async fn demo(client: meta_whatsapp_client::Client) -> meta_whatsapp_core::Result<()> {
//! use meta_whatsapp_core::ids::{BusinessId, SolutionId};
//! use meta_whatsapp_core::secret::AccessToken;
//!
//! let system = client.with_token(AccessToken::new("<SYSTEM_USER_TOKEN>"));
//! let solution = SolutionId::new("303610109049230")?;
//! // From the `account_update` webhook a client's onboarding triggered.
//! let customer = BusinessId::new("2729063490586005")?;
//! let token = system
//!     .partner_solutions()
//!     .access_token(&solution, &customer)
//!     .await?;
//! let business = client.with_token(token.access_token);
//! # let _ = business; Ok(()) }
//! ```
//!
//! # The solution token
//!
//! [`PartnerSolutions::access_token`] answers an onboarded client's
//! business integration system user access token: a bearer credential for
//! the client's assets, as [`AccessToken`] (whose `Debug` is redacted).
//! Use it with [`Client::with_token`] and keep it where you keep the
//! tokens Embedded Signup produces (the token vault). The reference says
//! the token is time-limited and gives its expiry as `expires_at` (unix
//! seconds, [`SolutionAccessToken::expires_at`]); the guide's example
//! carries no expiry: keep `expires_at` next to the token and read a new
//! one before it passes. A body that fails to decode never reaches the
//! error: neither the body nor serde's message, which would quote the
//! token; nor does a non-2xx body that is not Meta's error envelope.
//!
//! # Where Meta's pages disagree (decided here)
//!
//! - **The token's answer**: the guide's examples wrap it,
//!   `{"data": [{"access_token": …}]}`; the reference's schema is the bare
//!   object `{"access_token", "expires_at"}`. Both decode.
//! - **Deactivation answers**: the guide shows `{"success": true}` for
//!   accepting and rejecting a deactivation request, the references a
//!   solution object. Both decode: `None`, or the solution
//!   ([`PartnerSolutions::accept_deactivation_request`]).
//! - **Who calls what**: the guide says the invited partner accepts or
//!   rejects a solution, and the partner accepts or rejects a
//!   deactivation request the owner sent (its API subsections say "the
//!   solution owner" for all three deactivation calls); the reject
//!   reference says solution owners reject. Meta decides from the token;
//!   the methods do not check roles.
//! - **MESSAGING on creation**: the creation reference says exactly one of
//!   the two apps may have `MESSAGING`, the embedded creation guide's
//!   example gives it to both. [`CreateSolution`] checks only what both
//!   agree on: at least one side has it.
//! - **Versions**: the pages' examples use v20.0 and v26.0; the client's
//!   configured version is used for all of them.

use futures::Stream;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{AppId, BusinessId, SolutionId};
use meta_whatsapp_core::paging::Page;
use meta_whatsapp_core::secret::AccessToken;
use meta_whatsapp_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::phone_numbers::fields_param;
use crate::request::{paginate_or_error, reject_cursors, send_checked};
use crate::templates::macros::string_enum;
use crate::waba::WabaTask;
use crate::{Client, GraphRequest};

/// Entry point, see [`Client::partner_solutions`].
#[derive(Debug, Clone)]
pub struct PartnerSolutions {
    client: Client,
}

impl Client {
    /// Multi-Partner Solution operations, authenticated with this client's
    /// token (each method says which token Meta expects; see the
    /// [module docs](crate::partners::solutions)).
    pub fn partner_solutions(&self) -> PartnerSolutions {
        PartnerSolutions {
            client: self.clone(),
        }
    }
}

string_enum! {
    /// A solution's `status` (`WhatsAppBusinessSolutionStatus` in the
    /// references). The App Dashboard names `INITIATED` "Pending" and
    /// `REJECTED` "Inactive".
    pub enum SolutionStatus {
        /// Saved, not yet sent to the partner.
        Draft => "DRAFT",
        /// Sent; the partner has neither accepted nor rejected it.
        Initiated => "INITIATED",
        /// Accepted: usable to configure Embedded Signup.
        Active => "ACTIVE",
        /// The partner rejected it.
        Rejected => "REJECTED",
        /// Deactivated: no longer onboards clients.
        Deactivated => "DEACTIVATED",
    }
}

string_enum! {
    /// A solution's `status_for_pending_request`
    /// (`WhatsAppBusinessSolutionPendingStatus`): the transition waiting
    /// for the other partner.
    pub enum PendingRequestStatus {
        /// Waiting for the partner to accept the solution.
        PendingActivation => "PENDING_ACTIVATION",
        /// The owner asked to deactivate it; waiting for the partner.
        PendingDeactivation => "PENDING_DEACTIVATION",
        /// Nothing pending (`NONE`).
        NoRequest => "NONE",
    }
}

string_enum! {
    /// A partner's `status` in a solution (`GET /{SOLUTION_ID}/partners`).
    /// The guide's examples show `ACCEPTED`; the accept reference names
    /// `NOTIFICATION_SENT` as the state accepting leaves.
    pub enum PartnerStatus {
        /// Invited, not yet accepted.
        NotificationSent => "NOTIFICATION_SENT",
        /// Accepted.
        Accepted => "ACCEPTED",
    }
}

string_enum! {
    /// The app's relationship to a solution, the `role` filter of
    /// [`PartnerSolutions::app_solutions`].
    pub enum SolutionRole {
        /// Solutions the app owns.
        Owner => "OWNER",
        /// Solutions the app is a partner of.
        Partner => "PARTNER",
    }
}

/// An app, as a solution names it (`owner_app`, `partner_app`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SolutionApp {
    /// The app id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<AppId>,
    /// The app's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The app's link (`partner_app.link` in the guide's examples).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// A Multi-Partner Solution (`WhatsAppBusinessSolution`), as
/// `GET /{SOLUTION_ID}`, the lists and the deactivation answers give it.
/// Which fields are present depends on `fields`: Meta's defaults are
/// `name`, `status` and `status_for_pending_request`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SolutionInfo {
    /// The solution id.
    pub id: SolutionId,
    /// The solution's name, as the App Dashboard shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Current status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SolutionStatus>,
    /// The transition waiting for the other partner, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_for_pending_request: Option<PendingRequestStatus>,
    /// The app that owns the solution (`fields=owner_app`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_app: Option<SolutionApp>,
    /// The permissions onboarded clients grant the owner's app
    /// (`fields=owner_permissions`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_permissions: Option<Vec<WabaTask>>,
    /// The partners (`fields=partners{partner_permissions,partner_app}`,
    /// as the guide asks before accepting): one page of the edge
    /// [`PartnerSolutions::partners`] reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partners: Option<Page<SolutionPartner>>,
}

/// One partner of a solution (`GET /{SOLUTION_ID}/partners`, or the
/// `partners` field of [`SolutionInfo`]).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SolutionPartner {
    /// The partner record's id (not an app id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The partner's app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partner_app: Option<SolutionApp>,
    /// Whether the partner accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PartnerStatus>,
    /// The permissions onboarded clients grant this partner's app
    /// (`partners{partner_permissions}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partner_permissions: Option<Vec<WabaTask>>,
}

/// Body of `POST /{APP_ID}/whatsapp_business_solution`
/// (`WhatsAppBusinessSolutionCreateRequest`): the app in the path owns
/// the solution, `partner_app_id` is invited to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct CreateSolution {
    /// The solution's name, shown to both partners in the App Dashboard:
    /// make it one neither confuses with another solution. Not blank (the
    /// accept-deactivation reference gives 2 to 75 characters; not checked
    /// here).
    pub solution_name: String,
    /// The invited partner's app.
    pub partner_app_id: AppId,
    /// What onboarded clients grant the owner's app. The reference allows
    /// only `MESSAGING` here (or nothing).
    pub owner_permissions: Vec<WabaTask>,
    /// What onboarded clients grant the partner's app, as
    /// `owner_permissions`.
    pub partner_permissions: Vec<WabaTask>,
}

impl CreateSolution {
    /// A solution named `solution_name` with `partner_app_id`, with the
    /// two permission lists.
    pub fn new(
        solution_name: impl Into<String>,
        partner_app_id: AppId,
        owner_permissions: Vec<WabaTask>,
        partner_permissions: Vec<WabaTask>,
    ) -> Self {
        Self {
            solution_name: solution_name.into(),
            partner_app_id,
            owner_permissions,
            partner_permissions,
        }
    }

    /// Check what Meta's pages agree on: a name that is not blank, and
    /// `MESSAGING` on at least one side (the creation reference: "At least
    /// one partner must have MESSAGING permission").
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.solution_name.trim().is_empty() {
            return Err(ValidationError::new("solution_name", "required"));
        }
        let messaging = |tasks: &[WabaTask]| tasks.contains(&WabaTask::Messaging);
        if !messaging(&self.owner_permissions) && !messaging(&self.partner_permissions) {
            return Err(ValidationError::new(
                "owner_permissions",
                "MESSAGING must be granted to the owner or the partner",
            ));
        }
        Ok(())
    }
}

/// Answer of `POST /{APP_ID}/whatsapp_business_solution`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SolutionCreated {
    /// The new solution, `INITIATED` until the partner accepts it.
    pub solution_id: SolutionId,
}

/// An onboarded client's business token, from
/// `GET /{SOLUTION_ID}/access_token` (`WhatsAppBusinessSolutionAccessToken`).
/// `Debug` never shows the token.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct SolutionAccessToken {
    /// The client's business integration system user access token.
    pub access_token: AccessToken,
    /// When it expires, in unix seconds (the reference's `expires_at`;
    /// the guide's example has none). Meta's value as sent, not
    /// interpreted: `None` when absent or `null`; a value that is not an
    /// integer fails the whole answer, withheld as any other.
    #[serde(default)]
    pub expires_at: Option<i64>,
}

/// The two shapes Meta's pages give a deactivation answer: the guide's
/// `{"success": true}` (a `false` is refused before this is read), and the
/// references' solution object.
#[derive(Deserialize)]
#[serde(untagged)]
enum DeactivationAnswer {
    Success {
        #[serde(rename = "success")]
        _success: bool,
    },
    Solution(Box<SolutionInfo>),
}

/// Options of [`PartnerSolutions::app_solutions`]
/// (`reference/application/application-solutions-api`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListAppSolutions {
    /// Only the solutions the app owns, or only those it is a partner of;
    /// both when `None`.
    pub role: Option<SolutionRole>,
    /// Fields of each solution (empty = Meta's defaults: `name`, `status`,
    /// `status_for_pending_request`).
    pub fields: Vec<String>,
    /// Page size, 1 to 100 (Meta's default 25 when `None`).
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`
    /// ([`Page::next_cursor`]); for the one-page read only, the stream
    /// manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl ListAppSolutions {
    /// Every solution of the app, Meta's fields and page size.
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by the app's role.
    #[must_use]
    pub fn role(mut self, role: SolutionRole) -> Self {
        self.role = Some(role);
        self
    }

    /// Request these fields of each solution.
    #[must_use]
    pub fn fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Page size, 1 to 100.
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Continue after this cursor.
    #[must_use]
    pub fn after(mut self, cursor: impl Into<String>) -> Self {
        self.after = Some(cursor.into());
        self
    }

    /// Go back before this cursor.
    #[must_use]
    pub fn before(mut self, cursor: impl Into<String>) -> Self {
        self.before = Some(cursor.into());
        self
    }
}

/// Options of [`PartnerSolutions::connected_client_businesses`]
/// (`reference/application/application-connected-client-businesses`;
/// its only fields, `id` and `name`, are both defaults).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListConnectedClientBusinesses {
    /// Page size, 1 to 100 (Meta's default 25 when `None`).
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`; for the
    /// one-page read only.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl ListConnectedClientBusinesses {
    /// The first page, Meta's page size.
    pub fn new() -> Self {
        Self::default()
    }

    /// Page size, 1 to 100.
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Continue after this cursor.
    #[must_use]
    pub fn after(mut self, cursor: impl Into<String>) -> Self {
        self.after = Some(cursor.into());
        self
    }

    /// Go back before this cursor.
    #[must_use]
    pub fn before(mut self, cursor: impl Into<String>) -> Self {
        self.before = Some(cursor.into());
        self
    }
}

/// Options of [`PartnerSolutions::partners`]. The guide shows the edge's
/// `paging.cursors` and documents no page size, so only cursors.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListSolutionPartners {
    /// Cursor from a previous page's `paging.cursors.after`; for the
    /// one-page read only.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl ListSolutionPartners {
    /// The first page.
    pub fn new() -> Self {
        Self::default()
    }

    /// Continue after this cursor.
    #[must_use]
    pub fn after(mut self, cursor: impl Into<String>) -> Self {
        self.after = Some(cursor.into());
        self
    }

    /// Go back before this cursor.
    #[must_use]
    pub fn before(mut self, cursor: impl Into<String>) -> Self {
        self.before = Some(cursor.into());
        self
    }
}

/// A client business connected to an app
/// (`GET /{APP_ID}/connected_client_businesses`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ConnectedClientBusiness {
    /// The client's business portfolio id.
    pub id: BusinessId,
    /// The client business's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The decode-error body snippet of [`PartnerSolutions::access_token`]:
/// the answer is a credential.
pub(crate) const TOKEN_WITHHELD: &[u8] = TOKEN_WITHHELD_TEXT.as_bytes();

/// [`TOKEN_WITHHELD`] as text, the snippet of an [`Error::Http`].
const TOKEN_WITHHELD_TEXT: &str = "(withheld: the response carries a business token)";

/// The page size the list references allow.
fn check_limit(limit: Option<u32>) -> Result<()> {
    match limit {
        Some(n) if !(1..=100).contains(&n) => {
            Err(ValidationError::new("limit", "must be 1 to 100").into())
        }
        _ => Ok(()),
    }
}

fn joined(fields: &[String]) -> Option<String> {
    (!fields.is_empty()).then(|| fields.join(","))
}

/// The two shapes Meta's pages give the token's answer.
#[derive(Deserialize)]
#[serde(untagged)]
enum TokenAnswer {
    /// The guide's: `{"data": [{"access_token": …}]}`.
    Wrapped { data: Vec<SolutionAccessToken> },
    /// The reference's: `{"access_token": …, "expires_at": …}`.
    Bare(SolutionAccessToken),
}

/// A decode error for the token's answer: [`TOKEN_WITHHELD`] stands for
/// the body.
fn token_shape_error(why: &'static str) -> Error {
    Error::decode(
        "solution access token response",
        <serde_json::Error as serde::de::Error>::custom(why),
        TOKEN_WITHHELD,
    )
}

/// A non-2xx answer of the token's endpoint that is not Meta's error
/// envelope: its snippet becomes [`TOKEN_WITHHELD`], whatever the body
/// names. The crate's own rule (`safe_snippet`) withholds only bodies
/// naming `paging` or `access_token`; this endpoint's body is a
/// credential under any key. Meta's envelope (`Error::Api`) carries
/// Meta's message, not the body, and is kept.
fn withhold_http_body(error: Error) -> Error {
    match error {
        Error::Http { status, .. } => Error::Http {
            status,
            body_snippet: TOKEN_WITHHELD_TEXT.to_owned(),
        },
        other => other,
    }
}

impl PartnerSolutions {
    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `POST /{APP_ID}/whatsapp_business_solution`: create a solution
    /// owned by `owner_app`, inviting `solution.partner_app_id`. It starts
    /// `INITIATED`; the partner accepts it with [`Self::accept`].
    ///
    /// Token: in the embedded creation flow, the Tech Provider's user
    /// access token the flow returned (`manage_app_solution`), with the
    /// Tech Provider's app as `owner_app`. Checked locally first
    /// ([`CreateSolution::validate`]); never replayed, since a lost answer
    /// may have created the solution: list [`Self::app_solutions`] before
    /// creating again.
    pub async fn create(
        &self,
        owner_app: &AppId,
        solution: &CreateSolution,
    ) -> Result<SolutionCreated> {
        solution.validate()?;
        self.client
            .post_at(&[owner_app.as_str(), "whatsapp_business_solution"])
            .json(solution)
            .idempotent(false)
            .context("create solution response")
            .send()
            .await
    }

    /// `GET /{SOLUTION_ID}`: the solution's fields (`fields` empty =
    /// Meta's defaults). The guide asks for
    /// `name,owner_permissions,partners{partner_permissions,partner_app}`
    /// before accepting, since an accepted solution cannot be declined.
    pub async fn get(&self, solution: &SolutionId, fields: &[&str]) -> Result<SolutionInfo> {
        self.client
            .get_at(&[solution.as_str()])
            .query_opt("fields", fields_param(fields))
            .context("solution")
            .send()
            .await
    }

    /// `POST /{SOLUTION_ID}/accept`: accept the invitation, as the invited
    /// partner (in the embedded creation flow, the Solution Partner's
    /// system user token with `manage_app_solution`). The solution must be
    /// `INITIATED`. **Cannot be undone**: an accepted solution cannot be
    /// declined. Read it with [`Self::get`] first. Never replayed.
    pub async fn accept(&self, solution: &SolutionId) -> Result<()> {
        self.state_change(solution, "accept", "accept solution response")
            .await
    }

    /// `POST /{SOLUTION_ID}/reject`: decline the invitation (the guide: the
    /// invited partner; the reference: the solution owner). **Permanent**:
    /// the other partner has to send a new solution. Never replayed.
    pub async fn reject(&self, solution: &SolutionId) -> Result<()> {
        self.state_change(solution, "reject", "reject solution response")
            .await
    }

    /// `POST /{SOLUTION_ID}/send_deactivation_request`: as the solution
    /// owner, ask the partner to deactivate a solution (`ACTIVE` in the
    /// guide; the reference also allows `INITIATED`). It stays usable for
    /// onboarding until the partner accepts. Never replayed.
    pub async fn send_deactivation_request(&self, solution: &SolutionId) -> Result<()> {
        self.state_change(
            solution,
            "send_deactivation_request",
            "send deactivation request response",
        )
        .await
    }

    /// `POST /{SOLUTION_ID}/accept_deactivation_request`: accept a pending
    /// deactivation request (the guide's prose: the partner the owner asked;
    /// its API subsection: the owner). The solution becomes `DEACTIVATED`
    /// and no longer onboards clients; the reference requires outstanding
    /// payments to be settled first. Never replayed.
    ///
    /// `None` when Meta answered `{"success": true}` (the guide), the
    /// solution afterwards when it answered with it (the reference).
    pub async fn accept_deactivation_request(
        &self,
        solution: &SolutionId,
    ) -> Result<Option<SolutionInfo>> {
        self.deactivation_answer(
            solution,
            "accept_deactivation_request",
            "accept deactivation request response",
        )
        .await
    }

    /// `POST /{SOLUTION_ID}/reject_deactivation_request`: reject a pending
    /// deactivation request; the solution stays `ACTIVE`. Never replayed.
    /// Answers as [`Self::accept_deactivation_request`].
    pub async fn reject_deactivation_request(
        &self,
        solution: &SolutionId,
    ) -> Result<Option<SolutionInfo>> {
        self.deactivation_answer(
            solution,
            "reject_deactivation_request",
            "reject deactivation request response",
        )
        .await
    }

    async fn state_change(
        &self,
        solution: &SolutionId,
        edge: &'static str,
        context: &'static str,
    ) -> Result<()> {
        self.client
            .post_at(&[solution.as_str(), edge])
            .idempotent(false)
            .context(context)
            .send_success()
            .await
    }

    async fn deactivation_answer(
        &self,
        solution: &SolutionId,
        edge: &'static str,
        context: &'static str,
    ) -> Result<Option<SolutionInfo>> {
        let request = self
            .client
            .post_at(&[solution.as_str(), edge])
            .idempotent(false);
        Ok(match send_checked(request, context).await? {
            DeactivationAnswer::Success { .. } => None,
            DeactivationAnswer::Solution(solution) => Some(*solution),
        })
    }

    /// `GET /{SOLUTION_ID}/access_token?business_id=…`: the business token
    /// of `business`, a client onboarded through the solution (its
    /// business portfolio id is in the `account_update` webhook of its
    /// onboarding). Token: your system user token.
    ///
    /// The answer is a credential: see the
    /// [module docs](crate::partners::solutions#the-solution-token). A
    /// body that does not decode, or holds no token (an empty `data`, a
    /// blank `access_token`), is an [`Error::Decode`] that carries neither
    /// the body nor serde's message; a non-2xx answer that is not Meta's
    /// error envelope is an [`Error::Http`] whose snippet is a placeholder,
    /// never the body. A read: retried as any GET is.
    pub async fn access_token(
        &self,
        solution: &SolutionId,
        business: &BusinessId,
    ) -> Result<SolutionAccessToken> {
        let answer: TokenAnswer = self
            .client
            .get_at(&[solution.as_str(), "access_token"])
            .query("business_id", business.as_str())
            .context("solution access token response")
            .send_withheld(TOKEN_WITHHELD)
            .await
            .map_err(withhold_http_body)?;
        let token = match answer {
            TokenAnswer::Wrapped { data } => data
                .into_iter()
                .next()
                .ok_or_else(|| token_shape_error("no token in `data`"))?,
            TokenAnswer::Bare(token) => token,
        };
        if token.access_token.expose_secret().trim().is_empty() {
            return Err(token_shape_error("empty access_token"));
        }
        Ok(token)
    }

    fn partners_request(&self, solution: &SolutionId) -> GraphRequest {
        self.client
            .get_at(&[solution.as_str(), "partners"])
            .context("solution partners")
    }

    /// `GET /{SOLUTION_ID}/partners`, one page. The next page: the same
    /// query with `after` set to this page's [`Page::next_cursor`].
    pub async fn partners(
        &self,
        solution: &SolutionId,
        query: &ListSolutionPartners,
    ) -> Result<Page<SolutionPartner>> {
        self.partners_request(solution)
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every partner of the solution, following cursors. A query with
    /// `after` or `before` set is refused (the stream's single item is
    /// that validation error).
    pub fn partners_stream(
        &self,
        solution: &SolutionId,
        query: &ListSolutionPartners,
    ) -> impl Stream<Item = Result<SolutionPartner>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .map(|()| self.partners_request(solution)),
        )
    }

    fn app_solutions_request(&self, app: &AppId, query: &ListAppSolutions) -> Result<GraphRequest> {
        check_limit(query.limit)?;
        Ok(self
            .client
            .get_at(&[app.as_str(), "whatsapp_business_solutions"])
            .query_opt("role", query.role.as_ref())
            .query_opt("fields", joined(&query.fields))
            .query_opt("limit", query.limit)
            .context("app solutions"))
    }

    /// `GET /{APP_ID}/whatsapp_business_solutions`, one page: the
    /// solutions `app` owns or is a partner of.
    pub async fn app_solutions(
        &self,
        app: &AppId,
        query: &ListAppSolutions,
    ) -> Result<Page<SolutionInfo>> {
        self.app_solutions_request(app, query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every solution of `app`, following cursors (a query with a cursor is
    /// refused, as its single item).
    pub fn app_solutions_stream(
        &self,
        app: &AppId,
        query: &ListAppSolutions,
    ) -> impl Stream<Item = Result<SolutionInfo>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.app_solutions_request(app, query)),
        )
    }

    fn connected_request(
        &self,
        app: &AppId,
        query: &ListConnectedClientBusinesses,
    ) -> Result<GraphRequest> {
        check_limit(query.limit)?;
        Ok(self
            .client
            .get_at(&[app.as_str(), "connected_client_businesses"])
            .query_opt("limit", query.limit)
            .context("connected client businesses"))
    }

    /// `GET /{APP_ID}/connected_client_businesses`, one page: the client
    /// businesses connected to `app`.
    pub async fn connected_client_businesses(
        &self,
        app: &AppId,
        query: &ListConnectedClientBusinesses,
    ) -> Result<Page<ConnectedClientBusiness>> {
        self.connected_request(app, query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every client business connected to `app`, following cursors (a
    /// query with a cursor is refused, as its single item).
    pub fn connected_client_businesses_stream(
        &self,
        app: &AppId,
        query: &ListConnectedClientBusinesses,
    ) -> impl Stream<Item = Result<ConnectedClientBusiness>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.connected_request(app, query)),
        )
    }
}
