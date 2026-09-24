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

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use wa_core::GraphApiError;
use wa_core::ids::{GroupId, UserId, WaId};

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupUpdate {
    /// When the event happened.
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
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
