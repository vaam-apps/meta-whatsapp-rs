//! Groups (docs/design/server.md, section 4.2; roadmap M5f), as services an
//! API adapter calls, with the tenant's WABA token, over the library's
//! `client::groups` and the group analytics of `client::analytics`:
//!
//! | Operation | Graph call |
//! | --- | --- |
//! | [`Groups::create`] | `POST /{pn}/groups` (asynchronous: the group's id arrives in a `group_lifecycle_update` webhook) |
//! | [`Groups::list`] | `GET /{pn}/groups?limit&after&before` |
//! | [`Groups::get`] | `GET /{group_id}?fields=…` |
//! | [`Groups::update`] | `POST /{group_id}` (subject, description) |
//! | [`Groups::delete`] | `DELETE /{group_id}` |
//! | [`Groups::invite_link`], [`Groups::reset_invite_link`] | `GET`, `POST /{group_id}/invite_link` |
//! | [`Groups::participants`] | `GET /{group_id}?fields=participants,total_participant_count` |
//! | [`Groups::remove_participants`] | `DELETE /{group_id}/participants` |
//! | [`Groups::join_requests`], [`Groups::approve_join_requests`], [`Groups::reject_join_requests`] | `GET`, `POST`, `DELETE /{group_id}/join_requests` |
//! | [`Groups::analytics`] | `GET /{waba_id}/group_analytics` for the one group |
//! | [`Groups::set_picture`] | `POST /{group_id}` as `multipart/form-data` (roadmap M5f2) |
//!
//! Pages: `groups/reference`, `groups/webhooks`, `analytics` (group
//! analytics). Meta documents no call that makes the business leave a group
//! other than deleting it (which removes every participant), and the
//! library's pin and add-participant calls are not offered here; its picture
//! call is ([`Groups::set_picture`], roadmap M5f2).
//!
//! **A group is the number's, or it does not exist** (the design's M5.2),
//! and that is decided **before the tenant's token is opened** (D150): a
//! group's id says nothing of the tenant that may act on it (it is an opaque
//! id, and one token may reach several tenants' numbers), and a lookup at
//! Meta needs the token. So the service keeps its own records of the groups
//! a number holds ([`GroupRecords`], a typed store on the bundle's
//! `KvStore`, namespace [`NAMESPACE`], pending the owner's confirmation):
//!
//! - **written** by the webhook pipeline when a `group_lifecycle_update`
//!   reports a group created for a tenant's number (and **removed** when one
//!   reports it deleted), and by [`Groups::list`] for every group Meta
//!   lists for the number (the way to adopt groups that were created before
//!   the service, or whose webhook was missed);
//! - **read** by [`GroupRecords::owned`], which runs step 4 of the
//!   authorization order for the number (bound to the caller's tenant: no
//!   token) and then looks the group up under that number: a record of
//!   another tenant, of another number, a missing one and an id that cannot
//!   be a group's are all `404 not_found`, with nothing asked of Meta and
//!   **no vault read** (the token is opened afterwards, step 5, by whoever
//!   holds the [`OwnedGroup`] it makes).
//!
//! A record holds the tenant and the number, and is keyed by a SHA-256 of
//! the two ids: the group's id is not stored in clear.
//!
//! **Nothing of a participant reaches a log, an error or an audit event.**
//! A participant's phone number, BSUID, username and a join request's id
//! (which embeds a number) are in the answers of the routes that return
//! them, and nowhere else: a failed call answers Meta's code and the
//! service's kind, never Meta's text (it may quote them), and the changes'
//! audit events ([`crate::audit::groups_audit`]) hold counts and outcomes.
//! A group's id (which embeds the business number) is in no line of the
//! service's own, audit events included; the library's debug line for a
//! Graph request (`graph request`, `debug`) names the path it calls, so a
//! group's id is in that one (a gap of the library's, reported with M5f).
//! The invite link is a credential (whoever holds it can join): it is in the
//! answer to its two routes and nowhere else, and the library's `Debug`
//! shows it redacted.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::analytics::{GroupAnalytics, GroupAnalyticsQuery, GroupMetric};
use meta_whatsapp_rs::client::groups::{
    ApprovedJoinRequests, CreateGroup, DESCRIPTION_MAX_CHARS, GroupField, GroupInfo,
    GroupParticipant, GroupSettingsUpdate, JoinApprovalMode, JoinRequest, LIST_LIMIT_MAX,
    ListGroups, ListJoinRequests, MAX_PARTICIPANTS_PER_REQUEST, PICTURE_MAX_BYTES,
    RejectedJoinRequests, SUBJECT_MAX_CHARS,
};
use meta_whatsapp_rs::core::GraphApiError;
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{GroupId, PhoneNumberId, UserId, WaId};
use meta_whatsapp_rs::core::recipient::Recipient;
use meta_whatsapp_rs::core::store::{Expiry, JsonStore, KvStore};
use meta_whatsapp_rs::webhooks::WebhookEvent;
use meta_whatsapp_rs::webhooks::fields::GroupUpdateType;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use crate::audit::{GroupFacts, Outcome, Subject, groups_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber, object_refused};
use crate::error::{ErrorCode, ServiceError, kind_code};
use crate::messages::is_e164;
use crate::model::TenantId;
use crate::observe::Observer;
use crate::ratelimit::{Slot, Slots};

/// The target of this module's log lines.
const LOG_TARGET: &str = "meta_whatsapp_server::api::groups";

/// The `KvStore` namespace of the groups' records: a stored name, pending
/// the owner's confirmation before the first release.
pub const NAMESPACE: &str = "wa.server.group";

/// The audit actions (docs/design/server.md, D155); pending the owner's
/// confirmation before the first release.
const CREATION_REQUESTED: &str = "group_creation_requested";
const SETTINGS_UPDATED: &str = "group_settings_updated";
const DELETED: &str = "group_deleted";
const INVITE_LINK_RESET: &str = "group_invite_link_reset";
const PARTICIPANTS_REMOVED: &str = "group_participants_removed";
const JOIN_REQUESTS_APPROVED: &str = "group_join_requests_approved";
const JOIN_REQUESTS_REJECTED: &str = "group_join_requests_rejected";
const PICTURE_SET: &str = "group_picture_set";

/// Most join requests one approval or rejection names: the service's own
/// bound (D154), Meta documents none.
pub const MAX_JOIN_REQUESTS_PER_CALL: usize = 100;

/// Longest a group id or a join request id may be: the service's own bound
/// (Meta's ids are base64-like strings of about 60 characters; none is
/// documented).
const MAX_ID_CHARS: usize = 512;

/// Group analytics reach back at most this far (`analytics`, "Group
/// analytics": "no more than 90 days from the current date").
pub const ANALYTICS_LOOK_BACK: Duration = Duration::days(90);

/// Meta's code for "not all participant-level operations succeeded".
const PARTIAL_FAILURE: i64 = 131_201;

// ─── The groups a number holds ───────────────────────────────────────────

/// What the service keeps of a group: whose it is. No group id (the key is
/// a hash of it).
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    tenant: String,
    phone_number_id: String,
}

/// The records of the groups each number holds, over the bundle's
/// `KvStore` (a typed store on the port, never a port of its own): what
/// decides, before any token is opened, whether a group is a number's
/// ([`Self::owned`]). Cheap to clone.
#[derive(Clone)]
pub struct GroupRecords {
    store: JsonStore<Record>,
}

impl fmt::Debug for GroupRecords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GroupRecords").finish_non_exhaustive()
    }
}

/// A group's id as the service takes one: 1 to [`MAX_ID_CHARS`] visible
/// ASCII characters (an id Meta issues is base64-like), never a path
/// segment's trouble (the library keeps it one encoded segment). Anything
/// else cannot be a group the service holds.
fn group_id(raw: &str) -> Option<GroupId> {
    (!raw.is_empty() && raw.len() <= MAX_ID_CHARS && raw.bytes().all(|b| b.is_ascii_graphic()))
        .then(|| GroupId::new(raw))
}

/// A number's id as a record key may hold it: digits only (a bound number's
/// is; one that arrives as a plain string may not be), so that the key's
/// `.` always separates the number from the hash, whatever reaches the
/// writers.
fn keyable(number: &PhoneNumberId) -> bool {
    !number.as_str().is_empty() && number.as_str().bytes().all(|b| b.is_ascii_digit())
}

impl GroupRecords {
    /// The records in `kv`.
    pub fn new(kv: Arc<dyn KvStore>) -> Self {
        Self {
            store: JsonStore::new(kv, NAMESPACE),
        }
    }

    /// The record's key: the number, and a hash of the group's id.
    fn key(number: &PhoneNumberId, group: &GroupId) -> String {
        let mut hash = Sha256::new();
        hash.update(b"meta-whatsapp-server/group/v1\0");
        hash.update(number.as_str().as_bytes());
        hash.update(b"\0");
        hash.update(group.as_str().as_bytes());
        format!("{}.{}", number.as_str(), hex::encode(hash.finalize()))
    }

    /// Keep `group` as `tenant`'s number `number`'s. Crate-private: only
    /// the webhook pipeline and [`Groups::list`] write the records. A
    /// record already naming the same tenant and number is not written
    /// again.
    pub(crate) async fn record(
        &self,
        tenant: &TenantId,
        number: &PhoneNumberId,
        group: &GroupId,
    ) -> Result<(), StorageError> {
        if !keyable(number) {
            return Ok(());
        }
        let key = Self::key(number, group);
        if let Some((held, _, _)) = self.store.get(&key).await?
            && held.tenant == tenant.as_str()
            && held.phone_number_id == number.as_str()
        {
            return Ok(());
        }
        let record = Record {
            tenant: tenant.as_str().to_owned(),
            phone_number_id: number.as_str().to_owned(),
        };
        self.store.put(&key, &record, Expiry::Never).await?;
        Ok(())
    }

    /// Forget `group` of `number`. Crate-private: the webhook pipeline's.
    pub(crate) async fn forget(
        &self,
        number: &PhoneNumberId,
        group: &GroupId,
    ) -> Result<(), StorageError> {
        if !keyable(number) {
            return Ok(());
        }
        self.store.delete(&Self::key(number, group)).await?;
        Ok(())
    }

    /// What a recorded webhook event says about the groups of a number
    /// held by `tenant`: a group created is its number's, a group deleted
    /// is not any longer. Anything else (a creation that failed, a
    /// deletion that failed, every other group event) changes nothing.
    /// Crate-private: the webhook pipeline's, once the event is routed to
    /// the tenant owning its number.
    pub(crate) async fn on_event(
        &self,
        tenant: &TenantId,
        event: &WebhookEvent,
    ) -> Result<(), StorageError> {
        let WebhookEvent::GroupUpdated {
            phone_number_id,
            update,
            ..
        } = event
        else {
            return Ok(());
        };
        if !update.errors.is_empty() {
            return Ok(());
        }
        match update.update_type {
            GroupUpdateType::GroupCreate => {
                self.record(tenant, phone_number_id, &update.group_id).await
            }
            GroupUpdateType::GroupDelete => self.forget(phone_number_id, &update.group_id).await,
            _ => Ok(()),
        }
    }

    /// The group `group` of the number `pn`, for `caller`: step 4 of the
    /// authorization order for the number (bound to the caller's tenant:
    /// **no token, no vault read**), then the group among the number's
    /// ([the module](self)). The [`OwnedGroup`] is what lets the caller
    /// open the token (step 5) and act on that group only.
    ///
    /// # Errors
    ///
    /// `403 forbidden` (a `caller` another [`Authorizer`] made); `404
    /// not_found`: the number is not the caller's tenant's, or the group
    /// is not among the number's (another tenant's, another number's, none,
    /// or an id no group has), the same answer for each, with nothing asked
    /// of Meta; `503 storage_unavailable`.
    pub async fn owned(
        &self,
        authz: &Authorizer,
        caller: &Caller,
        pn: &PhoneNumberId,
        group: &str,
    ) -> Result<OwnedGroup, ServiceError> {
        authz.admit(caller)?;
        authz.tenant_number(caller, pn).await?;
        let Some(group) = group_id(group).filter(|_| keyable(pn)) else {
            return Err(ServiceError::not_found());
        };
        let held = self.store.get(&Self::key(pn, &group)).await?;
        match held {
            Some((record, _, _))
                if record.tenant == caller.tenant().as_str()
                    && record.phone_number_id == pn.as_str() =>
            {
                Ok(OwnedGroup {
                    phone_number_id: pn.clone(),
                    tenant: caller.tenant().clone(),
                    group,
                })
            }
            _ => {
                tracing::info!(
                    target: LOG_TARGET,
                    "a group the number does not hold: not found, Meta not asked"
                );
                Err(ServiceError::not_found())
            }
        }
    }
}

/// A group the caller's tenant's number holds, proved by the records before
/// any token was opened. Only [`GroupRecords::owned`] makes one. `Debug`
/// shows nothing of the group.
#[derive(Clone)]
pub struct OwnedGroup {
    phone_number_id: PhoneNumberId,
    tenant: TenantId,
    group: GroupId,
}

impl fmt::Debug for OwnedGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OwnedGroup")
            .field("phone_number_id", &self.phone_number_id)
            .finish_non_exhaustive()
    }
}

impl OwnedGroup {
    /// The number the group is held by.
    pub fn phone_number_id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }
}

// ─── The requests' checks ────────────────────────────────────────────────

/// The group a request creates, checked (the library's limits: a subject of
/// 1 to 128 characters once trimmed, a description of at most 2,048).
///
/// # Errors
///
/// `422 invalid_request` on `subject`, `description` or
/// `join_approval_mode` (not `approval_required` or `auto_approve`). Never
/// quotes a value.
pub fn new_group(
    subject: &str,
    description: Option<String>,
    join_approval_mode: Option<&str>,
) -> Result<CreateGroup, ServiceError> {
    check_subject(subject)?;
    check_description(description.as_deref())?;
    let join_approval_mode = match join_approval_mode {
        None => None,
        Some("approval_required") => Some(JoinApprovalMode::ApprovalRequired),
        Some("auto_approve") => Some(JoinApprovalMode::AutoApprove),
        Some(_) => return Err(ServiceError::invalid("join_approval_mode")),
    };
    Ok(CreateGroup {
        subject: subject.to_owned(),
        description,
        join_approval_mode,
    })
}

/// A change of a group's subject and/or description, checked.
///
/// # Errors
///
/// `422 invalid_request` on `update` (neither is set), `subject` or
/// `description`.
pub fn settings_update(
    subject: Option<String>,
    description: Option<String>,
) -> Result<GroupSettingsUpdate, ServiceError> {
    if subject.is_none() && description.is_none() {
        return Err(ServiceError::invalid("update"));
    }
    if let Some(subject) = &subject {
        check_subject(subject)?;
    }
    check_description(description.as_deref())?;
    Ok(GroupSettingsUpdate {
        subject,
        description,
    })
}

fn check_subject(subject: &str) -> Result<(), ServiceError> {
    let chars = subject.trim().chars().count();
    if (1..=SUBJECT_MAX_CHARS).contains(&chars) {
        Ok(())
    } else {
        Err(ServiceError::invalid("subject"))
    }
}

fn check_description(description: Option<&str>) -> Result<(), ServiceError> {
    if description.is_some_and(|d| d.chars().count() > DESCRIPTION_MAX_CHARS) {
        Err(ServiceError::invalid("description"))
    } else {
        Ok(())
    }
}

/// A participant a request names: a phone number, a `wa_id` or a BSUID,
/// exactly one.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Person {
    /// E.164 **with** its `+`.
    pub phone: Option<String>,
    /// A WhatsApp user id as Meta lists it for a participant: digits, no
    /// `+` (the `wa_id`).
    pub wa_id: Option<String>,
    /// A business-scoped user id (`US.13491208655302741918`).
    pub user_id: Option<String>,
}

impl fmt::Debug for Person {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Person").finish_non_exhaustive()
    }
}

/// The library's recipients for `people` (1 to 8: Meta's limit a request),
/// checked. Meta's page names a participant by phone number or WhatsApp
/// user id (`user`), or by BSUID (`user_id`), not both.
///
/// # Errors
///
/// `422 invalid_request` on `users` (none, or more than 8), on
/// `users[i]` (none or more than one of `phone`, `wa_id`, `user_id`), on
/// `users[i].phone` (not strict E.164 with its `+`), `users[i].wa_id` (not
/// 1 to 15 digits) or `users[i].user_id` (not a standard BSUID). Never
/// quotes a value.
pub fn people(people: Vec<Person>) -> Result<Vec<Recipient>, ServiceError> {
    if people.is_empty() || people.len() > MAX_PARTICIPANTS_PER_REQUEST {
        return Err(ServiceError::invalid("users"));
    }
    people
        .into_iter()
        .enumerate()
        .map(|(i, person)| {
            let field = |name: &str| ServiceError::invalid(format!("users[{i}].{name}"));
            match (person.phone, person.wa_id, person.user_id) {
                (Some(phone), None, None) if is_e164(&phone) => Ok(Recipient::Phone(phone)),
                (Some(_), None, None) => Err(field("phone")),
                (None, Some(wa_id), None)
                    if (1..=15).contains(&wa_id.len())
                        && wa_id.bytes().all(|b| b.is_ascii_digit())
                        && !wa_id.starts_with('0') =>
                {
                    Ok(Recipient::Phone(wa_id))
                }
                (None, Some(_), None) => Err(field("wa_id")),
                (None, None, Some(user)) => {
                    let id = UserId::new(user);
                    if id.is_bsuid() {
                        Ok(Recipient::User(id))
                    } else {
                        Err(field("user_id"))
                    }
                }
                _ => Err(ServiceError::invalid(format!("users[{i}]"))),
            }
        })
        .collect()
}

/// The join request ids of a request: 1 to [`MAX_JOIN_REQUESTS_PER_CALL`],
/// each a visible ASCII id (Meta's ids are base64-like).
///
/// # Errors
///
/// `422 invalid_request` on `join_request_ids` (none, or too many) or on
/// `join_request_ids[i]`. Never quotes a value.
pub fn join_request_ids(ids: Vec<String>) -> Result<Vec<String>, ServiceError> {
    if ids.is_empty() || ids.len() > MAX_JOIN_REQUESTS_PER_CALL {
        return Err(ServiceError::invalid("join_request_ids"));
    }
    for (i, id) in ids.iter().enumerate() {
        if id.is_empty() || id.len() > MAX_ID_CHARS || !id.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ServiceError::invalid(format!("join_request_ids[{i}]")));
        }
    }
    Ok(ids)
}

/// The fields of a group a read asks for: a comma-separated list of the
/// group's scalar fields (the participants have a route of their own).
/// Empty: all of them.
///
/// # Errors
///
/// `422 invalid_request` on `fields` (a name that is none of `subject`,
/// `description`, `join_approval_mode`, `suspended`, `creation_timestamp`,
/// `total_participant_count`).
pub fn group_fields(fields: Option<&str>) -> Result<Vec<GroupField>, ServiceError> {
    const ALL: [GroupField; 6] = [
        GroupField::Subject,
        GroupField::Description,
        GroupField::JoinApprovalMode,
        GroupField::Suspended,
        GroupField::CreationTimestamp,
        GroupField::TotalParticipantCount,
    ];
    let Some(fields) = fields.filter(|f| !f.is_empty()) else {
        return Ok(ALL.to_vec());
    };
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for name in fields.split(',') {
        let field = ALL
            .into_iter()
            .find(|f| f.as_str() == name)
            .ok_or_else(|| ServiceError::invalid("fields"))?;
        if seen.insert(name) {
            out.push(field);
        }
    }
    Ok(out)
}

/// A group analytics query, checked: the range within the look-back
/// ([`ANALYTICS_LOOK_BACK`]), `start` before `end`, and 1 to 5 distinct
/// metrics of `SENT`, `DELIVERED`, `READ`, `PARTICIPANTS_JOINED`,
/// `PARTICIPANTS_LEFT` (a comma-separated list).
///
/// # Errors
///
/// `422 invalid_request` on `start` (earlier than the look-back), `end`
/// (not after `start`) or `metrics` (none, or a name that is none of them).
pub fn analytics_query(
    start: OffsetDateTime,
    end: OffsetDateTime,
    metrics: &str,
    now: OffsetDateTime,
) -> Result<(OffsetDateTime, OffsetDateTime, Vec<GroupMetric>), ServiceError> {
    if start < now - ANALYTICS_LOOK_BACK {
        return Err(ServiceError::invalid("start"));
    }
    if end <= start {
        return Err(ServiceError::invalid("end"));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for name in metrics.split(',').filter(|m| !m.is_empty()) {
        let metric = match name {
            "SENT" => GroupMetric::Sent,
            "DELIVERED" => GroupMetric::Delivered,
            "READ" => GroupMetric::Read,
            "PARTICIPANTS_JOINED" => GroupMetric::ParticipantsJoined,
            "PARTICIPANTS_LEFT" => GroupMetric::ParticipantsLeft,
            _ => return Err(ServiceError::invalid("metrics")),
        };
        if seen.insert(name) {
            out.push(metric);
        }
    }
    if out.is_empty() {
        return Err(ServiceError::invalid("metrics"));
    }
    Ok((start, end, out))
}

// ─── The answers ─────────────────────────────────────────────────────────

/// What a creation answers: Meta's request id (the group's id follows in a
/// `group_lifecycle_update` event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// Meta's id of the creation request, which the webhook echoes.
    pub request_id: Option<String>,
}

/// A group of a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSummary {
    /// Its id.
    pub id: String,
    /// Its subject.
    pub subject: Option<String>,
    /// When it was created.
    pub created_at: Option<OffsetDateTime>,
}

/// A page of a number's groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupsPage {
    /// The groups.
    pub groups: Vec<GroupSummary>,
    /// Meta's cursor for the next page, when one follows.
    pub next_after: Option<String>,
    /// Meta's cursor for the page before.
    pub previous_before: Option<String>,
}

/// A group, as Meta describes it. The participants are the participants
/// route's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDetails {
    /// Its id.
    pub id: Option<String>,
    /// Its subject.
    pub subject: Option<String>,
    /// Its description.
    pub description: Option<String>,
    /// `approval_required` or `auto_approve`.
    pub join_approval_mode: Option<String>,
    /// Whether WhatsApp suspended it.
    pub suspended: Option<bool>,
    /// When it was created.
    pub creation_timestamp: Option<OffsetDateTime>,
    /// How many participants it has, the business excluded.
    pub total_participant_count: Option<u64>,
}

/// A participant of a group. `Debug` shows nothing of them.
#[derive(Clone, PartialEq, Eq)]
pub struct Participant {
    /// Their phone number as WhatsApp knows it (absent when they have a
    /// username).
    pub wa_id: Option<String>,
    /// Their BSUID.
    pub user_id: Option<String>,
    /// Their parent BSUID, when the portfolio is enrolled for them.
    pub parent_user_id: Option<String>,
    /// Their username.
    pub username: Option<String>,
}

impl fmt::Debug for Participant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Participant").finish_non_exhaustive()
    }
}

impl From<GroupParticipant> for Participant {
    fn from(p: GroupParticipant) -> Self {
        Self {
            wa_id: p.wa_id.map(WaId::into_inner),
            user_id: p.user_id.map(UserId::into_inner),
            parent_user_id: p.parent_user_id.map(UserId::into_inner),
            username: p.username,
        }
    }
}

/// A group's participants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participants {
    /// The participants, the business excluded.
    pub participants: Vec<Participant>,
    /// Their count, as Meta says it.
    pub total_participant_count: Option<u64>,
}

/// What removing participants came to: whether Meta says some were not
/// removed (HTTP `206`, its code `131201`). Which ones, Meta tells in the
/// `group_participants_update` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Removal {
    /// Some participants were not removed.
    pub partial: bool,
}

/// A group's invite link: a credential. `Debug` shows it redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct InviteLink {
    /// `https://chat.whatsapp.com/<code>`.
    pub invite_link: String,
}

impl fmt::Debug for InviteLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InviteLink").finish_non_exhaustive()
    }
}

/// An open join request. `Debug` shows nothing of the person.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingJoin {
    /// Its id.
    pub join_request_id: String,
    /// The person's phone number (absent when they have a username).
    pub wa_id: Option<String>,
    /// Their BSUID.
    pub user_id: Option<String>,
    /// Their parent BSUID.
    pub parent_user_id: Option<String>,
    /// Their username.
    pub username: Option<String>,
    /// When they asked.
    pub created_at: Option<OffsetDateTime>,
}

impl fmt::Debug for PendingJoin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingJoin").finish_non_exhaustive()
    }
}

impl From<JoinRequest> for PendingJoin {
    fn from(j: JoinRequest) -> Self {
        Self {
            join_request_id: j.join_request_id,
            wa_id: j.wa_id.map(WaId::into_inner),
            user_id: j.user_id.map(UserId::into_inner),
            parent_user_id: j.parent_user_id.map(UserId::into_inner),
            username: j.username,
            created_at: j.creation_timestamp,
        }
    }
}

/// A page of a group's open join requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinRequestsPage {
    /// The requests.
    pub requests: Vec<PendingJoin>,
    /// Meta's cursor for the next page, when one follows.
    pub next_after: Option<String>,
    /// Meta's cursor for the page before.
    pub previous_before: Option<String>,
}

/// Why Meta refused: its code and the service's kind for it, never its
/// text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    /// Meta's error code.
    pub code: i64,
    /// The service's error code for it (`ErrorKind`'s), to branch on.
    pub kind: &'static str,
}

impl From<&GraphApiError> for Reason {
    fn from(error: &GraphApiError) -> Self {
        Self {
            code: error.code,
            kind: kind_code(error.kind()),
        }
    }
}

/// A join request Meta did not approve or reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedJoin {
    /// Its id.
    pub join_request_id: Option<String>,
    /// Why.
    pub reasons: Vec<Reason>,
}

/// What approving or rejecting join requests came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinOutcome {
    /// The requests Meta approved (or rejected).
    pub done: Vec<String>,
    /// The ones it did not.
    pub failed: Vec<FailedJoin>,
    /// Meta's errors of the call as a whole.
    pub reasons: Vec<Reason>,
}

fn reasons(errors: &[GraphApiError]) -> Vec<Reason> {
    errors.iter().map(Reason::from).collect()
}

impl From<ApprovedJoinRequests> for JoinOutcome {
    fn from(a: ApprovedJoinRequests) -> Self {
        Self {
            done: a.approved_join_requests,
            failed: a
                .failed_join_requests
                .into_iter()
                .map(|f| FailedJoin {
                    join_request_id: f.join_request_id,
                    reasons: reasons(&f.errors),
                })
                .collect(),
            reasons: reasons(&a.errors),
        }
    }
}

impl From<RejectedJoinRequests> for JoinOutcome {
    fn from(r: RejectedJoinRequests) -> Self {
        Self {
            done: r.rejected_join_requests,
            failed: r
                .failed_join_requests
                .into_iter()
                .map(|f| FailedJoin {
                    join_request_id: f.join_request_id,
                    reasons: reasons(&f.errors),
                })
                .collect(),
            reasons: reasons(&r.errors),
        }
    }
}

/// A day of a group's analytics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayOfAnalytics {
    /// The day's start.
    pub start: OffsetDateTime,
    /// The day's end.
    pub end: OffsetDateTime,
    /// Messages the business sent.
    pub sent: Option<u64>,
    /// Deliveries to participants.
    pub delivered: Option<u64>,
    /// Reads by participants.
    pub read: Option<u64>,
    /// Participants who joined.
    pub joined: Option<u64>,
    /// Participants who left.
    pub left: Option<u64>,
}

/// A page of a group's analytics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsPage {
    /// The days, in Meta's order.
    pub days: Vec<DayOfAnalytics>,
    /// Meta's cursor for the next page, when one follows.
    pub next_after: Option<String>,
}

// ─── A group's picture ───────────────────────────────────────────────────

/// Smallest side of a group's picture, in pixels (Meta: "Minimum size:
/// 192 x 192").
pub const PICTURE_MIN_SIDE: u32 = 192;

/// Most bytes a group's picture may be: the library's bound (5 MiB; Meta's
/// page says "5MB"), what the upload reader holds at most.
pub const PICTURE_MAX: u64 = PICTURE_MAX_BYTES as u64;

/// A group's picture, checked before any request: a JPEG (its first bytes,
/// not a claimed type) of at most [`PICTURE_MAX`] bytes whose frame header
/// says it is square and at least [`PICTURE_MIN_SIDE`] pixels a side. Only
/// the header is read; the service decodes no image, so a file that is a
/// JPEG at its start and corrupt after it is Meta's to refuse. Its `Debug`
/// shows no byte.
#[derive(Clone)]
pub struct Picture {
    data: Bytes,
}

impl fmt::Debug for Picture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Picture")
            .field("len", &self.data.len())
            .finish()
    }
}

impl Picture {
    /// Check `data`.
    ///
    /// # Errors
    ///
    /// `413 media_too_large` over [`PICTURE_MAX`] bytes; `422
    /// invalid_request` on `file` when it is empty, does not start as a
    /// JPEG, has no readable frame header, or is not square or smaller than
    /// [`PICTURE_MIN_SIDE`] a side.
    pub fn new(data: Bytes) -> Result<Self, ServiceError> {
        if data.len() > PICTURE_MAX_BYTES {
            return Err(ServiceError::new(ErrorCode::MEDIA_TOO_LARGE));
        }
        // The SOI marker and the first segment's marker prefix. Three
        // guards hold this, each enough alone: this line, the header
        // reader's own SOI check and the client library's `set_picture`
        // (the same three bytes); so no route test can tell one gone, and
        // `the_header_reader_survives_hostile_input` holds the reader's
        // (roadmap M5f2 review).
        if !data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Err(ServiceError::invalid("file"));
        }
        match jpeg_dimensions(&data) {
            Some((width, height)) if width == height && width >= PICTURE_MIN_SIDE => {
                Ok(Self { data })
            }
            _ => Err(ServiceError::invalid("file")),
        }
    }

    /// The file.
    pub fn data(&self) -> &Bytes {
        &self.data
    }
}

/// The frame header's `(width, height)` of a JPEG: the first start-of-frame
/// segment, before any scan. `None` when there is none, the segments do not
/// add up, or a side is `0` (defined later by a DNL segment, which is not
/// read).
fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let be16 = |at: usize| -> Option<u32> {
        Some(u32::from(*data.get(at)?) << 8 | u32::from(*data.get(at + 1)?))
    };
    // The reader does not lean on its caller for the signature.
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2; // past the SOI marker
    loop {
        // A marker: 0xFF, any fill 0xFF, then its code.
        if *data.get(at)? != 0xFF {
            return None;
        }
        while *data.get(at)? == 0xFF {
            at += 1;
        }
        let marker = *data.get(at)?;
        at += 1;
        match marker {
            // Without a length: TEM, RSTn, SOI.
            0x01 | 0xD0..=0xD8 => continue,
            // The end, or a scan, before any frame header; a stuffed 0xFF.
            0xD9 | 0xDA | 0x00 => return None,
            _ => {}
        }
        let length = usize::try_from(be16(at)?).ok()?;
        if length < 2 {
            return None;
        }
        // SOF0..SOF15, but DHT (C4), JPG (C8) and DAC (CC).
        if matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            // length, precision, height, width.
            if length < 8 {
                return None;
            }
            let (height, width) = (be16(at + 3)?, be16(at + 5)?);
            return (width > 0 && height > 0).then_some((width, height));
        }
        at = at.checked_add(length)?;
    }
}

// ─── The operations ──────────────────────────────────────────────────────

/// The group operations on a number of the caller's tenant: an
/// [`OwnedNumber`] its [`Authorizer`] made from that tenant's binding, for
/// a [`Caller`] it made, both admitted ([`Self::new`]), with the number's
/// WABA token. Cheap: a few references.
pub struct Groups<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    records: &'a GroupRecords,
    clock: Arc<dyn Clock>,
    observer: &'a dyn Observer,
}

impl fmt::Debug for Groups<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Groups")
            .field("caller", self.caller)
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> Groups<'a> {
    /// The operations on `number` for `caller`, once `authz` admits both
    /// ([`Authorizer::admit`], [`Authorizer::admit_number`]) and `number`
    /// was made from a binding of `caller`'s tenant. Failed Graph calls are
    /// reported to nobody until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `caller` or `number` another [`Authorizer`] made: `403 forbidden`.
    /// `number` another tenant's: `404 not_found`, logged at `warn`.
    /// Either way, before anything is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        number: &'a OwnedNumber,
        records: &'a GroupRecords,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_number(number)?;
        if number.tenant() != caller.tenant() {
            tracing::warn!("refused the group services on another tenant's number");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
            number,
            records,
            clock,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// The error of a failed Graph call: Meta's code and the service's
    /// kind, **without Meta's texts** in the answer or any log (they may
    /// quote a participant: [`OwnedNumber::failed_withholding_texts`]).
    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self
            .number
            .failed_withholding_texts(self.authz, error)
            .await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    /// [`Self::failed`] for a call on the group an id names: Meta refusing
    /// the group is `404 not_found` (the record outlived the group, say).
    ///
    /// Meta's documented refusal is `131041` (`ErrorKind::NotFound`), but
    /// the rule is wider on purpose (D172): [`object_refused`] also takes
    /// an invalid parameter, a permission error and a 4xx `Unknown` as
    /// Meta refusing the object, because the page is not all of Meta's
    /// behaviour. It no longer reaches the codes the groups page documents
    /// with a kind of their own (`131059` `invalid_cursor`, `131207`
    /// `group_suspended`, `131215` `feature_not_available`, `131208`
    /// `group_rate_limited`...): those are answered as that kind (D170).
    async fn failed_on_group(&self, error: &Error) -> ServiceError {
        let api = self.failed(error).await;
        // An answer that does not decode is not Meta refusing the group (the
        // id came from the service's own records, not from the caller).
        if !matches!(error, Error::Decode { .. }) && object_refused(error) {
            tracing::debug!(
                target: LOG_TARGET,
                code = api.code(),
                "Meta refused the group: answered not_found"
            );
            return ServiceError::not_found();
        }
        api
    }

    /// `group` is the one of this number (the capability was made for it).
    fn group(
        &self,
        group: &OwnedGroup,
    ) -> Result<meta_whatsapp_rs::client::groups::Group, ServiceError> {
        if &group.phone_number_id != self.number.phone_number_id()
            || &group.tenant != self.caller.tenant()
        {
            return Err(ServiceError::not_found());
        }
        Ok(self.number.client().group(group.group.clone()))
    }

    fn numbers_groups(&self) -> meta_whatsapp_rs::client::groups::Groups {
        self.number
            .client()
            .groups(self.number.phone_number_id().clone())
    }

    /// The caller's audit event `action`.
    fn audit(&self, action: &'static str, facts: GroupFacts, outcome: Outcome) {
        groups_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.number.waba_id().as_str()),
                phone_number_id: Some(self.number.phone_number_id().as_str()),
                ..Subject::default()
            },
            facts,
            outcome,
        );
    }

    /// The error of a change that failed, audited as `action` with an
    /// unknown outcome when it may have gone through.
    async fn change_failed(
        &self,
        action: &'static str,
        users: usize,
        error: &Error,
        on_group: bool,
    ) -> ServiceError {
        let failed = if on_group {
            self.failed_on_group(error).await
        } else {
            self.failed(error).await
        };
        let partial = failed.graph().is_some_and(|g| g.code == PARTIAL_FAILURE);
        if failed.may_have_been_sent() || partial {
            self.audit(
                action,
                GroupFacts {
                    users: users as u64,
                    ..GroupFacts::default()
                },
                Outcome::Unanswered,
            );
        }
        failed
    }

    /// Ask Meta to create the group `new` (checked by [`new_group`]):
    /// `POST /{pn}/groups`, not replayed on a timeout. The group is made
    /// asynchronously: its id comes in a `group_lifecycle_update` event,
    /// which also makes the number's record of it. The caller's audit event
    /// `group_creation_requested`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure (a timeout `may_have_been_sent`).
    pub async fn create(&self, new: &CreateGroup) -> Result<Created, ServiceError> {
        match self.numbers_groups().create(new).await {
            Ok(created) => {
                self.audit(CREATION_REQUESTED, GroupFacts::default(), Outcome::Answered);
                Ok(Created {
                    request_id: created.request_id,
                })
            }
            Err(error) => Err(self
                .change_failed(CREATION_REQUESTED, 0, &error, false)
                .await),
        }
    }

    /// A page of the number's groups, which the service records as the
    /// number's (the way to hold groups it has no webhook for).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `limit` (1 to 1,024), before any request.
    /// Meta's refusal or failure; `503 storage_unavailable` (a record).
    pub async fn list(
        &self,
        limit: Option<u32>,
        after: Option<String>,
        before: Option<String>,
    ) -> Result<GroupsPage, ServiceError> {
        if limit.is_some_and(|limit| !(1..=LIST_LIMIT_MAX).contains(&limit)) {
            return Err(ServiceError::invalid("limit"));
        }
        let page = match self
            .numbers_groups()
            .list(&ListGroups {
                limit,
                after,
                before,
            })
            .await
        {
            Ok(page) => page,
            Err(error) => return Err(self.failed(&error).await),
        };
        let next_after = page.next_cursor().map(str::to_owned);
        let previous_before = page
            .paging
            .as_ref()
            .and_then(|p| p.cursors.as_ref())
            .and_then(|c| c.before.clone());
        for group in &page.data {
            self.records
                .record(
                    self.caller.tenant(),
                    self.number.phone_number_id(),
                    &group.id,
                )
                .await?;
        }
        Ok(GroupsPage {
            groups: page
                .data
                .into_iter()
                .map(|g| GroupSummary {
                    id: g.id.into_inner(),
                    subject: g.subject,
                    created_at: g.created_at,
                })
                .collect(),
            next_after,
            previous_before,
        })
    }

    /// The group `group`, with `fields` ([`group_fields`]).
    ///
    /// # Errors
    ///
    /// Meta's refusal (`404`: the group is gone) or failure.
    pub async fn get(
        &self,
        group: &OwnedGroup,
        fields: &[GroupField],
    ) -> Result<GroupDetails, ServiceError> {
        let info = match self.group(group)?.info(fields).await {
            Ok(info) => info,
            Err(error) => return Err(self.failed_on_group(&error).await),
        };
        let GroupInfo {
            id,
            subject,
            description,
            suspended,
            creation_timestamp,
            total_participant_count,
            join_approval_mode,
            ..
        } = info;
        Ok(GroupDetails {
            id: id.map(GroupId::into_inner),
            subject,
            description,
            join_approval_mode: join_approval_mode.map(|m| m.as_str().to_owned()),
            suspended,
            creation_timestamp,
            total_participant_count,
        })
    }

    /// The group's participants (the business excluded).
    ///
    /// # Errors
    ///
    /// Meta's refusal (`404`: the group is gone) or failure.
    pub async fn participants(&self, group: &OwnedGroup) -> Result<Participants, ServiceError> {
        let info = match self
            .group(group)?
            .info(&[GroupField::Participants, GroupField::TotalParticipantCount])
            .await
        {
            Ok(info) => info,
            Err(error) => return Err(self.failed_on_group(&error).await),
        };
        Ok(Participants {
            participants: info
                .participants
                .into_iter()
                .map(Participant::from)
                .collect(),
            total_participant_count: info.total_participant_count,
        })
    }

    /// Change the group's subject and/or description ([`settings_update`]):
    /// `POST /{group_id}`. Meta tells the outcome of each field in a
    /// `group_settings_update` event. The caller's audit event
    /// `group_settings_updated`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure.
    pub async fn update(
        &self,
        group: &OwnedGroup,
        update: &GroupSettingsUpdate,
    ) -> Result<(), ServiceError> {
        match self.group(group)?.update(update).await {
            Ok(()) => {
                self.audit(SETTINGS_UPDATED, GroupFacts::default(), Outcome::Answered);
                Ok(())
            }
            Err(error) => Err(self.change_failed(SETTINGS_UPDATED, 0, &error, false).await),
        }
    }

    /// Replace the group's picture with `picture` (checked by
    /// [`Picture::new`]): `POST /{group_id}` as `multipart/form-data`.
    /// Meta tells the outcome in a `group_settings_update` event. The
    /// caller's audit event `group_picture_set`, which holds no byte of the
    /// picture.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure (a timeout `may_have_been_sent`). Meta
    /// refusing the picture itself is its code and the service's kind for
    /// it, not `not_found`.
    pub async fn set_picture(
        &self,
        group: &OwnedGroup,
        picture: &Picture,
    ) -> Result<(), ServiceError> {
        match self.group(group)?.set_picture(picture.data().clone()).await {
            Ok(()) => {
                self.audit(PICTURE_SET, GroupFacts::default(), Outcome::Answered);
                Ok(())
            }
            Err(error) => Err(self.change_failed(PICTURE_SET, 0, &error, false).await),
        }
    }

    /// A transfer slot of the number's tenant's share of `transfers` (the
    /// adapter's `WA_SERVER_MEDIA_CONCURRENCY` pool, the media routes'),
    /// to be taken **before the picture's body is read**: the body is held
    /// in memory, at most [`PICTURE_MAX`] bytes.
    ///
    /// # Errors
    ///
    /// `429 too_many_requests` (`Retry-After: 1`): the tenant's share, or
    /// the pool, is taken.
    pub fn transfer_slot(&self, transfers: &Slots) -> Result<Slot, ServiceError> {
        transfers
            .try_acquire(self.number.tenant())
            .ok_or_else(|| ServiceError::too_many_requests(1))
    }

    /// Delete the group, every participant removed, the business included:
    /// `DELETE /{group_id}`. The record is dropped when Meta's
    /// `group_lifecycle_update` says the group is gone. The caller's audit
    /// event `group_deleted`.
    ///
    /// # Errors
    ///
    /// Meta's refusal (`404`: the group is gone) or failure.
    pub async fn delete(&self, group: &OwnedGroup) -> Result<(), ServiceError> {
        match self.group(group)?.delete().await {
            Ok(()) => {
                self.audit(DELETED, GroupFacts::default(), Outcome::Answered);
                Ok(())
            }
            Err(error) => Err(self.change_failed(DELETED, 0, &error, true).await),
        }
    }

    /// The group's invite link.
    ///
    /// # Errors
    ///
    /// Meta's refusal (`404`: the group is gone) or failure.
    pub async fn invite_link(&self, group: &OwnedGroup) -> Result<InviteLink, ServiceError> {
        match self.group(group)?.invite_link().await {
            Ok(link) => Ok(InviteLink {
                invite_link: link.invite_link,
            }),
            Err(error) => Err(self.failed_on_group(&error).await),
        }
    }

    /// Replace the group's invite link, every previous one stopping to
    /// work: `POST /{group_id}/invite_link`, not replayed on a timeout. The
    /// caller's audit event `group_invite_link_reset`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure.
    pub async fn reset_invite_link(&self, group: &OwnedGroup) -> Result<InviteLink, ServiceError> {
        match self.group(group)?.reset_invite_link().await {
            Ok(link) => {
                self.audit(INVITE_LINK_RESET, GroupFacts::default(), Outcome::Answered);
                Ok(InviteLink {
                    invite_link: link.invite_link,
                })
            }
            Err(error) => Err(self
                .change_failed(INVITE_LINK_RESET, 0, &error, false)
                .await),
        }
    }

    /// Remove `users` ([`people`]) from the group, who can no longer join
    /// through an invite link: `DELETE /{group_id}/participants`. The
    /// outcome of each arrives in a `group_participants_update` event. The
    /// caller's audit event `group_participants_removed`, by count.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, as its code and kind (never its text).
    pub async fn remove_participants(
        &self,
        group: &OwnedGroup,
        users: &[Recipient],
    ) -> Result<Removal, ServiceError> {
        match self.group(group)?.remove_participants(users).await {
            Ok(outcome) => {
                self.audit(
                    PARTICIPANTS_REMOVED,
                    GroupFacts {
                        users: users.len() as u64,
                        ..GroupFacts::default()
                    },
                    if outcome.partial {
                        Outcome::Unanswered
                    } else {
                        Outcome::Answered
                    },
                );
                Ok(Removal {
                    partial: outcome.partial,
                })
            }
            Err(error) => Err(self
                .change_failed(PARTICIPANTS_REMOVED, users.len(), &error, false)
                .await),
        }
    }

    /// A page of the group's open join requests.
    ///
    /// # Errors
    ///
    /// Meta's refusal (`404`: the group is gone) or failure.
    pub async fn join_requests(
        &self,
        group: &OwnedGroup,
        after: Option<String>,
        before: Option<String>,
    ) -> Result<JoinRequestsPage, ServiceError> {
        let page = match self
            .group(group)?
            .join_requests(&ListJoinRequests { after, before })
            .await
        {
            Ok(page) => page,
            Err(error) => return Err(self.failed_on_group(&error).await),
        };
        let next_after = page.next_cursor().map(str::to_owned);
        let previous_before = page
            .paging
            .as_ref()
            .and_then(|p| p.cursors.as_ref())
            .and_then(|c| c.before.clone());
        Ok(JoinRequestsPage {
            requests: page.data.into_iter().map(PendingJoin::from).collect(),
            next_after,
            previous_before,
        })
    }

    /// Approve `ids` ([`join_request_ids`]): `POST /{group_id}/join_requests`.
    /// A partial success is answered with the failures listed, as Meta's
    /// `206`. The caller's audit event `group_join_requests_approved`, by
    /// count.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, as its code and kind.
    pub async fn approve_join_requests(
        &self,
        group: &OwnedGroup,
        ids: &[String],
    ) -> Result<JoinOutcome, ServiceError> {
        match self.group(group)?.approve_join_requests(ids).await {
            Ok(done) => {
                let outcome = JoinOutcome::from(done);
                self.audit(
                    JOIN_REQUESTS_APPROVED,
                    join_facts(ids.len(), &outcome),
                    Outcome::Answered,
                );
                Ok(outcome)
            }
            Err(error) => Err(self
                .change_failed(JOIN_REQUESTS_APPROVED, ids.len(), &error, false)
                .await),
        }
    }

    /// Reject `ids` ([`join_request_ids`]): `DELETE /{group_id}/join_requests`.
    /// The caller's audit event `group_join_requests_rejected`, by count.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, as its code and kind.
    pub async fn reject_join_requests(
        &self,
        group: &OwnedGroup,
        ids: &[String],
    ) -> Result<JoinOutcome, ServiceError> {
        match self.group(group)?.reject_join_requests(ids).await {
            Ok(done) => {
                let outcome = JoinOutcome::from(done);
                self.audit(
                    JOIN_REQUESTS_REJECTED,
                    join_facts(ids.len(), &outcome),
                    Outcome::Answered,
                );
                Ok(outcome)
            }
            Err(error) => Err(self
                .change_failed(JOIN_REQUESTS_REJECTED, ids.len(), &error, false)
                .await),
        }
    }

    /// The group's analytics, day by day (`GET /{waba_id}/group_analytics`,
    /// the number's WABA, the one group): `metrics` from `start` to `end`
    /// checked by [`analytics_query`] against the service's clock.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure.
    pub async fn analytics(
        &self,
        group: &OwnedGroup,
        start: OffsetDateTime,
        end: OffsetDateTime,
        metrics: &str,
        after: Option<String>,
    ) -> Result<AnalyticsPage, ServiceError> {
        let (start, end, metrics) = analytics_query(start, end, metrics, self.clock.now())?;
        // The group's id is this number's (the capability), whichever
        // WABA's analytics are asked.
        let _ = self.group(group)?;
        let mut query = GroupAnalyticsQuery::new(start, end, vec![group.group.clone()], metrics);
        query.after = after;
        let page = match self
            .number
            .client()
            .analytics(self.number.waba_id().clone())
            .groups(&query)
            .await
        {
            Ok(page) => page,
            Err(error) => return Err(self.failed_on_group(&error).await),
        };
        let next_after = page.next_cursor().map(str::to_owned);
        let days = page
            .data
            .into_iter()
            .flat_map(|GroupAnalytics { data_points, .. }| data_points)
            .map(|point| DayOfAnalytics {
                start: point.start,
                end: point.end,
                sent: point.sent,
                delivered: point.delivered,
                read: point.read,
                joined: point.joined,
                left: point.left,
            })
            .collect();
        Ok(AnalyticsPage { days, next_after })
    }
}

/// An audit event's counts for join requests.
fn join_facts(named: usize, outcome: &JoinOutcome) -> GroupFacts {
    GroupFacts {
        users: named as u64,
        done: outcome.done.len() as u64,
        failed: outcome.failed.len() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phone(phone: &str) -> Person {
        Person {
            phone: Some(phone.to_owned()),
            ..Person::default()
        }
    }

    fn field(error: &ServiceError) -> String {
        error.field().unwrap_or("?").to_owned()
    }

    /// A JPEG's headers: SOI, `before` (segments), a frame header of
    /// `marker` for `width` by `height`, and the end.
    fn jpeg(marker: u8, width: u16, height: u16, before: &[u8]) -> Bytes {
        let mut out = vec![0xFF, 0xD8];
        out.extend(before);
        out.extend([0xFF, marker, 0, 11, 8]);
        out.extend(height.to_be_bytes());
        out.extend(width.to_be_bytes());
        out.extend([1, 1, 0x11, 0, 0xFF, 0xD9]);
        Bytes::from(out)
    }

    /// A group's picture is a JPEG by its bytes, of at most 5 MiB, square,
    /// and at least 192 a side: the frame header's side, whichever frame
    /// type (baseline, progressive, ...), past other segments, fill bytes
    /// and a DHT (whose marker `C4` is not a frame). Decisive: each clause
    /// and the marker table.
    #[test]
    fn a_picture_is_a_square_jpeg_of_at_least_192_pixels() {
        let dht = [0xFF, 0xC4, 0, 4, 0, 0];
        let app = [0xFF, 0xE1, 0, 5, 1, 2, 3];
        for marker in [0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC9, 0xCA, 0xCF] {
            assert!(
                Picture::new(jpeg(marker, 192, 192, &[])).is_ok(),
                "{marker:#x}"
            );
        }
        let mut with_fill = vec![0xFF, 0xFF];
        with_fill.extend(app);
        with_fill.extend(dht);
        assert!(Picture::new(jpeg(0xC0, 3000, 3000, &with_fill)).is_ok());
        // A DHT, a JPG extension and a DAC are no frame header.
        for marker in [0xC4, 0xC8, 0xCC] {
            assert!(
                Picture::new(jpeg(marker, 192, 192, &[])).is_err(),
                "{marker:#x}"
            );
        }
        for (width, height) in [(192, 193), (193, 192), (191, 191), (0, 0), (192, 0)] {
            let error = Picture::new(jpeg(0xC0, width, height, &[])).unwrap_err();
            assert_eq!(field(&error), "file", "{width}x{height}");
        }
        // Not a JPEG at its start, or cut.
        let mut unsigned = vec![0x42, 0x42];
        unsigned.extend(&jpeg(0xC0, 512, 512, &[])[2..]);
        assert!(Picture::new(Bytes::from(unsigned)).is_err(), "no signature");
        for bad in [
            Bytes::new(),
            Bytes::from_static(b"\x89PNG\r\n\x1a\n"),
            Bytes::from_static(&[0xFF, 0xD8]),
            Bytes::from_static(&[0xFF, 0xD8, 0xFF]),
            Bytes::from_static(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 16, 1]),
            // A length that runs past the file, and one under its own size.
            Bytes::from_static(&[0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF, 1, 2]),
            Bytes::from_static(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 1, 0xFF, 0xC0]),
            // A scan, or the end, before a frame header.
            Bytes::from_static(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]),
            Bytes::from_static(&[0xFF, 0xD8, 0xFF, 0xD9]),
        ] {
            assert!(Picture::new(bad.clone()).is_err(), "{bad:?}");
        }
        // The bound is 5 MiB, in bytes: the size is judged first.
        let mut big = jpeg(0xC0, 192, 192, &[]).to_vec();
        big.resize(PICTURE_MAX_BYTES, 0);
        assert!(Picture::new(Bytes::from(big.clone())).is_ok());
        big.push(0);
        let error = Picture::new(Bytes::from(big)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::MEDIA_TOO_LARGE.as_str());
        assert_eq!(
            format!("{:?}", Picture::new(jpeg(0xC0, 192, 192, &[])).unwrap()),
            "Picture { len: 17 }"
        );
    }

    /// The header reader on input built to hurt it: every prefix of a valid
    /// file, every value of every byte of its headers, a wall of fill bytes,
    /// a million empty segments, lengths running past the end, and
    /// pseudo-random bytes behind a signature end in a verdict, never a
    /// panic, and in one pass. Decisive: each guard of `jpeg_dimensions`
    /// (a scan, the end or a stuffed byte stops it; a frame under 8 bytes
    /// is no frame; a marker without a length is skipped).
    #[test]
    fn the_header_reader_survives_hostile_input() {
        let valid = jpeg(0xC0, 512, 512, &[0xFF, 0xE1, 0, 5, 1, 2, 3]).to_vec();
        assert!(Picture::new(Bytes::from(valid.clone())).is_ok());
        for cut in 0..valid.len() {
            let _ = Picture::new(Bytes::copy_from_slice(&valid[..cut]));
        }
        for at in 0..valid.len() {
            for value in 0..=255u8 {
                let mut changed = valid.clone();
                changed[at] = value;
                let _ = Picture::new(Bytes::from(changed));
            }
        }
        // Fill bytes up to the bound, then nothing; or then a frame.
        let mut fill = vec![0xFF, 0xD8];
        fill.resize(PICTURE_MAX_BYTES, 0xFF);
        assert!(Picture::new(Bytes::from(fill)).is_err());
        let mut fill_then_frame = vec![0xFF, 0xD8];
        fill_then_frame.resize(PICTURE_MAX_BYTES - 20, 0xFF);
        fill_then_frame.extend([0xC0, 0, 11, 8, 0x01, 0x00, 0x01, 0x00, 1, 1, 0x11, 0]);
        assert!(Picture::new(Bytes::from(fill_then_frame)).is_ok());
        // A million segments of nothing, and no frame.
        let mut empty = vec![0xFF, 0xD8];
        while empty.len() + 4 <= PICTURE_MAX_BYTES {
            empty.extend([0xFF, 0xE0, 0, 2]);
        }
        assert!(Picture::new(Bytes::from(empty)).is_err());
        // Lengths at the ends of their range.
        for length in [0u16, 1, 2, 3, 7, 8, 0xFFFE, 0xFFFF] {
            for marker in [0xE0, 0xC0, 0xFE] {
                let mut data = vec![0xFF, 0xD8, 0xFF, marker];
                data.extend(length.to_be_bytes());
                data.extend([8, 1, 0, 1, 0, 1, 1, 0x11, 0]);
                let _ = Picture::new(Bytes::from(data));
            }
        }
        // A frame of 7 bytes (no component count) is no frame, even with
        // sides that would pass; 8 is the smallest.
        let seven = [0xFF, 0xD8, 0xFF, 0xC0, 0, 7, 8, 1, 0, 1, 0];
        assert!(Picture::new(Bytes::copy_from_slice(&seven)).is_err());
        let mut eight = seven.to_vec();
        eight[5] = 8;
        eight.push(0);
        assert!(Picture::new(Bytes::from(eight)).is_ok());
        // What lies behind a scan, the end or a stuffed byte is no header,
        // however much it looks like one.
        let frame = [0xFF, 0xC0, 0, 11, 8, 1, 0, 1, 0, 1, 1, 0x11, 0];
        for (label, stop) in [
            ("a scan", &[0xFF, 0xDA, 0, 2][..]),
            ("the end", &[0xFF, 0xD9, 0, 2][..]),
            ("a byte that is no marker", &[0x42, 0, 2][..]),
            ("a stuffed byte", &[0xFF, 0x00, 0, 2][..]),
        ] {
            let mut data = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 2];
            data.extend(stop);
            data.extend(frame);
            assert!(Picture::new(Bytes::from(data)).is_err(), "{label}");
        }
        // Markers without a length are skipped: TEM, RSTn, a second SOI.
        for marker in [0x01, 0xD0, 0xD7, 0xD8] {
            let mut data = vec![0xFF, 0xD8, 0xFF, marker];
            data.extend(frame);
            assert!(Picture::new(Bytes::from(data)).is_ok(), "{marker:#x}");
        }
        // Pseudo-random bytes behind a signature (xorshift, fixed seed).
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        for _ in 0..2000 {
            let mut data = vec![0xFF, 0xD8, 0xFF];
            for _ in 0..(state % 97) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                // Biased to markers and lengths, which are what it parses.
                data.push(if state % 3 == 0 {
                    0xFF
                } else {
                    (state >> 24) as u8
                });
            }
            let _ = Picture::new(Bytes::from(data));
        }
        // The reader alone refuses a file with no SOI, whatever follows.
        let mut unsigned = vec![0x42, 0x42];
        unsigned.extend(frame);
        assert_eq!(jpeg_dimensions(&unsigned), None);
        let mut signed = vec![0xFF, 0xD8];
        signed.extend(frame);
        assert_eq!(jpeg_dimensions(&signed), Some((256, 256)));
        assert_eq!(jpeg_dimensions(&[]), None);
        assert_eq!(jpeg_dimensions(&[0xFF]), None);
        // The widest a side can be is accepted: no bound of ours.
        assert!(Picture::new(jpeg(0xC0, u16::MAX, u16::MAX, &[])).is_ok());
    }

    /// A group's subject is 1 to 128 characters once trimmed, its
    /// description at most 2,048, its join mode one of Meta's two values.
    /// Decisive: each clause, and the field named.
    #[test]
    fn a_new_group_is_checked_before_any_request() {
        assert!(new_group("  Cars  ", None, None).is_ok());
        assert!(
            new_group(
                &"x".repeat(128),
                Some("d".repeat(2048)),
                Some("auto_approve")
            )
            .is_ok()
        );
        assert_eq!(field(&new_group("   ", None, None).unwrap_err()), "subject");
        assert_eq!(
            field(&new_group(&"x".repeat(129), None, None).unwrap_err()),
            "subject"
        );
        assert_eq!(
            field(&new_group("a", Some("d".repeat(2049)), None).unwrap_err()),
            "description"
        );
        assert_eq!(
            field(&new_group("a", None, Some("manual")).unwrap_err()),
            "join_approval_mode"
        );
        let created = new_group("a", None, Some("approval_required")).unwrap();
        assert_eq!(
            created.join_approval_mode,
            Some(JoinApprovalMode::ApprovalRequired)
        );
    }

    /// A settings update changes something, within the same limits.
    #[test]
    fn a_settings_update_names_something_within_the_limits() {
        assert_eq!(field(&settings_update(None, None).unwrap_err()), "update");
        assert!(settings_update(Some("New".into()), None).is_ok());
        assert!(settings_update(None, Some(String::new())).is_ok());
        assert_eq!(
            field(&settings_update(Some(" ".into()), None).unwrap_err()),
            "subject"
        );
        assert_eq!(
            field(&settings_update(None, Some("d".repeat(2049))).unwrap_err()),
            "description"
        );
    }

    /// The participants of a request: 1 to 8, each exactly one of a strict
    /// E.164 phone, a `wa_id` of digits, a standard BSUID; the field named
    /// with its position and never the value. Decisive: each clause.
    #[test]
    fn a_removal_names_one_to_eight_people_each_one_way() {
        let ok = people(vec![
            phone("+16505551234"),
            Person {
                wa_id: Some("16505551234".to_owned()),
                ..Person::default()
            },
            Person {
                user_id: Some("US.13491208655302741918".to_owned()),
                ..Person::default()
            },
        ])
        .unwrap();
        assert!(matches!(ok[0], Recipient::Phone(_)));
        assert!(matches!(ok[1], Recipient::Phone(_)));
        assert!(matches!(ok[2], Recipient::User(_)));
        assert_eq!(field(&people(Vec::new()).unwrap_err()), "users");
        assert_eq!(
            field(&people(vec![phone("+16505551234"); 9]).unwrap_err()),
            "users"
        );
        assert!(people(vec![phone("+16505551234"); 8]).is_ok());
        assert_eq!(
            field(&people(vec![phone("16505551234")]).unwrap_err()),
            "users[0].phone",
            "a phone has its `+`; digits alone are a wa_id"
        );
        let wa = |id: &str| Person {
            wa_id: Some(id.to_owned()),
            ..Person::default()
        };
        for bad in ["+16505551234", "0123", "1234567890123456", ""] {
            assert_eq!(field(&people(vec![wa(bad)]).unwrap_err()), "users[0].wa_id");
        }
        let both = Person {
            phone: Some("+16505551234".to_owned()),
            user_id: Some("US.1".to_owned()),
            ..Person::default()
        };
        assert_eq!(field(&people(vec![both]).unwrap_err()), "users[0]");
        assert_eq!(
            field(&people(vec![Person::default()]).unwrap_err()),
            "users[0]"
        );
        let user = |id: &str| Person {
            user_id: Some(id.to_owned()),
            ..Person::default()
        };
        for bad in ["US.ENT.11815799212886844830", "13491208655302741918"] {
            assert_eq!(
                field(&people(vec![phone("+16505551234"), user(bad)]).unwrap_err()),
                "users[1].user_id"
            );
        }
    }

    /// Join request ids: 1 to 100, each visible ASCII.
    #[test]
    fn join_request_ids_are_one_to_a_hundred_visible_ids() {
        assert!(join_request_ids(vec!["MTY0NjcwNDM1OTU6MTIwMzYzNDA0Njk0MjMzODIw".into()]).is_ok());
        assert_eq!(
            field(&join_request_ids(Vec::new()).unwrap_err()),
            "join_request_ids"
        );
        assert_eq!(
            field(&join_request_ids(vec!["a".to_owned(); 101]).unwrap_err()),
            "join_request_ids"
        );
        assert_eq!(
            field(&join_request_ids(vec!["a".into(), "b c".into()]).unwrap_err()),
            "join_request_ids[1]"
        );
    }

    /// The group fields are the six scalar ones, all by default; the
    /// participants are not one of them.
    #[test]
    fn the_group_fields_are_the_scalar_ones() {
        assert_eq!(group_fields(None).unwrap().len(), 6);
        assert_eq!(group_fields(Some("")).unwrap().len(), 6);
        assert_eq!(
            group_fields(Some("subject,subject,suspended")).unwrap(),
            [GroupField::Subject, GroupField::Suspended]
        );
        assert_eq!(
            field(&group_fields(Some("participants")).unwrap_err()),
            "fields"
        );
    }

    /// An analytics range reaches back 90 days, no more, and ends after it
    /// starts; its metrics are Meta's five. Decisive: each bound.
    #[test]
    fn a_group_analytics_query_is_bounded() {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let day = Duration::days(1);
        let ok = analytics_query(now - day * 3, now - day, "SENT,READ,SENT", now).unwrap();
        assert_eq!(ok.2, [GroupMetric::Sent, GroupMetric::Read]);
        assert!(analytics_query(now - Duration::days(90), now, "SENT", now).is_ok());
        assert_eq!(
            field(&analytics_query(now - Duration::days(91), now, "SENT", now).unwrap_err()),
            "start"
        );
        assert_eq!(
            field(&analytics_query(now - day, now - day, "SENT", now).unwrap_err()),
            "end"
        );
        for bad in ["", "sent", "SENT,BOGUS"] {
            assert_eq!(
                field(&analytics_query(now - day, now, bad, now).unwrap_err()),
                "metrics",
                "{bad:?}"
            );
        }
    }

    /// An id that cannot be a group's, and the record's key: a hash that
    /// holds neither the group's id nor another number's.
    #[test]
    fn a_group_id_is_visible_ascii_and_its_record_key_hides_it() {
        assert!(group_id("Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD").is_some());
        for bad in ["", "a b", "a\nb", "é", &"x".repeat(513)] {
            assert!(group_id(bad).is_none(), "{bad:?}");
        }
        let pn = |s: &str| PhoneNumberId::new(s).unwrap();
        let group = GroupId::new("Y2FwaV9ncm91cDox");
        let key = GroupRecords::key(&pn("106540352242922"), &group);
        assert!(!key.contains("Y2Fw"));
        assert!(key.starts_with("106540352242922."));
        assert_ne!(key, GroupRecords::key(&pn("106540352242923"), &group));
        assert_ne!(
            key,
            GroupRecords::key(&pn("106540352242922"), &GroupId::new("other"))
        );
    }
    /// The stored names are pinned (they are in stored data once released,
    /// pending the owner's confirmation): the namespace, and the key of a
    /// record, by a known answer (the group's id is hashed with the number's
    /// under a domain, never stored in clear).
    #[test]
    fn the_namespace_and_the_record_key_are_pinned() {
        assert_eq!(NAMESPACE, "wa.server.group");
        let key = GroupRecords::key(
            &PhoneNumberId::new("106540352242922").unwrap(),
            &GroupId::new("Y2FwaV9ncm91cDox"),
        );
        assert_eq!(
            key,
            "106540352242922.1bd826a5cbb5abb66e438945d54e62079cc22bdd0f11627c880307ab33ded3d4"
        );
    }

    /// What a webhook event does to the records: a group created for the
    /// tenant's number is kept (under that tenant and number), a creation
    /// that failed is not, a deletion that failed leaves it and one that
    /// succeeded forgets it, and no other event moves anything. Decisive:
    /// each arm of `GroupRecords::on_event`.
    #[tokio::test]
    async fn an_event_keeps_forgets_or_leaves_a_group() {
        use meta_whatsapp_rs::adapters::store::MemoryKvStore;
        use meta_whatsapp_rs::webhooks::fields::GroupUpdate;
        let records = GroupRecords::new(Arc::new(MemoryKvStore::new()));
        let tenant = TenantId::parse("merchant-42").unwrap();
        let pn = PhoneNumberId::new("106540352242922").unwrap();
        let group = GroupId::new("Y2FwaV9ncm91cDox");
        let held = |records: &GroupRecords| {
            let records = records.clone();
            let (pn, group) = (pn.clone(), group.clone());
            async move {
                records
                    .store
                    .get(&GroupRecords::key(&pn, &group))
                    .await
                    .unwrap()
                    .is_some()
            }
        };
        let event = |kind: &str, errors: bool| {
            let mut update = serde_json::json!({
                "group_id": group.as_str(), "type": kind, "timestamp": "1750101000"});
            if errors {
                update["errors"] = serde_json::json!([{"code": 131_211, "message": "x"}]);
            }
            let update: GroupUpdate = serde_json::from_value(update).unwrap();
            WebhookEvent::GroupUpdated {
                waba_id: meta_whatsapp_rs::core::ids::WabaId::new("102290129340398").unwrap(),
                phone_number_id: pn.clone(),
                display_phone_number: "15550783881".to_owned(),
                field: "group_lifecycle_update".to_owned(),
                update: Box::new(update),
            }
        };
        for (kind, errors, then) in [
            ("group_create", true, false),
            ("group_participants_add", false, false),
            ("group_create", false, true),
            ("group_settings_update", false, true),
            ("group_delete", true, true),
            ("group_delete", false, false),
        ] {
            records
                .on_event(&tenant, &event(kind, errors))
                .await
                .unwrap();
            assert_eq!(held(&records).await, then, "{kind} (errors: {errors})");
        }
        // A number that is not a bound number's shape (an id a key's `.`
        // could be part of) is never keyed: nothing written or removed.
        let odd = PhoneNumberId::new_unchecked("106540352242922.abc");
        let mut forged = event("group_create", false);
        if let WebhookEvent::GroupUpdated {
            phone_number_id, ..
        } = &mut forged
        {
            *phone_number_id = odd.clone();
        }
        records.on_event(&tenant, &forged).await.unwrap();
        assert!(
            records
                .store
                .get(&GroupRecords::key(&odd, &group))
                .await
                .unwrap()
                .is_none()
        );
        // The record names the tenant and the number it was made for.
        records
            .on_event(&tenant, &event("group_create", false))
            .await
            .unwrap();
        let (record, _, _) = records
            .store
            .get(&GroupRecords::key(&pn, &group))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (record.tenant.as_str(), record.phone_number_id.as_str()),
            ("merchant-42", "106540352242922")
        );
    }
}
