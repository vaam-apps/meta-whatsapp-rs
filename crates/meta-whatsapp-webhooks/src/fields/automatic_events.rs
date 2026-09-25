//! The `automatic_events` field: purchase and lead events Meta detected in
//! chats that started from a Click to WhatsApp ad.
//!
//! Doc paths: `webhooks/overview` (field table),
//! `embedded-signup/automatic-events-api`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use meta_whatsapp_core::ids::MessageId;

use super::common::Metadata;
use crate::open_enum::open_enum;

/// `value` of an `automatic_events` change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomaticEventsValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The business phone number.
    pub metadata: Metadata,
    /// Detected events.
    #[serde(default)]
    pub automatic_events: Vec<AutomaticEvent>,
}

open_enum! {
    /// `automatic_events[].event_name`.
    pub enum AutomaticEventName {
        /// A lead.
        LeadSubmitted => "LeadSubmitted",
        /// A purchase.
        Purchase => "Purchase",
    }
}

/// `automatic_events[]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomaticEvent {
    /// Id of the message the event was detected in.
    pub id: MessageId,
    /// What was detected.
    pub event_name: AutomaticEventName,
    /// When.
    #[serde(with = "meta_whatsapp_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
    /// Click id of the ad, for the Conversions API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctwa_clid: Option<String>,
    /// Purchase value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_data: Option<PurchaseData>,
}

/// `automatic_events[].custom_data`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PurchaseData {
    /// Currency code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Amount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}
