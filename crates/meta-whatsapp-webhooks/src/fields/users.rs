//! User-level fields: marketing preferences and BSUID changes.
//!
//! Doc paths: `webhooks/reference/user_preferences`,
//! `templates/marketing-templates` (what `stop`/`resume` mean), and the
//! `user_preferences` / `user_id_update` sections of
//! `business-scoped-user-ids`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use meta_whatsapp_core::ids::{UserId, WaId};

use super::common::{Contact, Metadata};
use crate::open_enum::open_enum;

/// `value` of a `user_preferences` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPreferencesValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The business phone number.
    pub metadata: Metadata,
    /// The users whose preferences changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<Contact>,
    /// The changes.
    #[serde(default)]
    pub user_preferences: Vec<UserPreference>,
}

open_enum! {
    /// `user_preferences[].category`.
    pub enum PreferenceCategory {
        /// Marketing messages.
        MarketingMessages => "marketing_messages",
    }
}

open_enum! {
    /// `user_preferences[].value`.
    pub enum PreferenceValue {
        /// The user stopped marketing messages from you. Record the opt-out:
        /// sending marketing templates to them now fails with `131050`.
        Stop => "stop",
        /// The user resumed marketing messages.
        Resume => "resume",
    }
}

/// `user_preferences[]`: one preference change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPreference {
    /// Phone number; omitted when Meta may not share it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
    /// BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<UserId>,
    /// Parent BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_user_id: Option<UserId>,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Which preference.
    pub category: PreferenceCategory,
    /// New value.
    pub value: PreferenceValue,
    /// When the webhook was sent.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
}

/// `value` of a `user_id_update` change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserIdUpdateValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The business phone number.
    pub metadata: Metadata,
    /// The users whose BSUID changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<Contact>,
    /// The changes.
    #[serde(default)]
    pub user_id_update: Vec<UserIdUpdate>,
}

/// `user_id_update[]`: a user's BSUID changed. Re-key everything you stored
/// under [`IdChange::previous`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserIdUpdate {
    /// Phone number; omitted when Meta may not share it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Old and new BSUID.
    pub user_id: IdChange,
    /// Old and new parent BSUID, when parent BSUIDs are enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_user_id: Option<IdChange>,
    /// When the webhook was sent.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
}

/// `{previous, current}` pair of a BSUID change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdChange {
    /// The old id.
    pub previous: UserId,
    /// The new id.
    pub current: UserId,
}
