//! The `WA_EMBEDDED_SIGNUP` message event ("session info logging") that the
//! Embedded Signup popup posts to the window that opened it, and that your
//! page forwards to your server.
//!
//! Shapes from `embedded-signup/implementation` (session logging),
//! `embedded-signup/errors` (abandoned screens), `bypass-phone-addition`,
//! `app-only-install` and `onboarding-business-app-users`:
//!
//! ```json
//! {"type": "WA_EMBEDDED_SIGNUP", "event": "FINISH",
//!  "data": {"phone_number_id": "…", "waba_id": "…", "business_id": "…"}}
//! {"type": "WA_EMBEDDED_SIGNUP", "event": "CANCEL", "data": {"current_step": "PHONE_NUMBER_SETUP"}}
//! {"type": "WA_EMBEDDED_SIGNUP", "event": "CANCEL",
//!  "data": {"error_message": "…", "error_code": "524126", "session_id": "…", "timestamp": "1746041036"}}
//! ```
//!
//! **This payload comes from the browser and is not authenticated.** Treat
//! its ids as claims: [`EmbeddedSignup::onboard`](super::EmbeddedSignup::onboard)
//! checks the WABA against the token's grants and the phone number against
//! the WABA before storing anything. Limit the size of the request body
//! that carries it in your HTTP layer.

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use wa_core::error::ValidationError;
use wa_core::ids::{BusinessId, PhoneNumberId, WabaId};
use wa_core::{Error, Result};

/// The `type` every Embedded Signup message event carries.
pub const MESSAGE_TYPE: &str = "WA_EMBEDDED_SIGNUP";

/// A parsed `WA_EMBEDDED_SIGNUP` message event.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum EmbeddedSignupEvent {
    /// The flow completed (`FINISH` and its variants). Closing the popup on
    /// the last screen also counts as completion.
    Finish {
        /// Which completion.
        kind: FinishKind,
        /// Asset ids.
        session: SessionInfo,
    },
    /// The customer left the flow (`CANCEL`), either abandoning a screen or
    /// reporting an error from it.
    Cancel(CancelInfo),
    /// `ERROR`: the customer hit an error. Meta lists the value but does not
    /// document its `data`; the error fields are parsed when present and the
    /// raw object is kept.
    Error {
        /// Error fields, when present.
        details: ReportedError,
        /// The raw `data` object.
        data: serde_json::Value,
    },
    /// An `event` value this crate does not know yet.
    Unknown {
        /// The `event` string.
        event: String,
        /// The raw `data` object.
        data: serde_json::Value,
    },
}

/// Which kind of completion a `FINISH*` event reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FinishKind {
    /// `FINISH`: the Cloud API flow.
    Finish,
    /// `FINISH_ONLY_WABA`: completed without a phone number (bypass flow).
    OnlyWaba,
    /// `FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING`: coexistence. The number is
    /// already registered: do **not** call `register`, and start the
    /// `smb_app_data` syncs within 24 hours.
    WhatsappBusinessAppOnboarding,
    /// `FINISH_OBO_MIGRATION`: an on-behalf-of migration.
    OboMigration,
    /// `FINISH_GRANT_ONLY_API_ACCESS`: app-only install.
    GrantOnlyApiAccess,
}

impl FinishKind {
    /// The `event` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Finish => "FINISH",
            Self::OnlyWaba => "FINISH_ONLY_WABA",
            Self::WhatsappBusinessAppOnboarding => "FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING",
            Self::OboMigration => "FINISH_OBO_MIGRATION",
            Self::GrantOnlyApiAccess => "FINISH_GRANT_ONLY_API_ACCESS",
        }
    }

    fn from_event(event: &str) -> Option<Self> {
        Some(match event {
            "FINISH" => Self::Finish,
            "FINISH_ONLY_WABA" => Self::OnlyWaba,
            "FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING" => Self::WhatsappBusinessAppOnboarding,
            "FINISH_OBO_MIGRATION" => Self::OboMigration,
            "FINISH_GRANT_ONLY_API_ACCESS" => Self::GrantOnlyApiAccess,
            _ => return None,
        })
    }
}

/// The asset ids of a completed flow (`data` of a `FINISH*` event).
///
/// Every field is optional: which ids come back depends on the flow
/// (coexistence sends only `waba_id`; the bypass flow sends no business id).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SessionInfo {
    /// The customer's WABA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba_id: Option<WabaId>,
    /// The customer's business phone number id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number_id: Option<PhoneNumberId>,
    /// The customer's business portfolio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_id: Option<BusinessId>,
    /// All WABAs, in multi-WABA flows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waba_ids: Vec<WabaId>,
    /// Ad accounts, if the customer selected any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ad_account_ids: Vec<String>,
    /// Facebook Pages, if selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub page_ids: Vec<String>,
    /// Datasets, if selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dataset_ids: Vec<String>,
    /// Catalogs, if selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catalog_ids: Vec<String>,
    /// Instagram accounts, if selected.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instagram_account_ids: Vec<String>,
}

impl SessionInfo {
    /// The WABA to onboard: `waba_id`, else the first of `waba_ids`.
    pub fn primary_waba_id(&self) -> Option<&WabaId> {
        self.waba_id.as_ref().or_else(|| self.waba_ids.first())
    }
}

/// `data` of a `CANCEL` event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CancelInfo {
    /// The screen the customer abandoned, when they abandoned one.
    pub current_step: Option<CurrentStep>,
    /// The error the customer reported, when they reported one.
    pub error: Option<ReportedError>,
}

/// The screen a customer abandoned (`embedded-signup/errors`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CurrentStep {
    /// Business portfolio screen.
    BusinessAccountSelection,
    /// WABA selection screen.
    WabaPhoneProfilePicker,
    /// WABA creation screen.
    WhatsappBusinessProfileSetup,
    /// Phone number addition screen.
    PhoneNumberSetup,
    /// Phone number verification screen.
    PhoneNumberVerification,
    /// Permissions review screen.
    Permissions,
    /// A screen this crate does not know (v4 added several).
    Other(String),
}

impl CurrentStep {
    fn from_str_lossless(s: &str) -> Self {
        match s {
            "BUSINESS_ACCOUNT_SELECTION" => Self::BusinessAccountSelection,
            "WABA_PHONE_PROFILE_PICKER" => Self::WabaPhoneProfilePicker,
            "WHATSAPP_BUSINESS_PROFILE_SETUP" => Self::WhatsappBusinessProfileSetup,
            "PHONE_NUMBER_SETUP" => Self::PhoneNumberSetup,
            "PHONE_NUMBER_VERIFICATION" => Self::PhoneNumberVerification,
            "PERMISSIONS" => Self::Permissions,
            other => Self::Other(other.to_owned()),
        }
    }

    /// The string Meta sends.
    pub fn as_str(&self) -> &str {
        match self {
            Self::BusinessAccountSelection => "BUSINESS_ACCOUNT_SELECTION",
            Self::WabaPhoneProfilePicker => "WABA_PHONE_PROFILE_PICKER",
            Self::WhatsappBusinessProfileSetup => "WHATSAPP_BUSINESS_PROFILE_SETUP",
            Self::PhoneNumberSetup => "PHONE_NUMBER_SETUP",
            Self::PhoneNumberVerification => "PHONE_NUMBER_VERIFICATION",
            Self::Permissions => "PERMISSIONS",
            Self::Other(s) => s,
        }
    }
}

/// An error the customer reported from inside the flow. Quote
/// `error_code`, `session_id` and `timestamp` to Meta support.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct ReportedError {
    /// The text shown to the customer.
    #[serde(default)]
    pub error_message: Option<String>,
    /// Error code (a string in Meta's syntax, a number in its example).
    #[serde(default, deserialize_with = "crate::waba::lenient_string")]
    pub error_code: Option<String>,
    /// Embedded Signup session id.
    #[serde(default, deserialize_with = "crate::waba::lenient_string")]
    pub session_id: Option<String>,
    /// When the error was reported (unix seconds, string or number).
    #[serde(default, deserialize_with = "lenient_unix")]
    pub timestamp: Option<OffsetDateTime>,
}

impl ReportedError {
    fn is_empty(&self) -> bool {
        self.error_message.is_none()
            && self.error_code.is_none()
            && self.session_id.is_none()
            && self.timestamp.is_none()
    }
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename = "type")]
    kind: String,
    event: String,
    #[serde(default)]
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct RawCancel {
    #[serde(default)]
    current_step: Option<String>,
}

impl EmbeddedSignupEvent {
    /// Parse the message event forwarded by your page (the `event.data`
    /// string, or the object your page built from it).
    pub fn from_json(json: &str) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| Error::decode("Embedded Signup message event", e, json.as_bytes()))?;
        Self::from_value(value)
    }

    /// Parse an already-decoded message event.
    pub fn from_value(value: serde_json::Value) -> Result<Self> {
        let raw: RawEvent = serde_json::from_value(value).map_err(|e| {
            Error::from(ValidationError::new(
                "event",
                format!("not an Embedded Signup message event: {e}"),
            ))
        })?;
        if raw.kind != MESSAGE_TYPE {
            return Err(ValidationError::new("type", format!("expected {MESSAGE_TYPE}")).into());
        }
        let data = if raw.data.is_null() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            raw.data
        };
        let decode_err = |what: &'static str| {
            move |e: serde_json::Error| {
                Error::from(ValidationError::new(
                    format!("data ({what})"),
                    e.to_string(),
                ))
            }
        };
        if let Some(kind) = FinishKind::from_event(&raw.event) {
            let session = serde_json::from_value(data).map_err(decode_err("FINISH"))?;
            return Ok(Self::Finish { kind, session });
        }
        match raw.event.as_str() {
            "CANCEL" => {
                let step: RawCancel =
                    serde_json::from_value(data.clone()).map_err(decode_err("CANCEL"))?;
                let error: ReportedError =
                    serde_json::from_value(data).map_err(decode_err("CANCEL"))?;
                Ok(Self::Cancel(CancelInfo {
                    current_step: step
                        .current_step
                        .as_deref()
                        .map(CurrentStep::from_str_lossless),
                    error: (!error.is_empty()).then_some(error),
                }))
            }
            "ERROR" => {
                let details = serde_json::from_value(data.clone()).unwrap_or_default();
                Ok(Self::Error { details, data })
            }
            _ => Ok(Self::Unknown {
                event: raw.event,
                data,
            }),
        }
    }

    /// The asset ids, for a completed flow.
    pub fn session_info(&self) -> Option<&SessionInfo> {
        match self {
            Self::Finish { session, .. } => Some(session),
            _ => None,
        }
    }

    /// The completion kind, for a completed flow.
    pub fn finish_kind(&self) -> Option<FinishKind> {
        match self {
            Self::Finish { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for EmbeddedSignupEvent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Self::from_value(value).map_err(serde::de::Error::custom)
    }
}

/// Unix seconds as a string or number; anything else (or out of range)
/// becomes `None` rather than failing the whole event: the timestamp is only
/// informational.
fn lenient_unix<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<OffsetDateTime>, D::Error> {
    let secs = match Option::<serde_json::Value>::deserialize(d)? {
        Some(serde_json::Value::Number(n)) => n.as_i64(),
        Some(serde_json::Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    };
    Ok(secs.and_then(|s| OffsetDateTime::from_unix_timestamp(s).ok()))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn finish_with_all_documented_ids() {
        // embedded-signup/implementation, successful flow completion, with
        // the placeholders replaced by the page's example values.
        let ev = EmbeddedSignupEvent::from_value(json!({
          "data": {
            "phone_number_id": "106540352242922",
            "waba_id": "524126980791429",
            "business_id": "2729063490586005",
            "ad_account_ids": ["4052175343162067"],
            "page_ids": ["1791141545170328"],
            "dataset_ids": ["524126980791429"],
            "catalog_ids": ["8827498273649182"],
            "instagram_account_ids": ["1749204838281942"],
            "waba_ids": ["524126980791429"]
          },
          "type": "WA_EMBEDDED_SIGNUP",
          "event": "FINISH"
        }))
        .unwrap();
        assert_eq!(ev.finish_kind(), Some(FinishKind::Finish));
        let s = ev.session_info().unwrap();
        assert_eq!(s.waba_id.as_ref().unwrap().as_str(), "524126980791429");
        assert_eq!(
            s.phone_number_id.as_ref().unwrap().as_str(),
            "106540352242922"
        );
        assert_eq!(s.business_id.as_ref().unwrap().as_str(), "2729063490586005");
        assert_eq!(s.catalog_ids, vec!["8827498273649182"]);
        assert_eq!(s.primary_waba_id(), s.waba_id.as_ref());
    }

    #[test]
    fn finish_variants() {
        // bypass-phone-addition (note the top-level "version": 3).
        let ev = EmbeddedSignupEvent::from_json(
            r#"{"data": {"phone_number_id": "1", "waba_id": "2"}, "type": "WA_EMBEDDED_SIGNUP", "event": "FINISH_ONLY_WABA", "version": 3}"#,
        )
        .unwrap();
        assert_eq!(ev.finish_kind(), Some(FinishKind::OnlyWaba));
        // onboarding-business-app-users: only waba_id.
        let ev = EmbeddedSignupEvent::from_json(
            r#"{"data": {"waba_id": "W"}, "type": "WA_EMBEDDED_SIGNUP", "event": "FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING", "version": 3}"#,
        )
        .unwrap();
        assert_eq!(
            ev.finish_kind(),
            Some(FinishKind::WhatsappBusinessAppOnboarding)
        );
        assert_eq!(ev.session_info().unwrap().phone_number_id, None);
        for (event, kind) in [
            ("FINISH_OBO_MIGRATION", FinishKind::OboMigration),
            (
                "FINISH_GRANT_ONLY_API_ACCESS",
                FinishKind::GrantOnlyApiAccess,
            ),
        ] {
            let ev = EmbeddedSignupEvent::from_value(
                json!({"data": {"waba_id": "W", "business_id": "B"}, "type": "WA_EMBEDDED_SIGNUP", "event": event}),
            )
            .unwrap();
            assert_eq!(ev.finish_kind(), Some(kind));
            assert_eq!(kind.as_str(), event);
        }
        // Multi-WABA with only `waba_ids`.
        let ev = EmbeddedSignupEvent::from_value(
            json!({"data": {"waba_ids": ["A", "B"]}, "type": "WA_EMBEDDED_SIGNUP", "event": "FINISH"}),
        )
        .unwrap();
        assert_eq!(
            ev.session_info()
                .unwrap()
                .primary_waba_id()
                .map(WabaId::as_str),
            Some("A")
        );
    }

    #[test]
    fn cancel_abandoned_and_reported_error() {
        // embedded-signup/errors, "Abandoned flow screens".
        let ev = EmbeddedSignupEvent::from_value(json!({
          "data": {"current_step": "PHONE_NUMBER_SETUP"},
          "type": "WA_EMBEDDED_SIGNUP", "event": "CANCEL", "version": 3
        }))
        .unwrap();
        assert_eq!(
            ev,
            EmbeddedSignupEvent::Cancel(CancelInfo {
                current_step: Some(CurrentStep::PhoneNumberSetup),
                error: None
            })
        );
        // implementation, "User reported errors".
        let ev = EmbeddedSignupEvent::from_value(json!({
          "data": {
            "error_message": "Your verified name violates WhatsApp guidelines. Please edit your verified name and try again.",
            "error_code": "524126",
            "session_id": "f34b51dab5e0498",
            "timestamp": "1746041036"
          },
          "type": "WA_EMBEDDED_SIGNUP", "event": "CANCEL"
        }))
        .unwrap();
        let EmbeddedSignupEvent::Cancel(info) = ev else {
            panic!("not a cancel")
        };
        assert_eq!(info.current_step, None);
        let err = info.error.unwrap();
        assert_eq!(err.error_code.as_deref(), Some("524126"));
        assert_eq!(err.session_id.as_deref(), Some("f34b51dab5e0498"));
        assert_eq!(err.timestamp.unwrap().unix_timestamp(), 1746041036);
        // Numeric code and timestamp, as in the table's example values.
        let ev = EmbeddedSignupEvent::from_value(json!({
          "data": {"error_code": 524126, "timestamp": 1746041036},
          "type": "WA_EMBEDDED_SIGNUP", "event": "CANCEL"
        }))
        .unwrap();
        let EmbeddedSignupEvent::Cancel(info) = ev else {
            panic!("not a cancel")
        };
        assert_eq!(info.error.unwrap().error_code.as_deref(), Some("524126"));
    }

    #[test]
    fn unknown_steps_events_and_error_are_kept() {
        let ev = EmbeddedSignupEvent::from_value(json!({
          "data": {"current_step": "QR_CODE_VERIFICATION"},
          "type": "WA_EMBEDDED_SIGNUP", "event": "CANCEL"
        }))
        .unwrap();
        let EmbeddedSignupEvent::Cancel(info) = ev else {
            panic!("not a cancel")
        };
        assert_eq!(
            info.current_step.map(|s| s.as_str().to_owned()).as_deref(),
            Some("QR_CODE_VERIFICATION")
        );

        let ev = EmbeddedSignupEvent::from_value(
            json!({"data": {"x": 1}, "type": "WA_EMBEDDED_SIGNUP", "event": "FINISH_SOMETHING_NEW"}),
        )
        .unwrap();
        assert!(
            matches!(ev, EmbeddedSignupEvent::Unknown { ref event, .. } if event == "FINISH_SOMETHING_NEW")
        );

        let ev = EmbeddedSignupEvent::from_value(
            json!({"data": {"error_message": "boom", "extra": true}, "type": "WA_EMBEDDED_SIGNUP", "event": "ERROR"}),
        )
        .unwrap();
        let EmbeddedSignupEvent::Error { details, data } = ev else {
            panic!("not an error")
        };
        assert_eq!(details.error_message.as_deref(), Some("boom"));
        assert_eq!(data["extra"], json!(true));

        // Missing data is tolerated.
        let ev = EmbeddedSignupEvent::from_value(
            json!({"type": "WA_EMBEDDED_SIGNUP", "event": "FINISH"}),
        )
        .unwrap();
        assert_eq!(ev.session_info(), Some(&SessionInfo::default()));
    }

    #[test]
    fn rejects_foreign_or_malformed_messages() {
        assert!(matches!(
            EmbeddedSignupEvent::from_value(json!({"type": "OTHER", "event": "FINISH"})),
            Err(Error::Validation(_))
        ));
        assert!(EmbeddedSignupEvent::from_value(json!({"event": "FINISH"})).is_err());
        assert!(matches!(
            EmbeddedSignupEvent::from_json("{not json"),
            Err(Error::Decode { .. })
        ));
        assert!(
            EmbeddedSignupEvent::from_value(
                json!({"type": "WA_EMBEDDED_SIGNUP", "event": "FINISH", "data": {"waba_id": 5}})
            )
            .is_err(),
            "a numeric id is not an id"
        );
        let via_serde: EmbeddedSignupEvent = serde_json::from_value(
            json!({"type": "WA_EMBEDDED_SIGNUP", "event": "CANCEL", "data": {}}),
        )
        .unwrap();
        assert!(matches!(via_serde, EmbeddedSignupEvent::Cancel(_)));
    }
}
