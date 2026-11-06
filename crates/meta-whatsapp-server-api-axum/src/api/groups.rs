//! Groups (scope `groups`; docs/design/server.md, section 4.2; roadmap
//! M5f): a number's groups, their settings, invite links, participants,
//! join requests and analytics.
//!
//! | Route | Graph call, with the tenant's WABA token |
//! | --- | --- |
//! | `POST /v1/numbers/{pn}/groups` | `POST /{pn}/groups` (asynchronous: `202` and Meta's `request_id`; the group's id comes in a `group_lifecycle_update` event) |
//! | `GET /v1/numbers/{pn}/groups` | `GET /{pn}/groups` (the groups listed are recorded as the number's) |
//! | `GET`, `PATCH`, `DELETE /v1/numbers/{pn}/groups/{group_id}` | `GET`, `POST`, `DELETE /{group_id}` |
//! | `GET`, `POST /v1/numbers/{pn}/groups/{group_id}/invite-link` | `GET`, `POST /{group_id}/invite_link` |
//! | `GET /v1/numbers/{pn}/groups/{group_id}/participants` | `GET /{group_id}?fields=participants,total_participant_count` |
//! | `POST /v1/numbers/{pn}/groups/{group_id}/participants/remove` | `DELETE /{group_id}/participants` (a body on a `DELETE`: the service takes a `POST`) |
//! | `GET /v1/numbers/{pn}/groups/{group_id}/join-requests` | `GET /{group_id}/join_requests` |
//! | `POST /v1/numbers/{pn}/groups/{group_id}/join-requests/approve`, `…/reject` | `POST`, `DELETE /{group_id}/join_requests` |
//! | `GET /v1/numbers/{pn}/groups/{group_id}/analytics` | `GET /{waba_id}/group_analytics` for the one group |
//!
//! **A group is the number's, or it does not exist** (the design's M5.2,
//! D150): the [`OwnedGroup`] extractor looks the group up in the service's
//! records of the number's groups *before the tenant's token is opened*, so
//! another tenant's group, another number's, an unrecorded one and an id no
//! group has are all `404 not_found`, with the vault unread and Meta not
//! asked (the core's [`meta_whatsapp_server_core::groups`] says how the
//! records are kept). Creating a group takes an `Idempotency-Key`.
//!
//! Nothing of a participant (a phone number, a BSUID, a username, a join
//! request's id) reaches a log, an error or an audit event; the changes are
//! audited by count and outcome. Meta's texts are answered nowhere here:
//! Meta's code and the service's kind for it only.

use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::State;
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::groups::{self as domain, Groups, Person};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::ToSchema;

use super::common::{ApiJson, rfc3339};
use super::templates::ApiQuery;
use crate::auth::{Caller, OwnedGroup, OwnedNumber};
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

// ─── Bodies ──────────────────────────────────────────────────────────────

/// A group to create.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = CreateGroupRequest)]
pub struct CreateBody {
    /// Its subject: 1 to 128 characters (Meta trims the whitespace).
    pub subject: String,
    /// Its description, at most 2,048 characters.
    pub description: Option<String>,
    /// `approval_required` (people who open the invite link ask to join,
    /// and you approve or reject them) or `auto_approve` (Meta's default).
    pub join_approval_mode: Option<String>,
}

/// What to change in a group: at least one field.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = UpdateGroupRequest)]
pub struct UpdateBody {
    /// A new subject: 1 to 128 characters.
    pub subject: Option<String>,
    /// A new description, at most 2,048 characters (empty clears it).
    pub description: Option<String>,
}

/// A participant to remove: exactly one of the three.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = GroupParticipantRef)]
pub struct PersonBody {
    /// A phone number in E.164 **with** its `+` (`+16505551234`).
    pub phone: Option<String>,
    /// A WhatsApp user id as the participants route lists it: digits, no
    /// `+` (`16505551234`).
    pub wa_id: Option<String>,
    /// A business-scoped user id (`US.13491208655302741918`); a parent
    /// BSUID (`US.ENT.…`) is refused.
    pub user_id: Option<String>,
}

/// The participants to remove. No `Debug`: it names people.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = RemoveGroupParticipantsRequest)]
pub struct RemoveBody {
    /// 1 to 8 participants (Meta's limit a request).
    #[schema(min_items = 1, max_items = 8)]
    pub users: Vec<PersonBody>,
}

/// The join requests to approve or reject. No `Debug`: the ids embed a
/// number.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = GroupJoinRequestsRequest)]
pub struct JoinBody {
    /// 1 to 100 join request ids, as the join requests route lists them.
    #[schema(min_items = 1, max_items = 100)]
    pub join_request_ids: Vec<String>,
}

// ─── Queries ─────────────────────────────────────────────────────────────

/// The listing's query.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    /// Page size, 1 to 1,024 (Meta's default is 25).
    #[param(minimum = 1, maximum = 1024)]
    pub limit: Option<u32>,
    /// `next_after` of the previous page.
    pub after: Option<String>,
    /// `previous_before` of the next page.
    pub before: Option<String>,
}

/// The cursors of a page of join requests.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CursorQuery {
    /// `next_after` of the previous page.
    pub after: Option<String>,
    /// `previous_before` of the next page.
    pub before: Option<String>,
}

/// The fields of a group to read.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct FieldsQuery {
    /// A comma-separated list of `subject`, `description`,
    /// `join_approval_mode`, `suspended`, `creation_timestamp`,
    /// `total_participant_count`; all of them without it. The participants
    /// have a route of their own.
    pub fields: Option<String>,
}

/// An analytics query.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsQuery {
    /// Start of the range, RFC 3339: at most 90 days ago.
    pub start: String,
    /// End of the range, RFC 3339: after `start`.
    pub end: String,
    /// A comma-separated list of `SENT`, `DELIVERED`, `READ`,
    /// `PARTICIPANTS_JOINED`, `PARTICIPANTS_LEFT`.
    pub metrics: String,
    /// `next_after` of the previous page.
    pub after: Option<String>,
}

// ─── Answers ─────────────────────────────────────────────────────────────

/// A group asked of Meta.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = GroupCreationRequested)]
pub struct CreatedView {
    /// Meta's id of the creation request; the `group_lifecycle_update`
    /// event that reports the group (its id and invite link) echoes it.
    #[schema(required = true)]
    pub request_id: Option<String>,
}

/// A group of the listing.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = GroupSummary)]
pub struct SummaryView {
    /// Its id.
    pub id: String,
    /// Its subject.
    #[schema(required = true)]
    pub subject: Option<String>,
    /// When it was created (RFC 3339).
    #[schema(required = true, format = DateTime)]
    pub created_at: Option<String>,
}

/// A page of the number's groups.
#[derive(Debug, Serialize, ToSchema)]
pub struct GroupList {
    /// The groups.
    pub data: Vec<SummaryView>,
    /// Pass as `after` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_after: Option<String>,
    /// Pass as `before` for the page before.
    #[schema(required = true)]
    pub previous_before: Option<String>,
}

/// A group, as Meta describes it.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Group)]
pub struct DetailsView {
    /// Its id.
    #[schema(required = true)]
    pub id: Option<String>,
    /// Its subject.
    #[schema(required = true)]
    pub subject: Option<String>,
    /// Its description.
    #[schema(required = true)]
    pub description: Option<String>,
    /// `approval_required` or `auto_approve`.
    #[schema(required = true)]
    pub join_approval_mode: Option<String>,
    /// Whether WhatsApp suspended it.
    #[schema(required = true)]
    pub suspended: Option<bool>,
    /// When it was created (RFC 3339).
    #[schema(required = true, format = DateTime)]
    pub creation_timestamp: Option<String>,
    /// How many participants it has, the business excluded.
    #[schema(required = true)]
    pub total_participant_count: Option<u64>,
}

/// A participant of a group.
#[derive(Serialize, ToSchema)]
#[schema(as = GroupParticipant)]
pub struct ParticipantView {
    /// Their phone number as WhatsApp knows it (absent when they have a
    /// username).
    #[schema(required = true)]
    pub wa_id: Option<String>,
    /// Their BSUID.
    #[schema(required = true)]
    pub user_id: Option<String>,
    /// Their parent BSUID, when the portfolio is enrolled for them.
    #[schema(required = true)]
    pub parent_user_id: Option<String>,
    /// Their username.
    #[schema(required = true)]
    pub username: Option<String>,
}

/// A group's participants.
#[derive(Serialize, ToSchema)]
#[schema(as = GroupParticipants)]
pub struct ParticipantsView {
    /// The participants, the business excluded.
    pub participants: Vec<ParticipantView>,
    /// Their count, as Meta says it.
    #[schema(required = true)]
    pub total_participant_count: Option<u64>,
}

/// What removing participants came to.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = GroupParticipantsRemoval)]
pub struct RemovalView {
    /// `true` when Meta says some were not removed (its `206`, code
    /// `131201`); the `group_participants_update` event says which.
    pub partial: bool,
}

/// A group's invite link: a credential, whoever holds it can join.
#[derive(Serialize, ToSchema)]
#[schema(as = GroupInviteLink)]
pub struct InviteLinkView {
    /// `https://chat.whatsapp.com/<code>`.
    pub invite_link: String,
}

/// An open join request.
#[derive(Serialize, ToSchema)]
#[schema(as = GroupJoinRequest)]
pub struct JoinRequestView {
    /// Its id: what approving or rejecting takes.
    pub join_request_id: String,
    /// The person's phone number (absent when they have a username).
    #[schema(required = true)]
    pub wa_id: Option<String>,
    /// Their BSUID.
    #[schema(required = true)]
    pub user_id: Option<String>,
    /// Their parent BSUID.
    #[schema(required = true)]
    pub parent_user_id: Option<String>,
    /// Their username.
    #[schema(required = true)]
    pub username: Option<String>,
    /// When they asked (RFC 3339).
    #[schema(required = true, format = DateTime)]
    pub created_at: Option<String>,
}

/// A page of a group's open join requests.
#[derive(Serialize, ToSchema)]
#[schema(as = GroupJoinRequestList)]
pub struct JoinRequestList {
    /// The requests.
    pub data: Vec<JoinRequestView>,
    /// Pass as `after` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_after: Option<String>,
    /// Pass as `before` for the page before.
    #[schema(required = true)]
    pub previous_before: Option<String>,
}

/// Why Meta refused: its code and the service's kind, never its text.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = GroupReason)]
pub struct ReasonView {
    /// Meta's error code.
    pub code: i64,
    /// The service's error code for it, to branch on (`ErrorCode`'s).
    pub kind: String,
}

impl From<domain::Reason> for ReasonView {
    fn from(reason: domain::Reason) -> Self {
        Self {
            code: reason.code,
            kind: reason.kind.to_owned(),
        }
    }
}

/// A join request Meta did not approve or reject.
#[derive(Serialize, ToSchema)]
#[schema(as = GroupJoinRequestFailure)]
pub struct FailedJoinView {
    /// Its id.
    #[schema(required = true)]
    pub join_request_id: Option<String>,
    /// Why: Meta's codes, never its texts.
    pub reasons: Vec<ReasonView>,
}

/// What approving or rejecting join requests came to.
#[derive(Serialize, ToSchema)]
#[schema(as = GroupJoinRequestsResult)]
pub struct JoinOutcomeView {
    /// The requests Meta approved (or rejected).
    pub done: Vec<String>,
    /// The ones it did not (a partial success is still a `200`).
    pub failed: Vec<FailedJoinView>,
    /// Meta's errors on the call as a whole: codes, never texts.
    pub reasons: Vec<ReasonView>,
}

impl From<domain::JoinOutcome> for JoinOutcomeView {
    fn from(outcome: domain::JoinOutcome) -> Self {
        Self {
            done: outcome.done,
            failed: outcome
                .failed
                .into_iter()
                .map(|f| FailedJoinView {
                    join_request_id: f.join_request_id,
                    reasons: f.reasons.into_iter().map(ReasonView::from).collect(),
                })
                .collect(),
            reasons: outcome.reasons.into_iter().map(ReasonView::from).collect(),
        }
    }
}

/// A day of a group's analytics.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = GroupAnalyticsDay)]
pub struct DayView {
    /// The day's start (RFC 3339).
    #[schema(format = DateTime)]
    pub start: String,
    /// The day's end (RFC 3339).
    #[schema(format = DateTime)]
    pub end: String,
    /// Messages the business sent (`SENT`).
    #[schema(required = true)]
    pub sent: Option<u64>,
    /// Deliveries to participants (`DELIVERED`).
    #[schema(required = true)]
    pub delivered: Option<u64>,
    /// Reads by participants (`READ`).
    #[schema(required = true)]
    pub read: Option<u64>,
    /// Participants who joined (`PARTICIPANTS_JOINED`).
    #[schema(required = true)]
    pub joined: Option<u64>,
    /// Participants who left (`PARTICIPANTS_LEFT`).
    #[schema(required = true)]
    pub left: Option<u64>,
}

/// A page of a group's analytics, day by day.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = GroupAnalytics)]
pub struct AnalyticsView {
    /// The days, in Meta's order; a metric not asked for is `null`.
    pub data: Vec<DayView>,
    /// Pass as `after` for the next page; `null` on the last one.
    #[schema(required = true)]
    pub next_after: Option<String>,
}

// ─── Helpers ─────────────────────────────────────────────────────────────

fn groups<'a>(
    state: &'a Grant,
    caller: &'a Caller,
    number: &'a OwnedNumber,
) -> Result<Groups<'a>, ApiError> {
    Ok(Groups::new(
        state.authz(),
        caller,
        number.capability(state),
        state.group_records(),
        state.clock(),
    )?
    .observed_by(state.metrics()))
}

/// A cursor Meta issued, passed back: visible ASCII, at most 1,024
/// characters (the library encodes it into the query).
fn cursor(field: &'static str, value: Option<String>) -> Result<Option<String>, ApiError> {
    match value {
        Some(v) if v.is_empty() || v.len() > 1024 || !v.bytes().all(|b| b.is_ascii_graphic()) => {
            Err(ApiError::invalid(field))
        }
        other => Ok(other),
    }
}

fn instant(field: &'static str, value: &str) -> Result<OffsetDateTime, ApiError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| ApiError::invalid(field))
}

fn when(at: Option<OffsetDateTime>) -> Option<String> {
    at.map(rfc3339)
}

impl From<PersonBody> for Person {
    fn from(person: PersonBody) -> Self {
        Self {
            phone: person.phone,
            wa_id: person.wa_id,
            user_id: person.user_id,
        }
    }
}

/// Ask for the group of a creation: `202` with Meta's request id.
async fn create(
    service: &Groups<'_>,
    new: &meta_whatsapp_rs::client::groups::CreateGroup,
) -> Result<Success, ApiError> {
    let created = service.create(new).await?;
    Success::json(
        StatusCode::ACCEPTED,
        &CreatedView {
            request_id: created.request_id,
        },
    )
}

// ─── Routes ──────────────────────────────────────────────────────────────

/// `POST /v1/numbers/{pn}/groups`: ask Meta to create a group.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/groups",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never create two groups"),
    ),
    request_body = CreateBody,
    responses(
        (status = 202, description = "Asked: Meta creates the group asynchronously. Its id and invite link come in a `group_lifecycle_update` event (type `group_create`, with this `request_id`), which also makes the group the number's here", body = CreatedView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, `idempotency_in_progress`, `outcome_unknown`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `subject`, `description`, `join_approval_mode` or `body`; `idempotency_key_reused`: nothing sent", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the group may have been created", body = ErrorBody),
    )
)]
pub(crate) async fn create_group(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let fingerprint = Fingerprint::create_group(owned.phone_number_id(), &body);
    let Ok(CreateBody {
        subject,
        description,
        join_approval_mode,
    }) = serde_json::from_value::<CreateBody>(body)
    else {
        return ApiError::invalid("body").into_response();
    };
    let new = match domain::new_group(&subject, description, join_approval_mode.as_deref()) {
        Ok(new) => new,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let service = match groups(&state, &caller, &owned) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };
    state
        .idempotent(&caller, key, fingerprint, create(&service, &new))
        .await
}

/// `GET /v1/numbers/{pn}/groups`: a page of the number's groups.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/groups",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ListQuery,
    ),
    responses(
        (status = 200, description = "A page of the number's active groups; each is recorded as the number's, so the routes on a group id find it (the way to adopt a group created before the service, or whose creation event was missed)", body = GroupList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `limit`, `after`, `before` or `query`; `invalid_cursor` (Meta's cursor expired: start again)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_groups(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedNumber,
    ApiQuery(query): ApiQuery<ListQuery>,
) -> Result<Json<GroupList>, ApiError> {
    let after = cursor("after", query.after)?;
    let before = cursor("before", query.before)?;
    let page = groups(&state, &caller, &owned)?
        .list(query.limit, after, before)
        .await?;
    Ok(Json(GroupList {
        data: page
            .groups
            .into_iter()
            .map(|g| SummaryView {
                id: g.id,
                subject: g.subject,
                created_at: when(g.created_at),
            })
            .collect(),
        next_after: page.next_after,
        previous_before: page.previous_before,
    }))
}

/// `GET /v1/numbers/{pn}/groups/{group_id}`: one group.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/groups/{group_id}",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        FieldsQuery,
    ),
    responses(
        (status = 200, description = "The group", body = DetailsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's (another tenant's looks like a missing one: decided before the token is opened, Meta not asked); or Meta no longer has it", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `fields` or `query`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_group(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
    ApiQuery(query): ApiQuery<FieldsQuery>,
) -> Result<Json<DetailsView>, ApiError> {
    let fields = domain::group_fields(query.fields.as_deref())?;
    let details = groups(&state, &caller, owned.number())?
        .get(owned.capability(&state), &fields)
        .await?;
    Ok(Json(DetailsView {
        id: details.id,
        subject: details.subject,
        description: details.description,
        join_approval_mode: details.join_approval_mode,
        suspended: details.suspended,
        creation_timestamp: when(details.creation_timestamp),
        total_participant_count: details.total_participant_count,
    }))
}

/// `PATCH /v1/numbers/{pn}/groups/{group_id}`: change a group's subject
/// and/or description.
#[utoipa::path(
    patch,
    path = "/v1/numbers/{pn}/groups/{group_id}",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = UpdateBody,
    responses(
        (status = 204, description = "Asked: Meta tells the outcome of each field in a `group_settings_update` event"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's; nothing changed", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `update` (nothing to change), `subject`, `description` or `body`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the group may have been changed", body = ErrorBody),
    )
)]
pub(crate) async fn update_group(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
    ApiJson(body): ApiJson<UpdateBody>,
) -> Result<StatusCode, ApiError> {
    let update = domain::settings_update(body.subject, body.description)?;
    groups(&state, &caller, owned.number())?
        .update(owned.capability(&state), &update)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/numbers/{pn}/groups/{group_id}`: delete a group, removing
/// every participant and the business.
#[utoipa::path(
    delete,
    path = "/v1/numbers/{pn}/groups/{group_id}",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Asked: the group's `group_lifecycle_update` event (type `group_delete`) tells whether it is gone, and forgets it here. There is no call to leave a group other than this one"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's; nothing deleted", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the group may have been deleted", body = ErrorBody),
    )
)]
pub(crate) async fn delete_group(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
) -> Result<StatusCode, ApiError> {
    groups(&state, &caller, owned.number())?
        .delete(owned.capability(&state))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/numbers/{pn}/groups/{group_id}/invite-link`: the group's
/// invite link.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/groups/{group_id}/invite-link",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The invite link", body = InviteLinkView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_group_invite_link(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
) -> Result<Json<InviteLinkView>, ApiError> {
    let link = groups(&state, &caller, owned.number())?
        .invite_link(owned.capability(&state))
        .await?;
    Ok(Json(InviteLinkView {
        invite_link: link.invite_link,
    }))
}

/// `POST /v1/numbers/{pn}/groups/{group_id}/invite-link`: replace the
/// invite link; every previous link stops working.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/groups/{group_id}/invite-link",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The new invite link", body = InviteLinkView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's; nothing changed", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: the link may have been replaced; read it again", body = ErrorBody),
    )
)]
pub(crate) async fn reset_group_invite_link(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
) -> Result<Json<InviteLinkView>, ApiError> {
    let link = groups(&state, &caller, owned.number())?
        .reset_invite_link(owned.capability(&state))
        .await?;
    Ok(Json(InviteLinkView {
        invite_link: link.invite_link,
    }))
}

/// `GET /v1/numbers/{pn}/groups/{group_id}/participants`: the group's
/// participants.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/groups/{group_id}/participants",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The participants, the business excluded", body = ParticipantsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_group_participants(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
) -> Result<Json<ParticipantsView>, ApiError> {
    let listed = groups(&state, &caller, owned.number())?
        .participants(owned.capability(&state))
        .await?;
    Ok(Json(ParticipantsView {
        participants: listed
            .participants
            .into_iter()
            .map(|p| ParticipantView {
                wa_id: p.wa_id,
                user_id: p.user_id,
                parent_user_id: p.parent_user_id,
                username: p.username,
            })
            .collect(),
        total_participant_count: listed.total_participant_count,
    }))
}

/// `POST /v1/numbers/{pn}/groups/{group_id}/participants/remove`: remove
/// participants, who can no longer join through an invite link.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/groups/{group_id}/participants/remove",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = RemoveBody,
    responses(
        (status = 200, description = "Asked: Meta tells each participant's outcome in a `group_participants_update` event; `partial` says it did not remove everyone", body = RemovalView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's; nothing removed", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `users`, `users[i]`, `users[i].phone`, `users[i].wa_id`, `users[i].user_id` or `body`: never quoted; `group_participant_not_found`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: they may have been removed", body = ErrorBody),
    )
)]
pub(crate) async fn remove_group_participants(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
    ApiJson(body): ApiJson<RemoveBody>,
) -> Result<Json<RemovalView>, ApiError> {
    let users = domain::people(body.users.into_iter().map(Person::from).collect())?;
    let removal = groups(&state, &caller, owned.number())?
        .remove_participants(owned.capability(&state), &users)
        .await?;
    Ok(Json(RemovalView {
        partial: removal.partial,
    }))
}

/// `GET /v1/numbers/{pn}/groups/{group_id}/join-requests`: a page of the
/// group's open join requests.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/groups/{group_id}/join-requests",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        CursorQuery,
    ),
    responses(
        (status = 200, description = "A page of open join requests", body = JoinRequestList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `after`, `before` or `query`; `invalid_cursor` (Meta's cursor expired: start again)", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn list_group_join_requests(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
    ApiQuery(query): ApiQuery<CursorQuery>,
) -> Result<Json<JoinRequestList>, ApiError> {
    let after = cursor("after", query.after)?;
    let before = cursor("before", query.before)?;
    let page = groups(&state, &caller, owned.number())?
        .join_requests(owned.capability(&state), after, before)
        .await?;
    Ok(Json(JoinRequestList {
        data: page
            .requests
            .into_iter()
            .map(|r| JoinRequestView {
                join_request_id: r.join_request_id,
                wa_id: r.wa_id,
                user_id: r.user_id,
                parent_user_id: r.parent_user_id,
                username: r.username,
                created_at: when(r.created_at),
            })
            .collect(),
        next_after: page.next_after,
        previous_before: page.previous_before,
    }))
}

/// `POST /v1/numbers/{pn}/groups/{group_id}/join-requests/approve`:
/// approve join requests.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/groups/{group_id}/join-requests/approve",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = JoinBody,
    responses(
        (status = 200, description = "Request by request: which Meta approved and which it did not, and why (a partial success is still a `200`)", body = JoinOutcomeView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's; nothing approved", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `join_request_ids`, `join_request_ids[i]` or `body`: never quoted; `join_request_not_found`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: they may have been approved", body = ErrorBody),
    )
)]
pub(crate) async fn approve_group_join_requests(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
    ApiJson(body): ApiJson<JoinBody>,
) -> Result<Json<JoinOutcomeView>, ApiError> {
    let ids = domain::join_request_ids(body.join_request_ids)?;
    let outcome = groups(&state, &caller, owned.number())?
        .approve_join_requests(owned.capability(&state), &ids)
        .await?;
    Ok(Json(JoinOutcomeView::from(outcome)))
}

/// `POST /v1/numbers/{pn}/groups/{group_id}/join-requests/reject`: reject
/// join requests.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/groups/{group_id}/join-requests/reject",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = JoinBody,
    responses(
        (status = 200, description = "Request by request: which Meta rejected and which it did not, and why (a partial success is still a `200`)", body = JoinOutcomeView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's; nothing rejected", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `join_request_ids`, `join_request_ids[i]` or `body`: never quoted; `join_request_not_found`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`: they may have been rejected", body = ErrorBody),
    )
)]
pub(crate) async fn reject_group_join_requests(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
    ApiJson(body): ApiJson<JoinBody>,
) -> Result<Json<JoinOutcomeView>, ApiError> {
    let ids = domain::join_request_ids(body.join_request_ids)?;
    let outcome = groups(&state, &caller, owned.number())?
        .reject_join_requests(owned.capability(&state), &ids)
        .await?;
    Ok(Json(JoinOutcomeView::from(outcome)))
}

/// `GET /v1/numbers/{pn}/groups/{group_id}/analytics`: a group's analytics,
/// day by day.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/groups/{group_id}/analytics",
    tag = "groups",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("group_id" = String, Path, description = "Group id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        AnalyticsQuery,
    ),
    responses(
        (status = 200, description = "A page of the group's days (Meta: daily granularity, a look-back of up to 90 days). Asked of the number's WABA, for this one group", body = AnalyticsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`, or Meta's refusal (`group_suspended`, `feature_not_available`)", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant, or no such group among the number's", body = ErrorBody),
        (status = 409, description = "`number_not_connected`, `reconnect_required`, or Meta's refusal (`group_limit_reached`, `group_creation_disabled`, `group_partially_applied`, `recipient_terms_not_accepted`)", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `start` (not RFC 3339, or more than 90 days ago), `end` (not after `start`), `metrics`, `after` or `query`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`, or Meta's throttling (`rate_limited`, `group_rate_limited`)", body = ErrorBody),
        (status = 502, description = "Meta failed", body = ErrorBody),
        (status = 504, description = "`timeout`", body = ErrorBody),
    )
)]
pub(crate) async fn get_group_analytics(
    State(state): State<Grant>,
    caller: Caller,
    owned: OwnedGroup,
    ApiQuery(query): ApiQuery<AnalyticsQuery>,
) -> Result<Json<AnalyticsView>, ApiError> {
    let start = instant("start", &query.start)?;
    let end = instant("end", &query.end)?;
    let after = cursor("after", query.after)?;
    let page = groups(&state, &caller, owned.number())?
        .analytics(owned.capability(&state), start, end, &query.metrics, after)
        .await?;
    Ok(Json(AnalyticsView {
        data: page
            .days
            .into_iter()
            .map(|d| DayView {
                start: rfc3339(d.start),
                end: rfc3339(d.end),
                sent: d.sent,
                delivered: d.delivered,
                read: d.read,
                joined: d.joined,
                left: d.left,
            })
            .collect(),
        next_after: page.next_after,
    }))
}
