//! The `flows` field: Flow status changes and endpoint health alerts.
//!
//! Doc path: `flows/guides/flowswebhooks`. Flow *responses* are not here:
//! they arrive as `messages` with an `interactive.nfm_reply`
//! ([`super::messages::NfmReply`]).
//!
//! The page types `error_rate` as an integer but its examples send `14.28`;
//! rates, latencies and thresholds are `f64`. `availability` appears in the
//! endpoint availability example but not in the value table; it is modelled.

use serde::{Deserialize, Serialize};
use meta_whatsapp_core::ids::FlowId;

use crate::open_enum::open_enum;

open_enum! {
    /// `flows.event`.
    pub enum FlowEvent {
        /// Status changed (published, throttled, blocked, deprecated).
        FlowStatusChange => "FLOW_STATUS_CHANGE",
        /// Client-side screen navigation error rate crossed a threshold.
        ClientErrorRate => "CLIENT_ERROR_RATE",
        /// Endpoint error rate crossed a threshold.
        EndpointErrorRate => "ENDPOINT_ERROR_RATE",
        /// Endpoint p90 latency crossed a threshold.
        EndpointLatency => "ENDPOINT_LATENCY",
        /// Endpoint availability crossed the 90% threshold.
        EndpointAvailability => "ENDPOINT_AVAILABILITY",
    }
}

open_enum! {
    /// Flow status.
    pub enum FlowStatus {
        /// Draft.
        Draft => "DRAFT",
        /// Published.
        Published => "PUBLISHED",
        /// Deprecated.
        Deprecated => "DEPRECATED",
        /// Blocked.
        Blocked => "BLOCKED",
        /// Throttled.
        Throttled => "THROTTLED",
    }
}

open_enum! {
    /// `flows.alert_state`.
    pub enum FlowAlertState {
        /// Threshold reached.
        Activated => "ACTIVATED",
        /// Recovered.
        Deactivated => "DEACTIVATED",
    }
}

/// `value` of a `flows` change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowsValue {
    /// Kind of notification.
    pub event: FlowEvent,
    /// The Flow.
    pub flow_id: FlowId,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Previous status (`FLOW_STATUS_CHANGE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_status: Option<FlowStatus>,
    /// New status (`FLOW_STATUS_CHANGE`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_status: Option<FlowStatus>,
    /// Alert state (alerts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alert_state: Option<FlowAlertState>,
    /// Threshold reached or recovered from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// Overall error rate, percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_rate: Option<f64>,
    /// Requests the metric was computed over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_count: Option<i64>,
    /// p50 endpoint latency, ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p50_latency: Option<f64>,
    /// p90 endpoint latency, ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p90_latency: Option<f64>,
    /// Endpoint availability, percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<f64>,
    /// Errors behind an error-rate alert.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<FlowErrorStat>,
}

/// `flows.errors[]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FlowErrorStat {
    /// Error type, e.g. `INVALID_SCREEN_TRANSITION`, `TIMEOUT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// Share of this error, percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_rate: Option<f64>,
    /// Occurrences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_count: Option<i64>,
}
