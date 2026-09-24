//! The `calls` field (Calling API signalling).
//!
//! Doc paths: `calling/reference` (§ Calling API Webhooks),
//! `calling/business-initiated-calls`, `calling/user-initiated-calls`,
//! `calling/call-recording`, `calling/call-transcription`, and the calling
//! section of `business-scoped-user-ids`. WebRTC media is out of scope; the
//! SDP is passed through as text.
//!
//! # Where the pages disagree
//!
//! - The terminate examples print `status` as `[FAILED | COMPLETED]` in
//!   `calling/reference` and `["Failed", "Completed"]` in the business- and
//!   user-initiated pages (both are "one of" notation). Both casings map to
//!   the same [`CallTerminateStatus`] variant.
//! - `calling/reference`'s Call Connect example has `to` equal to the
//!   business number and `from` equal to the user although `direction` is
//!   `BUSINESS_INITIATED`; `calling/business-initiated-calls` has them the
//!   other way round. Contact matching therefore tries every identifier.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use wa_core::GraphApiError;
use wa_core::ids::{CallId, UserId};

use super::common::{Contact, Metadata};
use super::messages::MediaContent;
use crate::open_enum::open_enum;

/// `value` of a `calls` change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallsValue {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// The business phone number.
    pub metadata: Metadata,
    /// The WhatsApp users on the calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<Contact>,
    /// Call events (connect, terminate, recording/transcript available…).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<Call>,
    /// Call status updates (ringing, accepted, rejected).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<CallStatus>,
    /// Errors (terminate webhooks).
    #[serde(
        default,
        deserialize_with = "crate::serde_ext::graph_errors::deserialize",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub errors: Vec<GraphApiError>,
}

open_enum! {
    /// `calls[].event`.
    pub enum CallEventType {
        /// Ready to connect (carries an SDP offer or answer).
        Connect => "connect",
        /// Call ended.
        Terminate => "terminate",
        /// A SIP call was attempted.
        CallCreated => "call_created",
        /// A recording is ready to download.
        CallRecordingAvailable => "call_recording_available",
        /// A transcript is ready to download.
        CallTranscriptionAvailable => "call_transcription_available",
    }
}

open_enum! {
    /// `calls[].direction`.
    pub enum CallDirection {
        /// The business called the user.
        BusinessInitiated => "BUSINESS_INITIATED",
        /// The user called the business.
        UserInitiated => "USER_INITIATED",
    }
}

open_enum! {
    /// `calls[].status` on terminate events.
    pub enum CallTerminateStatus {
        /// Completed.
        Completed => "COMPLETED" | "Completed",
        /// Failed.
        Failed => "FAILED" | "Failed",
    }
}

/// `calls[]`: one call event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    /// Call id (`wacid.…`).
    pub id: CallId,
    /// What happened.
    pub event: CallEventType,
    /// When.
    #[serde(with = "wa_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
    /// Who called whom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<CallDirection>,
    /// Callee number; omitted for a user whose number Meta may not share.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Callee BSUID (business-initiated calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_user_id: Option<UserId>,
    /// Callee parent BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_parent_user_id: Option<UserId>,
    /// Caller number; omitted for a user whose number Meta may not share.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Caller BSUID (user-initiated calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_user_id: Option<UserId>,
    /// Caller parent BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_parent_user_id: Option<UserId>,
    /// SDP offer/answer (`connect`; absent for SIP calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<CallSession>,
    /// Legacy `connection` object seen in the sample connect webhook; kept
    /// untyped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<serde_json::Value>,
    /// Your tracking string from the initiate / accept request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biz_opaque_callback_data: Option<String>,
    /// `biz_payload` of the call deep link the user used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deeplink_payload: Option<String>,
    /// `payload` of the call button the user tapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cta_payload: Option<String>,
    /// Outcome (`terminate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<CallTerminateStatus>,
    /// When the call was picked up (`terminate`, answered calls).
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub start_time: Option<OffsetDateTime>,
    /// When the call ended (`terminate`, answered calls).
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub end_time: Option<OffsetDateTime>,
    /// Duration in seconds (`terminate`, answered calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    /// Recording (`call_recording_available`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_recording: Option<CallRecording>,
    /// Transcript (`call_transcription_available`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_transcript: Option<CallTranscript>,
}

/// `calls[].session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSession {
    /// `offer` or `answer`.
    pub sdp_type: String,
    /// RFC 8866 SDP.
    pub sdp: String,
}

/// `calls[].call_recording`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRecording {
    /// Media type; currently always `audio`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub recording_type: Option<String>,
    /// The audio asset; the URL is valid for 5 minutes, the id for 7 days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<MediaContent>,
}

/// `calls[].call_transcript`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallTranscript {
    /// The transcript document (`application/json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<MediaContent>,
}

open_enum! {
    /// `statuses[].status` of a call.
    pub enum CallStatusValue {
        /// The user's device is ringing.
        Ringing => "RINGING",
        /// The user accepted.
        Accepted => "ACCEPTED",
        /// The user rejected.
        Rejected => "REJECTED",
    }
}

/// `statuses[]` of a `calls` change (business-initiated calls).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallStatus {
    /// Call id.
    pub id: CallId,
    /// Always `call`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub status_type: Option<String>,
    /// The status.
    pub status: CallStatusValue,
    /// When.
    #[serde(with = "wa_core::timestamp::unix")]
    pub timestamp: OffsetDateTime,
    /// Callee number; omitted when Meta may not share it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,
    /// Callee BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_user_id: Option<UserId>,
    /// Callee parent BSUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipient_parent_user_id: Option<UserId>,
    /// Your tracking string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biz_opaque_callback_data: Option<String>,
}
