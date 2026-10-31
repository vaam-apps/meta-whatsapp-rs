//! The per-call `recording` and `transcription` objects of `connect` and
//! `accept` (`calling/call-recording`, `calling/call-transcription`).
//!
//! Both are opt-in per call, independent of each other, and off unless you
//! set them: [`ConnectCall`](super::ConnectCall) and
//! [`AcceptOptions`](super::AcceptOptions) carry neither by default, and
//! no other call sets them. Before any audio is recorded or transcribed,
//! Meta plays an announcement to both participants, built from `purpose`
//! in `announcement_language`; a participant who does not consent ends the
//! call. When both are enabled on one call, a single combined announcement
//! uses the `recording` object's `purpose` and `announcement_language` and
//! ignores the `transcription` object's (each object is still checked on
//! its own: the pages require both fields whenever `status` is `ENABLED`).
//!
//! The results arrive after the call as `calls` webhooks
//! (`call_recording_available` with an audio media id,
//! `call_transcription_available` with a JSON document's media id), parsed
//! by `meta-whatsapp-webhooks`, and download through the Media API for 7
//! days.
//!
//! Limits enforced locally: with `status` `ENABLED`, `purpose` and
//! `announcement_language` are present and not blank; `purpose` is at most
//! [`PURPOSE_MAX_CHARS`] characters; a given `announcement_language` is not
//! blank. The list of supported announcement languages is Meta's to check
//! (it answers an unsupported one with a request error): the pages list
//! `en` (and `en_US`, `en_AU`, `en_CA`, `en_GB`, `en_IN`, `en_NZ`), `nl`,
//! `fr`, `de`, `hi`, `it`, `kn`, `pt`, `es`, `es_ES`, `te`, `vi`.

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use serde::{Deserialize, Serialize};

use super::settings::FeatureStatus;

/// Longest `purpose` of a [`CallRecording`] or [`CallTranscription`], in
/// characters.
pub const PURPOSE_MAX_CHARS: usize = 250;

/// The `recording` object of a `connect` or `accept`
/// (`calling/call-recording#recording-object-reference`): record the
/// call's audio. The recording arrives as a `call_recording_available`
/// webhook carrying its media id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallRecording {
    /// `ENABLED` to record the call, `DISABLED` to opt out explicitly
    /// (the same as leaving the object out).
    pub status: FeatureStatus,
    /// Why the call is recorded, spoken to both participants in the
    /// announcement, in the language of `announcement_language`. Required
    /// when `status` is `ENABLED`; at most [`PURPOSE_MAX_CHARS`]
    /// characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Locale code of the announcement, e.g. `en_US` or `es`. Required when
    /// `status` is `ENABLED`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announcement_language: Option<String>,
}

impl CallRecording {
    /// Record the call: `status` `ENABLED` with the announcement's
    /// `purpose` and `announcement_language`.
    pub fn enabled(purpose: impl Into<String>, announcement_language: impl Into<String>) -> Self {
        Self {
            status: FeatureStatus::Enabled,
            purpose: Some(purpose.into()),
            announcement_language: Some(announcement_language.into()),
        }
    }

    /// Opt out explicitly: `{"status": "DISABLED"}`.
    pub fn disabled() -> Self {
        Self {
            status: FeatureStatus::Disabled,
            purpose: None,
            announcement_language: None,
        }
    }

    /// Check the documented limits; errors name `recording.<field>`.
    pub(super) fn validate(&self) -> Result<()> {
        validate(
            "recording",
            &self.status,
            self.purpose.as_deref(),
            self.announcement_language.as_deref(),
        )
    }
}

/// The `transcription` object of a `connect` or `accept`
/// (`calling/call-transcription#transcription-object-reference`):
/// transcribe the call. The transcript, a JSON document whose spoken
/// language Meta detects, arrives as a `call_transcription_available`
/// webhook carrying its media id. It does not record the audio: that is
/// [`CallRecording`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallTranscription {
    /// `ENABLED` to transcribe the call, `DISABLED` to opt out explicitly
    /// (the same as leaving the object out).
    pub status: FeatureStatus,
    /// Why the call is transcribed, spoken to both participants in the
    /// announcement, in the language of `announcement_language`. Required
    /// when `status` is `ENABLED`; at most [`PURPOSE_MAX_CHARS`]
    /// characters. Ignored by Meta when the call is also recorded (the
    /// recording's is spoken).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Locale code of the announcement, e.g. `en_US` or `es` (`nl` and
    /// `es_ES` are accepted but play the English announcement for now).
    /// Required when `status` is `ENABLED`. Ignored by Meta when the call
    /// is also recorded. It is not the transcript's language.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announcement_language: Option<String>,
}

impl CallTranscription {
    /// Transcribe the call: `status` `ENABLED` with the announcement's
    /// `purpose` and `announcement_language`.
    pub fn enabled(purpose: impl Into<String>, announcement_language: impl Into<String>) -> Self {
        Self {
            status: FeatureStatus::Enabled,
            purpose: Some(purpose.into()),
            announcement_language: Some(announcement_language.into()),
        }
    }

    /// Opt out explicitly: `{"status": "DISABLED"}`.
    pub fn disabled() -> Self {
        Self {
            status: FeatureStatus::Disabled,
            purpose: None,
            announcement_language: None,
        }
    }

    /// Check the documented limits; errors name `transcription.<field>`.
    pub(super) fn validate(&self) -> Result<()> {
        validate(
            "transcription",
            &self.status,
            self.purpose.as_deref(),
            self.announcement_language.as_deref(),
        )
    }
}

/// The rules both pages state for their object. A `status` other than
/// `ENABLED` or `DISABLED` is passed through for Meta to judge.
fn validate(
    object: &str,
    status: &FeatureStatus,
    purpose: Option<&str>,
    announcement_language: Option<&str>,
) -> Result<()> {
    let enabled = *status == FeatureStatus::Enabled;
    let field = |name: &str| format!("{object}.{name}");
    match purpose {
        None if enabled => {
            return Err(
                ValidationError::new(field("purpose"), "required when status is ENABLED").into(),
            );
        }
        Some(p) if enabled && p.trim().is_empty() => {
            return Err(ValidationError::new(
                field("purpose"),
                "must not be empty when status is ENABLED",
            )
            .into());
        }
        Some(p) => {
            let len = p.chars().count();
            if len > PURPOSE_MAX_CHARS {
                return Err(ValidationError::new(
                    field("purpose"),
                    format!("must be at most {PURPOSE_MAX_CHARS} characters, got {len}"),
                )
                .into());
            }
        }
        None => {}
    }
    match announcement_language {
        None if enabled => Err(ValidationError::new(
            field("announcement_language"),
            "required when status is ENABLED",
        )
        .into()),
        Some(l) if l.trim().is_empty() => {
            Err(ValidationError::new(field("announcement_language"), "must not be empty").into())
        }
        _ => Ok(()),
    }
}
