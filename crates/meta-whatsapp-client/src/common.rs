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
//!
//! Doc paths: `messages/send-messages` ("Media caching"),
//! `templates/template-media`, `flows/guides/sendingaflow`,
//! `flows/guides/flows-templates`, `templates/template-quality`,
//! `reference/whatsapp-business-account/phone-number-management-api`
//! (`WhatsAppPhoneNumberQualityRating`), `business-phone-numbers/phone-numbers`.

use serde::{Deserialize, Serialize};
use meta_whatsapp_core::ids::MediaId;

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
