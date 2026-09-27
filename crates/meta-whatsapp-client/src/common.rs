//! Types more than one endpoint family uses, defined once.
//!
//! Each is also re-exported by the modules that use it, so
//! `messages::MediaSource` and `templates::MediaSource` (and so on) are the
//! same type under two paths:
//!
//! | Type | Re-exported by |
//! | --- | --- |
//! | [`MediaSource`] | [`messages`](crate::messages), [`templates`](crate::templates) |
//! | [`FlowAction`] | [`messages`](crate::messages), [`templates`](crate::templates) |
//! | [`QualityRating`] | [`phone_numbers`](crate::phone_numbers), [`templates`](crate::templates) |
//! | [`HealthStatus`] (and [`HealthEntity`], [`HealthError`], [`HealthState`], [`HealthEntityType`]) | [`phone_numbers`](crate::phone_numbers), [`templates`](crate::templates), [`waba`](crate::waba) |
//!
//! Doc paths: `messages/send-messages` ("Media caching"),
//! `templates/template-media`, `flows/guides/sendingaflow`,
//! `flows/guides/flows-templates`, `templates/template-quality`,
//! `reference/whatsapp-business-account/phone-number-management-api`
//! (`WhatsAppPhoneNumberQualityRating`), `business-phone-numbers/phone-numbers`,
//! `support/health-status`.

use meta_whatsapp_core::ids::MediaId;
use serde::{Deserialize, Serialize};

use crate::messages::validate::{self, Check};
use crate::templates::macros::string_enum;

/// Where a media asset comes from: an uploaded media id or a public link.
/// Serializes to `{"id": …}` or `{"link": …}`, in messages and in template
/// parameters alike.
///
/// Meta recommends uploaded ids (`Media::upload`, which also spares your
/// server Meta's fetches under load); links are cached by Meta for 10
/// minutes per URL (`messages/send-messages`, "Media caching").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaSource {
    /// An uploaded media id.
    Id(MediaId),
    /// A public URL on your server.
    Link(String),
}

impl MediaSource {
    /// Uploaded media.
    pub fn id(id: impl Into<MediaId>) -> Self {
        Self::Id(id.into())
    }

    /// Hosted media.
    pub fn link(url: impl Into<String>) -> Self {
        Self::Link(url.into())
    }

    /// The id or link is present (`<field>.id` / `<field>.link` otherwise).
    pub(crate) fn validate(&self, field: &str) -> Check {
        match self {
            Self::Id(id) => validate::non_empty(&format!("{field}.id"), id.as_str()),
            Self::Link(url) => validate::non_empty(&format!("{field}.link"), url),
        }
    }
}

impl From<MediaId> for MediaSource {
    fn from(id: MediaId) -> Self {
        Self::Id(id)
    }
}

string_enum! {
    /// `flow_action` of a Flow message or a Flow template button: how the
    /// Flow starts.
    pub enum FlowAction {
        /// Open the first screen directly (`flow_action_payload.screen`, or
        /// the button's `navigate_screen`). Meta's default.
        Navigate => "navigate",
        /// Ask your Flow endpoint for the first screen.
        DataExchange => "data_exchange",
    }
}

string_enum! {
    /// Quality rating of a template (`quality_score.score`,
    /// `templates/template-quality`) or of a business phone number
    /// (`quality_rating`).
    ///
    /// # The webhook's type
    ///
    /// The `message_template_quality_update` webhook carries the same score
    /// as `meta_whatsapp_webhooks::fields::templates::TemplateQualityScore`. The two
    /// types stay separate (the owner's decision, 2026-09-25: meta-whatsapp-client and
    /// meta-whatsapp-webhooks do not depend on each other); they correspond by wire
    /// value ([`Self::as_str`], and `as_str` on the webhook's type):
    ///
    /// | Wire | `QualityRating` | `TemplateQualityScore` |
    /// | --- | --- | --- |
    /// | `GREEN`, `YELLOW`, `RED` | `Green`, `Yellow`, `Red` | `Green`, `Yellow`, `Red` |
    /// | `UNKNOWN` (not rated yet) | `Unknown` | `Unknown` |
    /// | `NA` | `NotApplicable` | `Other("NA")`: the webhook documents no `NA` |
    /// | anything else | `Other`, verbatim | `Other`, verbatim |
    ///
    /// **Case**: this type matches values case-insensitively (`"green"` is
    /// `Green`; an `Other` keeps the value as sent), the webhook's type
    /// matches them exactly (`"green"` is `Other("green")`). So convert
    /// through the wire value, into this type:
    /// `score.as_str().parse::<QualityRating>()` (infallible) maps each
    /// webhook value to the variant above, `Other("NA")` and lower-case
    /// spellings included. The other way, `TemplateQualityScore::from`
    /// of [`Self::as_str`], turns `NotApplicable` into `Other("NA")`.
    pub enum QualityRating {
        /// High quality.
        Green => "GREEN",
        /// Medium quality; a template may be paused or disabled soon.
        Yellow => "YELLOW",
        /// Low quality; a template is paused automatically
        /// (`templates/template-pausing`).
        Red => "RED",
        /// Not rated yet, in the phone number examples (`NA`).
        NotApplicable => "NA",
        /// Not rated yet: every new template starts here, and the phone
        /// number reference lists it too. A documented value, not the
        /// catch-all (`Other`).
        Unknown => "UNKNOWN",
    }
}

/// `health_status` of a business phone number, a WABA or a message
/// template (`support/health-status`): whether messaging (and receiving
/// calls over SIP) will work through that node, given every node a request
/// through it involves.
///
/// Read it with `PhoneNumber::health_status`
/// ([`crate::phone_numbers::PhoneNumber::health_status`]),
/// [`crate::waba::Waba::health_status`], or the `health_status` field of a
/// template ([`crate::templates::TemplateInfo`]). Meta documents the field
/// on those three nodes only, and says the WABA, business and app entities
/// are "always included" in each answer: so a business portfolio's status
/// is its [`HealthEntityType::Business`] entry in any of them
/// ([`Self::entity`]), and there is no business reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HealthStatus {
    /// The overall messaging status: `BLOCKED` if any node is blocked,
    /// else `LIMITED` if any is limited, else `AVAILABLE`. There is no
    /// overall value for `can_receive_call_sip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_send_message: Option<HealthState>,
    /// One entry per node involved: the phone number (when the target is a
    /// number), the template (when it is a template), and always the WABA,
    /// the business portfolio and the app.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<HealthEntity>,
}

impl HealthStatus {
    /// The first entity of this type, e.g. [`HealthEntityType::Business`]
    /// for the business portfolio's own status.
    pub fn entity(&self, entity_type: &HealthEntityType) -> Option<&HealthEntity> {
        self.entities.iter().find(|e| &e.entity_type == entity_type)
    }
}

/// One node of a [`HealthStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HealthEntity {
    /// Which kind of node this is.
    pub entity_type: HealthEntityType,
    /// The node's id: a phone number, template, WABA, business portfolio
    /// or app id, per [`Self::entity_type`]; kept as the raw string for
    /// that reason.
    pub id: String,
    /// This node's messaging status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_send_message: Option<HealthState>,
    /// This node's ability to receive a call over SIP (phone numbers and
    /// apps, in Meta's examples).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_receive_call_sip: Option<HealthState>,
    /// Why a node is `LIMITED` (present with that status).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_info: Vec<String>,
    /// Why a node is `BLOCKED`, with a possible solution (present with that
    /// status).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<HealthError>,
}

/// One entry of [`HealthEntity::errors`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HealthError {
    /// Meta's error code, e.g. `141002` (template not approved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<i64>,
    /// What is wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_description: Option<String>,
    /// What to do about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub possible_solution: Option<String>,
}

string_enum! {
    /// A health status value (`can_send_message`, `can_receive_call_sip`).
    pub enum HealthState {
        /// Meets every requirement.
        Available => "AVAILABLE",
        /// Meets the requirements with limitations
        /// ([`HealthEntity::additional_info`] says which).
        Limited => "LIMITED",
        /// Fails one or more requirements ([`HealthEntity::errors`] says
        /// which).
        Blocked => "BLOCKED",
    }
}

string_enum! {
    /// The kind of node a [`HealthEntity`] describes.
    pub enum HealthEntityType {
        /// A business phone number.
        PhoneNumber => "PHONE_NUMBER",
        /// A message template.
        MessageTemplate => "MESSAGE_TEMPLATE",
        /// A WhatsApp Business Account.
        Waba => "WABA",
        /// A business portfolio.
        Business => "BUSINESS",
        /// A Meta app.
        App => "APP",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn media_source_is_an_id_or_a_link_object() {
        assert_eq!(
            serde_json::to_value(MediaSource::id("1479537139650973")).unwrap(),
            json!({"id": "1479537139650973"})
        );
        assert_eq!(
            serde_json::to_value(MediaSource::link("https://shop.example/a.png")).unwrap(),
            json!({"link": "https://shop.example/a.png"})
        );
        let back: MediaSource = serde_json::from_value(json!({"id": "7"})).unwrap();
        assert_eq!(back, MediaSource::Id(MediaId::new("7")));
        assert!(MediaSource::id("").validate("image").is_err());
        assert_eq!(
            MediaSource::link("").validate("image").unwrap_err().field,
            "image.link"
        );
    }

    #[test]
    fn health_status_parses_the_blocked_examples() {
        // support/health-status, "Example blocked response" (a template).
        let template: HealthStatus = serde_json::from_value(json!({
            "can_send_message": "BLOCKED",
            "entities": [
              {"entity_type": "MESSAGE_TEMPLATE", "id": "2632273056924580", "can_send_message": "BLOCKED", "can_receive_call_sip": "AVAILABLE",
               "errors": [{"error_code": 141002, "error_description": "Message templates can only be sent out if they are approved.", "possible_solution": "Edit or appeal the message template review decision."}]},
              {"entity_type": "WABA", "id": "102290129340398", "can_send_message": "AVAILABLE"},
              {"entity_type": "BUSINESS", "id": "506914307656634", "can_send_message": "AVAILABLE"},
              {"entity_type": "APP", "id": "634974688087057", "can_send_message": "AVAILABLE", "can_receive_call_sip": "AVAILABLE"}
            ]
        }))
        .unwrap();
        assert_eq!(template.can_send_message, Some(HealthState::Blocked));
        let entity = template.entity(&HealthEntityType::MessageTemplate).unwrap();
        assert_eq!(entity.errors.len(), 1);
        assert_eq!(entity.errors[0].error_code, Some(141_002));
        assert_eq!(
            entity.errors[0].possible_solution.as_deref(),
            Some("Edit or appeal the message template review decision.")
        );

        // "Example blocked response for receiving calls over SIP".
        let sip: HealthStatus = serde_json::from_value(json!({
            "can_send_message": "BLOCKED",
            "entities": [
              {"entity_type": "PHONE_NUMBER", "id": "597727103418254", "can_send_message": "AVAILABLE", "can_receive_call_sip": "BLOCKED",
               "errors": [{"error_code": 138024, "error_description": "WhatsApp Business calling cannot use SIP because it is not enabled", "possible_solution": "Configure SIP using {PHONE_NUMBER_ID}/settings API"}]},
              {"entity_type": "WABA", "id": "102290129340398", "can_send_message": "AVAILABLE"},
              {"entity_type": "BUSINESS", "id": "506914307656634", "can_send_message": "AVAILABLE"},
              {"entity_type": "APP", "id": "634974688087057", "can_send_message": "AVAILABLE", "can_receive_call_sip": "BLOCKED",
               "errors": [{"error_code": 138025, "error_description": "This app cannot use SIP for WhatsApp Business calling because it has not configured a SIP server for this business phone number", "possible_solution": "Configure SIP server using {PHONE_NUMBER_ID}/settings API"}]}
            ]
        }))
        .unwrap();
        let number = sip.entity(&HealthEntityType::PhoneNumber).unwrap();
        assert_eq!(number.can_send_message, Some(HealthState::Available));
        assert_eq!(number.can_receive_call_sip, Some(HealthState::Blocked));
        assert_eq!(number.errors[0].error_code, Some(138_024));
        let app = sip.entity(&HealthEntityType::App).unwrap();
        assert_eq!(app.errors[0].error_code, Some(138_025));

        // Values Meta adds later are kept, not refused.
        let later: HealthStatus = serde_json::from_value(json!({
            "can_send_message": "DEGRADED",
            "entities": [{"entity_type": "SOLUTION", "id": "1", "brand_new": true}]
        }))
        .unwrap();
        assert_eq!(
            later.can_send_message,
            Some(HealthState::Other("DEGRADED".into()))
        );
        assert_eq!(
            later.entities[0].entity_type,
            HealthEntityType::Other("SOLUTION".into())
        );
        // The template reference's minimal shape parses too.
        let minimal: HealthStatus =
            serde_json::from_value(json!({"can_send_message": "AVAILABLE"})).unwrap();
        assert!(minimal.entities.is_empty());
    }

    #[test]
    fn open_enums_keep_new_values() {
        let action: FlowAction = serde_json::from_value(json!("DATA_EXCHANGE")).unwrap();
        assert_eq!(action, FlowAction::DataExchange);
        assert_eq!(
            serde_json::to_value(FlowAction::DataExchange).unwrap(),
            json!("data_exchange")
        );
        let future: FlowAction = serde_json::from_value(json!("resume")).unwrap();
        assert_eq!(future, FlowAction::Other("resume".into()));
        for (wire, rating) in [
            ("GREEN", QualityRating::Green),
            ("NA", QualityRating::NotApplicable),
            ("UNKNOWN", QualityRating::Unknown),
        ] {
            assert_eq!(
                serde_json::from_value::<QualityRating>(json!(wire)).unwrap(),
                rating
            );
            assert_eq!(serde_json::to_value(&rating).unwrap(), json!(wire));
        }
        assert_eq!(
            serde_json::from_value::<QualityRating>(json!("PURPLE")).unwrap(),
            QualityRating::Other("PURPLE".into())
        );
    }
}
