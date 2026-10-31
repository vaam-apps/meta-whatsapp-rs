//! The Groups API fields: `group_lifecycle_update`,
//! `group_participants_update`, `group_settings_update`,
//! `group_status_update`. All four share one value shape, a `groups[]` array
//! of events tagged by `type`.
//!
//! Doc paths: `groups/webhooks`, the groups section of
//! `business-scoped-user-ids`, `reference/webhooks/whatsapp-incoming-webhook-payload`.
//!
//! # Where the pages disagree
//!
//! - The incoming-payload reference names the field `group_participant_update`
//!   and the types `group_add_participants` / `group_remove_participants`;
//!   `groups/webhooks`, `webhooks/override` and `business-scoped-user-ids`
//!   say `group_participants_update`, `group_participants_add` and
//!   `group_participants_remove`. Both spellings are accepted.
//! - `groups/webhooks` places `"initiated_by"` after the group object in its
//!   remove examples (invalid JSON); `business-scoped-user-ids` puts it
//!   inside the object, which is what is modelled.
//! - Error `code`s are printed as `"ERROR_CODE"` strings there; they are
//!   integers everywhere else and parse into [`GraphApiError`].

use std::fmt;

use meta_whatsapp_core::GraphApiError;
use meta_whatsapp_core::ids::{GroupId, UserId, WaId};
use meta_whatsapp_core::secret::RedactedPath;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::common::Metadata;
use crate::open_enum::open_enum;

/// `value` of any `group_*` change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupsValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The business phone number that owns the groups.
    pub metadata: Metadata,
    /// Group events.
    #[serde(default)]
    pub groups: Vec<GroupUpdate>,
}

open_enum! {
    /// `groups[].type`.
    pub enum GroupUpdateType {
        /// Group created (or creation failed; see `errors`).
        GroupCreate => "group_create",
        /// Group deleted (or deletion failed).
        GroupDelete => "group_delete",
        /// Participants joined.
        ParticipantsAdd => "group_participants_add" | "group_add_participants",
        /// Participants removed or left.
        ParticipantsRemove => "group_participants_remove" | "group_remove_participants",
        /// A user asked to join.
        JoinRequestCreated => "group_join_request_created",
        /// A user cancelled their join request.
        JoinRequestRevoked => "group_join_request_revoked",
        /// Subject, description or picture changed.
        SettingsUpdate => "group_settings_update",
        /// Group suspended.
        Suspend => "group_suspend",
        /// Suspension cleared.
        SuspendCleared => "group_suspend_cleared",
    }
}

open_enum! {
    /// `groups[].initiated_by` on removals.
    pub enum GroupRemovalInitiator {
        /// The business removed the participant.
        Business => "business",
        /// The participant left.
        Participant => "participant",
    }
}

/// `groups[]`: one group event. Which properties are set depends on
/// [`Self::update_type`].
///
/// `Debug` shows [`invite_link`](Self::invite_link) as [`RedactedPath`]
/// does, `https://chat.whatsapp.com/<redacted>`: the code in its path lets
/// whoever holds it join (or ask to join) the group. The field keeps it
/// whole, and serializing writes it whole.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupUpdate {
    /// When the event happened.
    #[serde(
        default,
        with = "meta_whatsapp_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub timestamp: Option<OffsetDateTime>,
    /// The group.
    pub group_id: GroupId,
    /// Kind of event.
    #[serde(rename = "type")]
    pub update_type: GroupUpdateType,
    /// Id of the API request that triggered it, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Group subject (creation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Group description (creation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Invite link (creation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_link: Option<String>,
    /// Join approval mode (creation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_approval_mode: Option<String>,
    /// Why (e.g. `invite_link`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Who removed the participant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiated_by: Option<GroupRemovalInitiator>,
    /// Join request id (join requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join_request_id: Option<String>,
    /// Requesting user's phone number (join requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
    /// Requesting user's BSUID (join requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<UserId>,
    /// Requesting user's parent BSUID (join requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_user_id: Option<UserId>,
    /// Requesting user's username (join requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Participants added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_participants: Vec<GroupParticipant>,
    /// Participants removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_participants: Vec<GroupParticipant>,
    /// Participants that could not be removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_participants: Vec<GroupParticipant>,
    /// Profile picture change (settings updates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_picture: Option<GroupSettingChange>,
    /// Subject change (settings updates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_subject: Option<GroupSettingChange>,
    /// Description change (settings updates).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_description: Option<GroupSettingChange>,
    /// Why the request (partially) failed.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::graph_errors::deserialize",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub errors: Vec<GraphApiError>,
}

impl fmt::Debug for GroupUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Destructured, so a field added later has to be listed here.
        let Self {
            timestamp,
            group_id,
            update_type,
            request_id,
            subject,
            description,
            invite_link,
            join_approval_mode,
            reason,
            initiated_by,
            join_request_id,
            wa_id,
            user_id,
            parent_user_id,
            username,
            added_participants,
            removed_participants,
            failed_participants,
            profile_picture,
            group_subject,
            group_description,
            errors,
        } = self;
        f.debug_struct("GroupUpdate")
            .field("timestamp", timestamp)
            .field("group_id", group_id)
            .field("update_type", update_type)
            .field("request_id", request_id)
            .field("subject", subject)
            .field("description", description)
            .field("invite_link", &invite_link.as_deref().map(RedactedPath))
            .field("join_approval_mode", join_approval_mode)
            .field("reason", reason)
            .field("initiated_by", initiated_by)
            .field("join_request_id", join_request_id)
            .field("wa_id", wa_id)
            .field("user_id", user_id)
            .field("parent_user_id", parent_user_id)
            .field("username", username)
            .field("added_participants", added_participants)
            .field("removed_participants", removed_participants)
            .field("failed_participants", failed_participants)
            .field("profile_picture", profile_picture)
            .field("group_subject", group_subject)
            .field("group_description", group_description)
            .field("errors", errors)
            .finish()
    }
}

/// A participant in `added_participants` / `removed_participants` /
/// `failed_participants`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GroupParticipant {
    /// The identifier you used in the request (phone number or BSUID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Phone number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
    /// BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<UserId>,
    /// Parent BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_user_id: Option<UserId>,
    /// Username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Why this participant failed.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::graph_errors::deserialize",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub errors: Vec<GraphApiError>,
}

/// One setting in a `group_settings_update`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GroupSettingChange {
    /// New text (subject, description).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// MIME type (profile picture).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Hash (profile picture).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Whether this setting changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_successful: Option<bool>,
    /// Why it did not.
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::graph_errors::deserialize",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub errors: Vec<GraphApiError>,
}
