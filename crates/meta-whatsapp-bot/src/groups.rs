//! The client's group operations through a [`Pacer`]: each waits for a
//! slot of the business number before its request, in the budget the
//! number's broadcasts, bot replies, read receipts and typing indicators
//! share (typing indicators: [`crate::PacedOutbound`]).
//!
//! [`PacedGroups`] wraps `client.groups(number)` (create, list, pin,
//! unpin); [`PacedGroup`] wraps `client.group(id)` (info, settings,
//! picture, delete, invite link, join requests, participants). Each method
//! is the client's, after [`Pacer::acquire`]: same arguments, same result,
//! and a limiter that fails fails the call before any request.
//!
//! Meta documents no rate for group operations (`groups`, `groups/reference`):
//! pacing them in the number's message budget is a choice, not a rule of
//! Meta's. One slot per call: the client's own retries of a call (its
//! `RetryPolicy`) go out inside it, without a slot of their own. To pace
//! every request, wrap the groups of a client that does not retry
//! (`client.clone().with_retry(RetryPolicy::NONE).groups(number)`) and
//! retry yourself after [`Pacer::acquire`]. The paging streams (`Groups::list_stream`,
//! `Group::join_requests_stream`) are not wrapped: page with
//! [`PacedGroups::list`] and [`PacedGroup::join_requests`], one slot a
//! page.

use bytes::Bytes;
use meta_whatsapp_client::groups::{
    ApprovedJoinRequests, CreateGroup, Group, GroupCreated, GroupField, GroupInfo,
    GroupSettingsUpdate, GroupSummary, Groups, InviteLink, JoinRequest, ListGroups,
    ListJoinRequests, ParticipantsOutcome, PinResponse, RejectedJoinRequests,
};
use meta_whatsapp_core::Result;
use meta_whatsapp_core::ids::{GroupId, MessageId, PhoneNumberId};
use meta_whatsapp_core::paging::Page;
use meta_whatsapp_core::recipient::Recipient;

use crate::pacer::Pacer;

/// A business number's group operations (`client.groups(number)`), each
/// after a slot of the number's [`Pacer`]. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct PacedGroups {
    groups: Groups,
    pacer: Pacer,
}

impl PacedGroups {
    /// Pace `groups` (`client.groups(number)`) with `pacer`, in `number`'s
    /// budget.
    pub fn new(groups: Groups, pacer: Pacer) -> Self {
        Self { groups, pacer }
    }

    /// The client's operations, unpaced.
    pub fn inner(&self) -> &Groups {
        &self.groups
    }

    /// One group's operations, paced in this number's budget.
    pub fn group(&self, group_id: impl Into<GroupId>) -> PacedGroup {
        PacedGroup::new(
            self.groups.group(group_id),
            self.groups.id().clone(),
            self.pacer.clone(),
        )
    }

    /// `Groups::create`, after a slot.
    pub async fn create(&self, request: &CreateGroup) -> Result<GroupCreated> {
        self.pacer.acquire(self.groups.id()).await?;
        self.groups.create(request).await
    }

    /// `Groups::list` (one page), after a slot.
    pub async fn list(&self, query: &ListGroups) -> Result<Page<GroupSummary>> {
        self.pacer.acquire(self.groups.id()).await?;
        self.groups.list(query).await
    }

    /// `Groups::pin_message`, after a slot.
    pub async fn pin_message(
        &self,
        group_id: &GroupId,
        message_id: &MessageId,
        expiration_days: u8,
    ) -> Result<PinResponse> {
        self.pacer.acquire(self.groups.id()).await?;
        self.groups
            .pin_message(group_id, message_id, expiration_days)
            .await
    }

    /// `Groups::unpin_message`, after a slot.
    pub async fn unpin_message(
        &self,
        group_id: &GroupId,
        message_id: &MessageId,
    ) -> Result<PinResponse> {
        self.pacer.acquire(self.groups.id()).await?;
        self.groups.unpin_message(group_id, message_id).await
    }
}

/// One group's operations (`client.group(id)`), each after a slot of the
/// business number that runs the group. See the [module docs](self).
#[derive(Debug, Clone)]
pub struct PacedGroup {
    group: Group,
    from: PhoneNumberId,
    pacer: Pacer,
}

impl PacedGroup {
    /// Pace `group` (`client.group(id)`) with `pacer`, in the budget of
    /// `from`, the business number that runs it (its path names the group
    /// only, so the number is given here).
    pub fn new(group: Group, from: impl Into<PhoneNumberId>, pacer: Pacer) -> Self {
        Self {
            group,
            from: from.into(),
            pacer,
        }
    }

    /// The client's operations, unpaced.
    pub fn inner(&self) -> &Group {
        &self.group
    }

    /// The number whose budget this group's operations take.
    pub fn number(&self) -> &PhoneNumberId {
        &self.from
    }

    /// `Group::info`, after a slot.
    pub async fn info(&self, fields: &[GroupField]) -> Result<GroupInfo> {
        self.pacer.acquire(&self.from).await?;
        self.group.info(fields).await
    }

    /// `Group::update`, after a slot.
    pub async fn update(&self, update: &GroupSettingsUpdate) -> Result<()> {
        self.pacer.acquire(&self.from).await?;
        self.group.update(update).await
    }

    /// `Group::set_picture`, after a slot.
    pub async fn set_picture(&self, jpeg: impl Into<Bytes>) -> Result<()> {
        let jpeg = jpeg.into();
        self.pacer.acquire(&self.from).await?;
        self.group.set_picture(jpeg).await
    }

    /// `Group::delete`, after a slot.
    pub async fn delete(&self) -> Result<()> {
        self.pacer.acquire(&self.from).await?;
        self.group.delete().await
    }

    /// `Group::invite_link`, after a slot.
    pub async fn invite_link(&self) -> Result<InviteLink> {
        self.pacer.acquire(&self.from).await?;
        self.group.invite_link().await
    }

    /// `Group::reset_invite_link`, after a slot.
    pub async fn reset_invite_link(&self) -> Result<InviteLink> {
        self.pacer.acquire(&self.from).await?;
        self.group.reset_invite_link().await
    }

    /// `Group::delete_invite_link`, after a slot.
    pub async fn delete_invite_link(&self) -> Result<()> {
        self.pacer.acquire(&self.from).await?;
        self.group.delete_invite_link().await
    }

    /// `Group::join_requests` (one page), after a slot.
    pub async fn join_requests(&self, query: &ListJoinRequests) -> Result<Page<JoinRequest>> {
        self.pacer.acquire(&self.from).await?;
        self.group.join_requests(query).await
    }

    /// `Group::approve_join_requests`, after a slot.
    pub async fn approve_join_requests<S: AsRef<str>>(
        &self,
        join_request_ids: &[S],
    ) -> Result<ApprovedJoinRequests> {
        self.pacer.acquire(&self.from).await?;
        self.group.approve_join_requests(join_request_ids).await
    }

    /// `Group::reject_join_requests`, after a slot.
    pub async fn reject_join_requests<S: AsRef<str>>(
        &self,
        join_request_ids: &[S],
    ) -> Result<RejectedJoinRequests> {
        self.pacer.acquire(&self.from).await?;
        self.group.reject_join_requests(join_request_ids).await
    }

    /// `Group::add_participants`, after a slot.
    pub async fn add_participants(&self, users: &[Recipient]) -> Result<ParticipantsOutcome> {
        self.pacer.acquire(&self.from).await?;
        self.group.add_participants(users).await
    }

    /// `Group::remove_participants`, after a slot.
    pub async fn remove_participants(&self, users: &[Recipient]) -> Result<ParticipantsOutcome> {
        self.pacer.acquire(&self.from).await?;
        self.group.remove_participants(users).await
    }
}
