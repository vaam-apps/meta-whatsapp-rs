//! Groups API: create and list groups, group info and settings, invite
//! links, join requests, participants, and pinning group messages.
//!
//! Docs:
//! - `groups`, `groups/get-started`, `groups/faq`, `groups/error-codes`,
//!   `groups/pricing` — overview, limits, error codes.
//! - `groups/reference` — every management operation below.
//! - `groups/groups-messaging#pin-and-unpin-group-message` — pin/unpin.
//!   Sending ordinary messages to a group is `crate::messages` with a
//!   [`Recipient::Group`]; group webhooks are `wa-webhooks`.
//! - `reference/whatsapp-business-phone-number/groups-management-api` —
//!   `GET`/`POST /{phone-number-id}/groups`.
//! - `reference/groups/groups-query-api` — `GET`/`POST`/`DELETE /{group-id}`.
//! - `reference/groups/groups-invite-link-api`,
//!   `reference/groups/groups-join-requests-api`,
//!   `reference/groups/groups-participants-api`.
//! - `business-scoped-user-ids#groups-api` — `user_id`, `parent_user_id`,
//!   `username` in group info and join requests; `user_id` to remove a
//!   participant.
//!
//! Phone-number-scoped operations hang off [`Client::groups`]; operations on
//! one group hang off [`Client::group`] (or [`Groups::group`]), since their
//! paths start at the group id.
//!
//! Limits enforced locally: subject 1–128 characters (after trimming, which
//! Meta does), description ≤ 2,048 characters, 1–8 participants per
//! request, list page size 1–1,024, pin duration 1–30 days, group picture a
//! non-empty JPEG of at most 5 MiB. Square shape and the 192×192 minimum
//! need an image decoder and are left to Meta (`131209`, `131210`).
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use bytes::Bytes;
use futures::{Stream, StreamExt, future, stream};
use serde::{Deserialize, Deserializer, Serialize};
use time::OffsetDateTime;
use wa_core::error::{ValidationError, snippet};
use wa_core::ids::{GroupId, MessageId, PhoneNumberId, UserId, WaId};
use wa_core::paging::{Page, Paging};
use wa_core::recipient::Recipient;
use wa_core::transport::Multipart;
use wa_core::{Error, GraphApiError, Result};

use crate::{Client, GraphRequest};

#[cfg(test)]
mod tests;

/// Maximum subject length, in characters (`groups/reference#create-group`).
pub const SUBJECT_MAX_CHARS: usize = 128;
/// Maximum description length, in characters.
pub const DESCRIPTION_MAX_CHARS: usize = 2048;
/// Most participants one add/remove request may name
/// (`groups/reference#remove-group-participants`).
pub const MAX_PARTICIPANTS_PER_REQUEST: usize = 8;
/// Largest page size of the active-groups list (`limit` max 1024).
pub const LIST_LIMIT_MAX: u32 = 1024;
/// Longest pin, in days (`groups/groups-messaging`, `expiration_days` 1–30).
pub const PIN_MAX_DAYS: u8 = 30;
/// Largest group picture, in bytes ("Maximum size: 5MB", read as MiB so a
/// picture Meta accepts is never refused locally).
pub const PICTURE_MAX_BYTES: usize = 5 * 1024 * 1024;

/// Entry point, see [`Client::groups`].
#[derive(Debug, Clone)]
pub struct Groups {
    client: Client,
    phone_number_id: PhoneNumberId,
}

/// One group, see [`Client::group`].
#[derive(Debug, Clone)]
pub struct Group {
    client: Client,
    group_id: GroupId,
}

impl Client {
    /// [`Groups`] API for `phone_number_id`.
    pub fn groups(&self, phone_number_id: impl Into<PhoneNumberId>) -> Groups {
        Groups {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }

    /// [`Group`] API for `group_id` (info, settings, invite link, join
    /// requests, participants, delete).
    pub fn group(&self, group_id: impl Into<GroupId>) -> Group {
        Group {
            client: self.clone(),
            group_id: group_id.into(),
        }
    }
}

impl Groups {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// [`Group`] API for `group_id`; same as [`Client::group`].
    pub fn group(&self, group_id: impl Into<GroupId>) -> Group {
        self.client.group(group_id)
    }

    fn path(&self) -> String {
        format!("{}/groups", self.phone_number_id)
    }

    /// Create a group: `POST /{phone-number-id}/groups`
    /// (`groups/reference#create-group`).
    ///
    /// Creation is asynchronous: the answer carries a `request_id`, and the
    /// group id and invite link arrive in a `group_lifecycle_update`
    /// webhook. Not idempotent: a replay would create a second group.
    pub async fn create(&self, request: &CreateGroup) -> Result<GroupCreated> {
        request.validate()?;
        self.client
            .post(&self.path())
            .json(&WithProduct::new(request))
            .context("create group response")
            .send()
            .await
    }

    /// One page of active groups: `GET /{phone-number-id}/groups`
    /// (`groups/reference#get-active-groups`).
    ///
    /// Meta nests this list as `{"data": {"groups": [...]}, "paging": …}`;
    /// it is returned as an ordinary [`Page`].
    pub async fn list(&self, query: &ListGroups) -> Result<Page<GroupSummary>> {
        validate_list_limit(query.limit)?;
        fetch_groups_page(
            &self.client,
            &self.path(),
            query.limit,
            query.after.as_deref(),
            query.before.as_deref(),
        )
        .await
    }

    /// Every active group, following `paging.cursors.after`.
    ///
    /// Implemented here rather than with [`GraphRequest::paginate`], which
    /// expects `data` to be the array. Like it, this re-issues the request
    /// with `after=` instead of following `paging.next`, so the token never
    /// leaves the configured endpoint. `query.after`/`query.before` must be
    /// `None`; otherwise (or on a bad `limit`) the stream yields that single
    /// validation error.
    pub fn list_stream(
        &self,
        query: &ListGroups,
    ) -> impl Stream<Item = Result<GroupSummary>> + Send + 'static {
        struct State {
            client: Client,
            path: String,
            limit: Option<u32>,
            after: Option<String>,
            buffer: std::vec::IntoIter<GroupSummary>,
            done: bool,
        }
        let checked = reject_cursors(query.after.as_deref(), query.before.as_deref())
            .and_then(|()| validate_list_limit(query.limit));
        if let Err(e) = checked {
            return stream::once(future::ready(Err(e))).right_stream();
        }
        let state = State {
            client: self.client.clone(),
            path: self.path(),
            limit: query.limit,
            after: None,
            buffer: Vec::new().into_iter(),
            done: false,
        };
        stream::unfold(state, |mut st| async move {
            loop {
                if let Some(item) = st.buffer.next() {
                    return Some((Ok(item), st));
                }
                if st.done {
                    return None;
                }
                let page =
                    fetch_groups_page(&st.client, &st.path, st.limit, st.after.as_deref(), None)
                        .await;
                match page {
                    Ok(page) => {
                        let next = page.next_cursor().map(str::to_owned);
                        st.done = next.is_none() || next == st.after;
                        st.after = next;
                        st.buffer = page.data.into_iter();
                    }
                    Err(e) => {
                        st.done = true;
                        return Some((Err(e), st));
                    }
                }
            }
        })
        .left_stream()
    }

    /// Pin a message in a group for 1–30 days: `POST
    /// /{phone-number-id}/messages` with `type: "pin"`
    /// (`groups/groups-messaging#pin-and-unpin-group-message`). Only a
    /// group admin can pin; at most three messages stay pinned (pinning a
    /// fourth unpins the oldest).
    pub async fn pin_message(
        &self,
        group_id: &GroupId,
        message_id: &MessageId,
        expiration_days: u8,
    ) -> Result<PinResponse> {
        if !(1..=PIN_MAX_DAYS).contains(&expiration_days) {
            return Err(ValidationError::new(
                "pin.expiration_days",
                format!("must be between 1 and {PIN_MAX_DAYS}"),
            )
            .into());
        }
        self.send_pin(
            group_id,
            message_id,
            PinOperation::Pin,
            Some(expiration_days),
        )
        .await
    }

    /// Unpin a group message (same endpoint, `pin.type: "unpin"`).
    pub async fn unpin_message(
        &self,
        group_id: &GroupId,
        message_id: &MessageId,
    ) -> Result<PinResponse> {
        self.send_pin(group_id, message_id, PinOperation::Unpin, None)
            .await
    }

    async fn send_pin(
        &self,
        group_id: &GroupId,
        message_id: &MessageId,
        operation: PinOperation,
        expiration_days: Option<u8>,
    ) -> Result<PinResponse> {
        validate_group_id(group_id)?;
        let body = PinBody {
            messaging_product: "whatsapp",
            to: Recipient::group(group_id.clone()),
            kind: "pin",
            pin: Pin {
                operation,
                message_id,
                expiration_days,
            },
        };
        self.client
            .post(&format!("{}/messages", self.phone_number_id))
            .json(&body)
            .context("pin group message response")
            .send()
            .await
    }
}

impl Group {
    /// The group this API is scoped to.
    pub fn id(&self) -> &GroupId {
        &self.group_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn path(&self, edge: Option<&str>) -> Result<String> {
        validate_group_id(&self.group_id)?;
        Ok(match edge {
            Some(edge) => format!("{}/{edge}", self.group_id),
            None => self.group_id.to_string(),
        })
    }

    /// Group metadata: `GET /{group-id}?fields=…`
    /// (`groups/reference#get-group-info`). With no `fields`, Meta returns
    /// only the id and `messaging_product`.
    pub async fn info(&self, fields: &[GroupField]) -> Result<GroupInfo> {
        let path = self.path(None)?;
        let fields = (!fields.is_empty()).then(|| {
            fields
                .iter()
                .map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(",")
        });
        self.client
            .get(&path)
            .query_opt("fields", fields)
            .context("group info response")
            .send()
            .await
    }

    /// Change subject and/or description: `POST /{group-id}`
    /// (`groups/reference#update-group-settings`). The outcome per field
    /// arrives in a `group_settings_update` webhook.
    ///
    /// Meta documents no response body; any 2xx is success unless the body
    /// says `"success": false`. Marked idempotent: it sets fields to values.
    pub async fn update(&self, update: &GroupSettingsUpdate) -> Result<()> {
        update.validate()?;
        let request = self
            .client
            .post(&self.path(None)?)
            .json(&WithProduct::new(update))
            .idempotent(true)
            .context("update group settings response");
        send_expecting_success(request).await
    }

    /// Replace the group picture: `POST /{group-id}` as
    /// `multipart/form-data` with `messaging_product` and `file`, "the same
    /// request structure as the Upload Media endpoint"
    /// (`groups/reference#update-group-settings`).
    ///
    /// `jpeg` must be a JPEG (the only accepted type) of at most 5 MiB; it
    /// must also be square and at least 192×192, which Meta checks.
    pub async fn set_picture(&self, jpeg: impl Into<Bytes>) -> Result<()> {
        let jpeg = jpeg.into();
        validate_picture(&jpeg)?;
        let form = Multipart::new().text("messaging_product", "whatsapp").file(
            "file",
            "group-picture.jpg",
            "image/jpeg",
            jpeg,
        );
        let request = self
            .client
            .post(&self.path(None)?)
            .multipart(form)
            .idempotent(true)
            .context("set group picture response");
        send_expecting_success(request).await
    }

    /// Delete the group and remove every participant, the business
    /// included: `DELETE /{group-id}` (`groups/reference#delete-group`).
    pub async fn delete(&self) -> Result<()> {
        self.client
            .delete(&self.path(None)?)
            .context("delete group response")
            .send_success()
            .await
    }

    /// Current invite link: `GET /{group-id}/invite_link`.
    pub async fn invite_link(&self) -> Result<InviteLink> {
        self.client
            .get(&self.path(Some("invite_link"))?)
            .context("get group invite link response")
            .send()
            .await
    }

    /// Replace the invite link; every previous link stops working:
    /// `POST /{group-id}/invite_link`. Not idempotent: each call issues a
    /// new link.
    pub async fn reset_invite_link(&self) -> Result<InviteLink> {
        self.client
            .post(&self.path(Some("invite_link"))?)
            .json(&ProductOnly::WHATSAPP)
            .context("reset group invite link response")
            .send()
            .await
    }

    /// Delete the invite link: `DELETE /{group-id}/invite_link`
    /// (`reference/groups/groups-invite-link-api`). The reference types the
    /// answer's `success` as a string, so both `true` and `"true"` count.
    pub async fn delete_invite_link(&self) -> Result<()> {
        let request = self
            .client
            .delete(&self.path(Some("invite_link"))?)
            .json(&ProductOnly::WHATSAPP)
            .context("delete group invite link response");
        send_expecting_success(request).await
    }

    /// One page of open join requests: `GET /{group-id}/join_requests`
    /// (`groups/reference#get-join-requests`).
    pub async fn join_requests(&self, query: &ListJoinRequests) -> Result<Page<JoinRequest>> {
        self.join_requests_request()?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every open join request, following `paging.cursors.after`.
    /// `query.after`/`query.before` must be `None`; otherwise the stream
    /// yields that single validation error.
    pub fn join_requests_stream(
        &self,
        query: &ListJoinRequests,
    ) -> impl Stream<Item = Result<JoinRequest>> + Send + 'static {
        let request = reject_cursors(query.after.as_deref(), query.before.as_deref())
            .and_then(|()| self.join_requests_request());
        match request {
            Ok(req) => req.paginate::<JoinRequest>().left_stream(),
            Err(e) => stream::once(future::ready(Err(e))).right_stream(),
        }
    }

    fn join_requests_request(&self) -> Result<GraphRequest> {
        Ok(self
            .client
            .get(&self.path(Some("join_requests"))?)
            .context("list join requests response"))
    }

    /// Approve join requests: `POST /{group-id}/join_requests`
    /// (`groups/reference#approve-join-requests`). A partial success comes
    /// back as `206` with the failures listed; it is not an error.
    pub async fn approve_join_requests<S: AsRef<str>>(
        &self,
        join_request_ids: &[S],
    ) -> Result<ApprovedJoinRequests> {
        let body = JoinRequestsBody::new(join_request_ids)?;
        self.client
            .post(&self.path(Some("join_requests"))?)
            .json(&body)
            .context("approve join requests response")
            .send()
            .await
    }

    /// Reject join requests: `DELETE /{group-id}/join_requests`
    /// (`groups/reference#reject-join-requests`). The users see "Request to
    /// join" again.
    pub async fn reject_join_requests<S: AsRef<str>>(
        &self,
        join_request_ids: &[S],
    ) -> Result<RejectedJoinRequests> {
        let body = JoinRequestsBody::new(join_request_ids)?;
        self.client
            .delete(&self.path(Some("join_requests"))?)
            .json(&body)
            .context("reject join requests response")
            .send()
            .await
    }

    /// Add participants: `POST /{group-id}/participants`
    /// (`reference/groups/groups-participants-api`).
    ///
    /// The management guide says participants cannot be added manually and
    /// join through invite links; the endpoint is in the reference, so it is
    /// wrapped, but expect Meta to refuse it unless your number is enabled
    /// for it. Only phone numbers are documented here (`user`), so
    /// BSUID-only recipients are refused locally. See
    /// [`ParticipantsOutcome`] for what the answer tells you.
    pub async fn add_participants(&self, users: &[Recipient]) -> Result<ParticipantsOutcome> {
        let body = ParticipantsBody::new(users, ParticipantsOp::Add)?;
        let request = self
            .client
            .post(&self.path(Some("participants"))?)
            .json(&body)
            .context("add group participants response");
        Ok(ParticipantsOutcome::from_response(
            send_lenient(request).await?,
        ))
    }

    /// Remove participants: `DELETE /{group-id}/participants`
    /// (`groups/reference#remove-group-participants`). Each user is named by
    /// phone number (`user`) or BSUID (`user_id`) — not both. A removed user
    /// can no longer join through an invite link. The per-user outcome
    /// arrives in a `group_participants_update` webhook; see
    /// [`ParticipantsOutcome`] for what the answer tells you.
    pub async fn remove_participants(&self, users: &[Recipient]) -> Result<ParticipantsOutcome> {
        let body = ParticipantsBody::new(users, ParticipantsOp::Remove)?;
        let request = self
            .client
            .delete(&self.path(Some("participants"))?)
            .json(&body)
            .context("remove group participants response");
        Ok(ParticipantsOutcome::from_response(
            send_lenient(request).await?,
        ))
    }
}

/// Answer of [`Group::add_participants`] / [`Group::remove_participants`].
///
/// Meta documents no response body for these endpoints, only that a
/// partial success is HTTP `206` with code `131201` ("Not all
/// participant-level operations in the request succeeded",
/// `groups/error-codes`). So this reports that, and keeps the body as sent
/// rather than guessing field names. An explicit `"success": false` is an
/// [`Error::Http`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantsOutcome {
    /// `true` on HTTP 206: some participants were not processed; the
    /// `group_participants_update` webhook says which.
    pub partial: bool,
    /// The response body (`Null` when empty or not JSON).
    pub raw: serde_json::Value,
}

impl ParticipantsOutcome {
    fn from_response((status, raw): (u16, serde_json::Value)) -> Self {
        Self {
            partial: status == 206,
            raw,
        }
    }
}

async fn fetch_groups_page(
    client: &Client,
    path: &str,
    limit: Option<u32>,
    after: Option<&str>,
    before: Option<&str>,
) -> Result<Page<GroupSummary>> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        data: RawData,
        #[serde(default)]
        paging: Option<Paging>,
    }
    #[derive(Default, Deserialize)]
    struct RawData {
        #[serde(default)]
        groups: Vec<GroupSummary>,
    }
    let raw: Raw = client
        .get(path)
        .query_opt("limit", limit)
        .query_opt("after", after)
        .query_opt("before", before)
        .context("list groups response")
        .send()
        .await?;
    Ok(Page {
        data: raw.data.groups,
        paging: raw.paging,
        summary: None,
    })
}

/// For endpoints whose answer Meta does not document (or types `success` as
/// a string): a 2xx is success unless the body explicitly says
/// `"success": false`.
async fn send_expecting_success(request: GraphRequest) -> Result<()> {
    send_lenient(request).await.map(|_| ())
}

/// [`send_expecting_success`], keeping the status and the body (JSON, or
/// `Null` when empty or not JSON).
async fn send_lenient(request: GraphRequest) -> Result<(u16, serde_json::Value)> {
    let resp = request.send_raw().await?;
    let status = resp.status.as_u16();
    let body =
        serde_json::from_slice::<serde_json::Value>(&resp.body).unwrap_or(serde_json::Value::Null);
    let refused = match body.get("success") {
        Some(serde_json::Value::Bool(ok)) => !ok,
        Some(serde_json::Value::String(s)) => s.eq_ignore_ascii_case("false"),
        _ => false,
    };
    if refused {
        return Err(Error::Http {
            status,
            body_snippet: snippet(&resp.body),
        });
    }
    Ok((status, body))
}

/// The group id is the first path segment: an empty id, or one containing
/// `/` (which the endpoint would split into two segments), would send the
/// request — a `DELETE`, possibly — to a different Graph object.
fn validate_group_id(group_id: &GroupId) -> Result<()> {
    let id = group_id.as_str();
    if id.is_empty() || id.contains('/') {
        return Err(
            ValidationError::new("group_id", "must be non-empty and contain no `/`").into(),
        );
    }
    Ok(())
}

fn validate_list_limit(limit: Option<u32>) -> Result<()> {
    match limit {
        Some(l) if !(1..=LIST_LIMIT_MAX).contains(&l) => Err(ValidationError::new(
            "limit",
            format!("must be between 1 and {LIST_LIMIT_MAX}"),
        )
        .into()),
        _ => Ok(()),
    }
}

fn reject_cursors(after: Option<&str>, before: Option<&str>) -> Result<()> {
    if after.is_some() {
        return Err(ValidationError::new("after", "streams manage cursors; leave it unset").into());
    }
    if before.is_some() {
        return Err(
            ValidationError::new("before", "streams manage cursors; leave it unset").into(),
        );
    }
    Ok(())
}

fn validate_subject(subject: &str) -> Result<()> {
    let len = subject.trim().chars().count();
    if len == 0 {
        return Err(ValidationError::new("subject", "must not be empty").into());
    }
    if len > SUBJECT_MAX_CHARS {
        return Err(ValidationError::new(
            "subject",
            format!("must be at most {SUBJECT_MAX_CHARS} characters, got {len}"),
        )
        .into());
    }
    Ok(())
}

fn validate_description(description: Option<&str>) -> Result<()> {
    let len = description.map_or(0, |d| d.chars().count());
    if len > DESCRIPTION_MAX_CHARS {
        return Err(ValidationError::new(
            "description",
            format!("must be at most {DESCRIPTION_MAX_CHARS} characters, got {len}"),
        )
        .into());
    }
    Ok(())
}

fn validate_picture(jpeg: &[u8]) -> Result<()> {
    if jpeg.len() > PICTURE_MAX_BYTES {
        return Err(ValidationError::new(
            "file",
            format!(
                "must be at most {PICTURE_MAX_BYTES} bytes, got {}",
                jpeg.len()
            ),
        )
        .into());
    }
    // SOI marker + the first segment's marker prefix; also rejects an empty
    // file. Meta accepts only image/jpeg, so a PNG named .jpg would
    // otherwise be refused only after the upload.
    if !jpeg.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Err(ValidationError::new("file", "must be a JPEG image").into());
    }
    Ok(())
}

/// `{"messaging_product": "whatsapp", ...fields of T}`.
#[derive(Serialize)]
struct WithProduct<'a, T: Serialize> {
    messaging_product: &'static str,
    #[serde(flatten)]
    inner: &'a T,
}

impl<'a, T: Serialize> WithProduct<'a, T> {
    fn new(inner: &'a T) -> Self {
        Self {
            messaging_product: "whatsapp",
            inner,
        }
    }
}

#[derive(Serialize)]
struct ProductOnly {
    messaging_product: &'static str,
}

impl ProductOnly {
    const WHATSAPP: Self = Self {
        messaging_product: "whatsapp",
    };
}

#[derive(Serialize)]
struct JoinRequestsBody<'a> {
    messaging_product: &'static str,
    join_requests: Vec<&'a str>,
}

impl<'a> JoinRequestsBody<'a> {
    fn new<S: AsRef<str>>(ids: &'a [S]) -> Result<Self> {
        if ids.is_empty() {
            return Err(ValidationError::new(
                "join_requests",
                "must list at least one join request",
            )
            .into());
        }
        Ok(Self {
            messaging_product: "whatsapp",
            join_requests: ids.iter().map(AsRef::as_ref).collect(),
        })
    }
}

#[derive(Clone, Copy)]
enum ParticipantsOp {
    Add,
    Remove,
}

#[derive(Serialize)]
struct ParticipantsBody<'a> {
    messaging_product: &'static str,
    participants: Vec<ParticipantEntry<'a>>,
}

#[derive(Serialize)]
struct ParticipantEntry<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<&'a UserId>,
}

impl<'a> ParticipantsBody<'a> {
    fn new(users: &'a [Recipient], op: ParticipantsOp) -> Result<Self> {
        if users.is_empty() {
            return Err(ValidationError::new("participants", "must list at least one user").into());
        }
        if users.len() > MAX_PARTICIPANTS_PER_REQUEST {
            return Err(ValidationError::new(
                "participants",
                format!(
                    "at most {MAX_PARTICIPANTS_PER_REQUEST} users per request, got {}",
                    users.len()
                ),
            )
            .into());
        }
        let participants = users
            .iter()
            .enumerate()
            .map(|(i, user)| {
                let field = || format!("participants[{i}]");
                match (user, op) {
                    (Recipient::Phone(phone), _) => Ok(ParticipantEntry {
                        user: Some(phone.as_str()),
                        user_id: None,
                    }),
                    (Recipient::User(id), ParticipantsOp::Remove) => Ok(ParticipantEntry {
                        user: None,
                        user_id: Some(id),
                    }),
                    (Recipient::User(_), ParticipantsOp::Add) => Err(ValidationError::new(
                        field(),
                        "adding participants by BSUID is not documented; use a phone number",
                    )),
                    (Recipient::PhoneAndUser { .. }, _) => Err(ValidationError::new(
                        field(),
                        "name a participant by phone number or BSUID, not both",
                    )),
                    _ => Err(ValidationError::new(
                        field(),
                        "only individual users can be group participants",
                    )),
                }
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Self {
            messaging_product: "whatsapp",
            participants,
        })
    }
}

#[derive(Serialize)]
struct PinBody<'a> {
    messaging_product: &'static str,
    #[serde(flatten)]
    to: Recipient,
    #[serde(rename = "type")]
    kind: &'static str,
    pin: Pin<'a>,
}

#[derive(Serialize)]
struct Pin<'a> {
    #[serde(rename = "type")]
    operation: PinOperation,
    message_id: &'a MessageId,
    #[serde(skip_serializing_if = "Option::is_none")]
    expiration_days: Option<u8>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum PinOperation {
    Pin,
    Unpin,
}

/// Body of [`Groups::create`], minus `messaging_product` (added for you).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateGroup {
    /// Subject, 1–128 characters; Meta trims surrounding whitespace.
    pub subject: String,
    /// Description, at most 2,048 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether joining needs approval; Meta's default is
    /// [`JoinApprovalMode::AutoApprove`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_approval_mode: Option<JoinApprovalMode>,
}

impl CreateGroup {
    /// A group with `subject` and Meta's defaults for the rest.
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            description: None,
            join_approval_mode: None,
        }
    }

    fn validate(&self) -> Result<()> {
        validate_subject(&self.subject)?;
        validate_description(self.description.as_deref())
    }
}

/// Answer of [`Groups::create`]. The group id is not in it: it arrives in
/// the `group_lifecycle_update` webhook, correlated by `request_id`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GroupCreated {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// Group creation request id.
    #[serde(default)]
    pub request_id: Option<String>,
}

/// Body of [`Group::update`], minus `messaging_product`. `None` fields are
/// not sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GroupSettingsUpdate {
    /// New subject, 1–128 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// New description, at most 2,048 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl GroupSettingsUpdate {
    fn validate(&self) -> Result<()> {
        if let Some(subject) = &self.subject {
            validate_subject(subject)?;
        }
        validate_description(self.description.as_deref())
    }
}

/// Whether users who open the invite link join directly or ask first.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinApprovalMode {
    /// `approval_required`: users send a join request you approve or
    /// reject.
    ApprovalRequired,
    /// `auto_approve`: users join directly.
    AutoApprove,
    /// A value this crate does not know yet, kept verbatim.
    #[serde(untagged)]
    Other(String),
}

/// Query of [`Groups::list`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListGroups {
    /// Page size, 1–1024 (Meta's default is 25).
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

/// An active group, as listed by [`Groups::list`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GroupSummary {
    /// Group id.
    pub id: GroupId,
    /// Subject.
    #[serde(default)]
    pub subject: Option<String>,
    /// Creation time (unix seconds, sent as a string or a number).
    #[serde(default, with = "wa_core::timestamp::unix_option")]
    pub created_at: Option<OffsetDateTime>,
}

/// A field of [`Group::info`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GroupField {
    /// `subject`.
    Subject,
    /// `description`.
    Description,
    /// `participants`.
    Participants,
    /// `join_approval_mode`.
    JoinApprovalMode,
    /// `suspended`.
    Suspended,
    /// `creation_timestamp`.
    CreationTimestamp,
    /// `total_participant_count`.
    TotalParticipantCount,
}

impl GroupField {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Description => "description",
            Self::Participants => "participants",
            Self::JoinApprovalMode => "join_approval_mode",
            Self::Suspended => "suspended",
            Self::CreationTimestamp => "creation_timestamp",
            Self::TotalParticipantCount => "total_participant_count",
        }
    }
}

/// Group metadata (`GroupInfo`). Every field but the id depends on
/// `?fields=`.
///
/// The guide's sample quotes every value (`"<SUSPENDED>"`,
/// `"<TOTAL_PARTICIPANT_COUNT>"`), the schema types them boolean/integer;
/// both forms are accepted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct GroupInfo {
    /// Group id.
    #[serde(default)]
    pub id: Option<GroupId>,
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// Subject.
    #[serde(default)]
    pub subject: Option<String>,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// Whether WhatsApp suspended the group.
    #[serde(default, deserialize_with = "lenient_opt_bool")]
    pub suspended: Option<bool>,
    /// Creation time.
    #[serde(default, with = "wa_core::timestamp::unix_option")]
    pub creation_timestamp: Option<OffsetDateTime>,
    /// Participants (the business excluded).
    #[serde(default)]
    pub participants: Vec<GroupParticipant>,
    /// Participant count, the business excluded.
    #[serde(default, deserialize_with = "lenient_opt_u64")]
    pub total_participant_count: Option<u64>,
    /// Join approval mode.
    #[serde(default)]
    pub join_approval_mode: Option<JoinApprovalMode>,
}

/// A participant in [`GroupInfo::participants`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct GroupParticipant {
    /// Phone number; omitted when the user has a username and Meta cannot
    /// share the number.
    #[serde(default)]
    pub wa_id: Option<WaId>,
    /// BSUID.
    #[serde(default)]
    pub user_id: Option<UserId>,
    /// Parent BSUID, if your portfolio is enrolled for them.
    #[serde(default)]
    pub parent_user_id: Option<UserId>,
    /// Username, if the user adopted one.
    #[serde(default)]
    pub username: Option<String>,
}

/// A group invite link.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InviteLink {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// `https://chat.whatsapp.com/<LINK_ID>`.
    pub invite_link: String,
}

/// Query of [`Group::join_requests`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListJoinRequests {
    /// Cursor from a previous page's `paging.cursors.after`.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

/// An open request to join a group.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JoinRequest {
    /// Id to approve or reject.
    pub join_request_id: String,
    /// Phone number; may be omitted for username users.
    #[serde(default)]
    pub wa_id: Option<WaId>,
    /// BSUID.
    #[serde(default)]
    pub user_id: Option<UserId>,
    /// Parent BSUID, if enabled.
    #[serde(default)]
    pub parent_user_id: Option<UserId>,
    /// Username, if the user adopted one.
    #[serde(default)]
    pub username: Option<String>,
    /// When the request was made.
    #[serde(default, with = "wa_core::timestamp::unix_option")]
    pub creation_timestamp: Option<OffsetDateTime>,
}

/// Answer of [`Group::approve_join_requests`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ApprovedJoinRequests {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// Approved join request ids.
    #[serde(default)]
    pub approved_join_requests: Vec<String>,
    /// Requests that could not be approved.
    #[serde(default)]
    pub failed_join_requests: Vec<FailedJoinRequest>,
    /// Request-level errors.
    #[serde(default)]
    pub errors: Vec<GraphApiError>,
}

/// Answer of [`Group::reject_join_requests`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RejectedJoinRequests {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// Rejected join request ids.
    #[serde(default)]
    pub rejected_join_requests: Vec<String>,
    /// Requests that could not be rejected.
    #[serde(default)]
    pub failed_join_requests: Vec<FailedJoinRequest>,
    /// Request-level errors.
    #[serde(default)]
    pub errors: Vec<GraphApiError>,
}

/// A join request an approve/reject could not process.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FailedJoinRequest {
    /// The join request id.
    #[serde(default)]
    pub join_request_id: Option<String>,
    /// Why, e.g. `131203`. Branch on [`GraphApiError::code`].
    #[serde(default)]
    pub errors: Vec<GraphApiError>,
}

/// Answer of [`Groups::pin_message`] / [`Groups::unpin_message`], shaped
/// like a send-message answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PinResponse {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// The group, echoed.
    #[serde(default)]
    pub contacts: Vec<PinContact>,
    /// The pin message; status webhooks refer to its id.
    #[serde(default)]
    pub messages: Vec<PinMessage>,
}

/// `contacts[]` of a [`PinResponse`]; both fields hold the group id.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PinContact {
    /// What you sent in `to`.
    #[serde(default)]
    pub input: Option<String>,
    /// The group id again (not a phone number, despite the name).
    #[serde(default)]
    pub wa_id: Option<String>,
}

/// `messages[]` of a [`PinResponse`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PinMessage {
    /// Message id.
    pub id: MessageId,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BoolOrString {
    Bool(bool),
    Str(String),
}

fn lenient_opt_bool<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<bool>, D::Error> {
    match Option::<BoolOrString>::deserialize(d)? {
        None => Ok(None),
        Some(BoolOrString::Bool(b)) => Ok(Some(b)),
        Some(BoolOrString::Str(s)) => s
            .trim()
            .parse::<bool>()
            .map(Some)
            .map_err(|_| serde::de::Error::custom(format!("invalid boolean `{s}`"))),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum U64OrString {
    Num(u64),
    Str(String),
}

fn lenient_opt_u64<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<u64>, D::Error> {
    match Option::<U64OrString>::deserialize(d)? {
        None => Ok(None),
        Some(U64OrString::Num(n)) => Ok(Some(n)),
        Some(U64OrString::Str(s)) => s
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| serde::de::Error::custom(format!("invalid count `{s}`"))),
    }
}
