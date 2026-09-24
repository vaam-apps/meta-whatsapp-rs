//! The webhook envelope and per-field dispatch.
//!
//! ```text
//! { "object": "whatsapp_business_account",
//!   "entry": [ { "id": "<WABA id>", "time": 1739321024?,
//!                "changes": [ { "field": "<name>", "value": { … } } ] } ] }
//! ```
//!
//! Doc paths: `webhooks/overview`, `reference/webhooks/whatsapp-incoming-webhook-payload`.
//!
//! `value` is typed by its sibling `field`, so [`Change`] deserializes the
//! pair as `{field, value: serde_json::Value}` first and converts second.
//! That two-step is what lets a failing typed parse fall back to
//! [`ChangeValue::Unknown`] for that one change, with the reason kept on
//! [`Change::parse_error`], instead of failing the whole delivery: Meta
//! batches up to 1000 updates per POST, and one odd value must not hold the
//! other 999 hostage for 7 days of retries.

use serde::de::DeserializeOwned;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use time::OffsetDateTime;

use crate::fields::{
    AccountAlertsValue, AccountReviewUpdateValue, AccountSettingsUpdateValue, AccountUpdateValue,
    AutomaticEventsValue, BusinessCapabilityUpdateValue, BusinessUsernameUpdateValue, CallsValue,
    FlowsValue, GroupsValue, HistoryValue, MessagesValue, PartnerSolutionsValue,
    PaymentConfigurationUpdateValue, PhoneNumberNameUpdateValue, PhoneNumberQualityUpdateValue,
    SecurityValue, SmbAppStateSyncValue, SmbMessageEchoesValue, TemplateCategoryUpdateValue,
    TemplateComponentsUpdateValue, TemplateCorrectCategoryDetectionValue,
    TemplateQualityUpdateValue, TemplateStatusUpdateValue, UserIdUpdateValue, UserPreferencesValue,
};

/// A whole webhook POST body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookPayload {
    /// `whatsapp_business_account` for every WhatsApp webhook.
    pub object: String,
    /// One entry per account that changed.
    pub entry: Vec<Entry>,
}

impl WebhookPayload {
    /// Parse a raw body. Only the envelope can fail here; a field value that
    /// does not match its type becomes [`ChangeValue::Unknown`].
    pub fn from_slice(body: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(body)
    }
}

/// `entry[]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The WhatsApp Business Account id for every field except
    /// `partner_solutions`, where it is a business portfolio id. Accepted
    /// as a JSON number too: an envelope that fails to parse makes the
    /// whole batch (up to 1000 updates) `Unparsed`.
    #[serde(deserialize_with = "crate::serde_ext::id::deserialize")]
    pub id: String,
    /// When the change happened; sent by the management fields, not by
    /// `messages`.
    #[serde(
        default,
        with = "wa_core::timestamp::unix_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub time: Option<OffsetDateTime>,
    /// The changes.
    pub changes: Vec<Change>,
}

/// `entry[].changes[]`: one field's new value.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// The subscribed field, e.g. `messages`.
    pub field: String,
    /// The value, typed when `field` is known and the value matches.
    pub value: ChangeValue,
    /// Why a known field's value fell back to [`ChangeValue::Unknown`].
    /// `None` for typed values and for fields this crate does not know.
    pub parse_error: Option<String>,
}

impl Change {
    /// Type `value` according to `field`.
    pub fn parse(field: impl Into<String>, value: Value) -> Self {
        let field = field.into();
        let (value, parse_error) = ChangeValue::parse(&field, value);
        Self {
            field,
            value,
            parse_error,
        }
    }
}

impl<'de> Deserialize<'de> for Change {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            field: String,
            #[serde(default)]
            value: Value,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self::parse(raw.field, raw.value))
    }
}

impl Serialize for Change {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let len = if self.parse_error.is_some() { 3 } else { 2 };
        let mut s = serializer.serialize_struct("Change", len)?;
        s.serialize_field("field", &self.field)?;
        s.serialize_field("value", &self.value)?;
        if let Some(error) = &self.parse_error {
            s.serialize_field("parse_error", error)?;
        }
        s.end()
    }
}

/// A change's `value`, typed by its `field`.
///
/// Boxed: the values range from one enum to dozens of optional properties,
/// and a payload holds many of them.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ChangeValue {
    /// `messages`.
    Messages(Box<MessagesValue>),
    /// `account_alerts`.
    AccountAlerts(Box<AccountAlertsValue>),
    /// `account_review_update`.
    AccountReviewUpdate(Box<AccountReviewUpdateValue>),
    /// `account_settings_update`.
    AccountSettingsUpdate(Box<AccountSettingsUpdateValue>),
    /// `account_update`.
    AccountUpdate(Box<AccountUpdateValue>),
    /// `automatic_events`.
    AutomaticEvents(Box<AutomaticEventsValue>),
    /// `business_capability_update`.
    BusinessCapabilityUpdate(Box<BusinessCapabilityUpdateValue>),
    /// `business_username_updates`.
    BusinessUsernameUpdates(Box<BusinessUsernameUpdateValue>),
    /// `calls`.
    Calls(Box<CallsValue>),
    /// `flows`.
    Flows(Box<FlowsValue>),
    /// `group_lifecycle_update`.
    GroupLifecycleUpdate(Box<GroupsValue>),
    /// `group_participants_update` (also accepted: `group_participant_update`).
    GroupParticipantsUpdate(Box<GroupsValue>),
    /// `group_settings_update`.
    GroupSettingsUpdate(Box<GroupsValue>),
    /// `group_status_update`.
    GroupStatusUpdate(Box<GroupsValue>),
    /// `history`.
    History(Box<HistoryValue>),
    /// `message_template_components_update`.
    MessageTemplateComponentsUpdate(Box<TemplateComponentsUpdateValue>),
    /// `message_template_quality_update`.
    MessageTemplateQualityUpdate(Box<TemplateQualityUpdateValue>),
    /// `message_template_status_update`.
    MessageTemplateStatusUpdate(Box<TemplateStatusUpdateValue>),
    /// `partner_solutions`.
    PartnerSolutions(Box<PartnerSolutionsValue>),
    /// `payment_configuration_update`.
    PaymentConfigurationUpdate(Box<PaymentConfigurationUpdateValue>),
    /// `phone_number_name_update`.
    PhoneNumberNameUpdate(Box<PhoneNumberNameUpdateValue>),
    /// `phone_number_quality_update`.
    PhoneNumberQualityUpdate(Box<PhoneNumberQualityUpdateValue>),
    /// `security`.
    Security(Box<SecurityValue>),
    /// `smb_app_state_sync`.
    SmbAppStateSync(Box<SmbAppStateSyncValue>),
    /// `smb_message_echoes`.
    SmbMessageEchoes(Box<SmbMessageEchoesValue>),
    /// `template_category_update`.
    TemplateCategoryUpdate(Box<TemplateCategoryUpdateValue>),
    /// `template_correct_category_detection`.
    TemplateCorrectCategoryDetection(Box<TemplateCorrectCategoryDetectionValue>),
    /// `user_id_update`.
    UserIdUpdate(Box<UserIdUpdateValue>),
    /// `user_preferences`.
    UserPreferences(Box<UserPreferencesValue>),
    /// A field this crate does not type, or a known field whose value did
    /// not parse (then [`Change::parse_error`] says why). The value as
    /// received.
    Unknown(Value),
}

/// `parse_error` of a list-bearing value (`messages`, `calls`, …) that
/// parsed but held no item this crate models.
pub const NO_RECOGNIZED_ITEMS: &str = "the value contains no item this crate recognizes";

/// Parse `value` as `T`. List-bearing values that come out empty are kept
/// as [`ChangeValue::Unknown`] too: typed parsing ignores unknown keys, so a
/// list Meta adds to an existing field (say a new array next to `messages`)
/// would otherwise flatten to zero events and vanish without a trace.
fn typed<T: DeserializeOwned>(
    value: Value,
    wrap: fn(Box<T>) -> ChangeValue,
    is_empty: fn(&T) -> bool,
) -> (ChangeValue, Option<String>) {
    match T::deserialize(&value) {
        Ok(typed) if is_empty(&typed) => (
            ChangeValue::Unknown(value),
            Some(NO_RECOGNIZED_ITEMS.into()),
        ),
        Ok(typed) => (wrap(Box::new(typed)), None),
        Err(e) => (ChangeValue::Unknown(value), Some(e.to_string())),
    }
}

/// `is_empty` for single-object values, which always make one event.
fn never<T>(_: &T) -> bool {
    false
}

fn no_groups(v: &GroupsValue) -> bool {
    v.groups.is_empty()
}

impl ChangeValue {
    /// Type `value` according to `field`. Returns the parse error text when
    /// a known field fell back to [`ChangeValue::Unknown`].
    pub fn parse(field: &str, value: Value) -> (Self, Option<String>) {
        match field {
            "messages" => typed(value, Self::Messages, |v: &MessagesValue| {
                v.messages.is_empty() && v.statuses.is_empty() && v.errors.is_empty()
            }),
            "account_alerts" => typed(value, Self::AccountAlerts, never),
            "account_review_update" => typed(value, Self::AccountReviewUpdate, never),
            "account_settings_update" => typed(value, Self::AccountSettingsUpdate, never),
            "account_update" => typed(value, Self::AccountUpdate, never),
            "automatic_events" => {
                typed(value, Self::AutomaticEvents, |v: &AutomaticEventsValue| {
                    v.automatic_events.is_empty()
                })
            }
            "business_capability_update" => typed(value, Self::BusinessCapabilityUpdate, never),
            "business_username_updates" => typed(value, Self::BusinessUsernameUpdates, never),
            "calls" => typed(value, Self::Calls, |v: &CallsValue| {
                v.calls.is_empty() && v.statuses.is_empty() && v.errors.is_empty()
            }),
            "flows" => typed(value, Self::Flows, never),
            "group_lifecycle_update" => typed(value, Self::GroupLifecycleUpdate, no_groups),
            // `group_participant_update` is the incoming-payload reference's
            // spelling; every other page uses the plural.
            "group_participants_update" | "group_participant_update" => {
                typed(value, Self::GroupParticipantsUpdate, no_groups)
            }
            "group_settings_update" => typed(value, Self::GroupSettingsUpdate, no_groups),
            "group_status_update" => typed(value, Self::GroupStatusUpdate, no_groups),
            "history" => typed(value, Self::History, |v: &HistoryValue| {
                v.history.is_empty() && v.messages.is_empty() && v.message_echoes.is_empty()
            }),
            "message_template_components_update" => {
                typed(value, Self::MessageTemplateComponentsUpdate, never)
            }
            "message_template_quality_update" => {
                typed(value, Self::MessageTemplateQualityUpdate, never)
            }
            "message_template_status_update" => {
                typed(value, Self::MessageTemplateStatusUpdate, never)
            }
            "partner_solutions" => typed(value, Self::PartnerSolutions, never),
            "payment_configuration_update" => typed(value, Self::PaymentConfigurationUpdate, never),
            "phone_number_name_update" => typed(value, Self::PhoneNumberNameUpdate, never),
            "phone_number_quality_update" => typed(value, Self::PhoneNumberQualityUpdate, never),
            "security" => typed(value, Self::Security, never),
            "smb_app_state_sync" => {
                typed(value, Self::SmbAppStateSync, |v: &SmbAppStateSyncValue| {
                    v.state_sync.is_empty()
                })
            }
            "smb_message_echoes" => typed(
                value,
                Self::SmbMessageEchoes,
                |v: &SmbMessageEchoesValue| v.message_echoes.is_empty(),
            ),
            "template_category_update" => typed(value, Self::TemplateCategoryUpdate, never),
            "template_correct_category_detection" => {
                typed(value, Self::TemplateCorrectCategoryDetection, never)
            }
            "user_id_update" => typed(value, Self::UserIdUpdate, |v: &UserIdUpdateValue| {
                v.user_id_update.is_empty()
            }),
            "user_preferences" => {
                typed(value, Self::UserPreferences, |v: &UserPreferencesValue| {
                    v.user_preferences.is_empty()
                })
            }
            _ => (Self::Unknown(value), None),
        }
    }

    /// Whether this value fell back to (or always was) untyped.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

impl Serialize for ChangeValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Messages(v) => v.serialize(serializer),
            Self::AccountAlerts(v) => v.serialize(serializer),
            Self::AccountReviewUpdate(v) => v.serialize(serializer),
            Self::AccountSettingsUpdate(v) => v.serialize(serializer),
            Self::AccountUpdate(v) => v.serialize(serializer),
            Self::AutomaticEvents(v) => v.serialize(serializer),
            Self::BusinessCapabilityUpdate(v) => v.serialize(serializer),
            Self::BusinessUsernameUpdates(v) => v.serialize(serializer),
            Self::Calls(v) => v.serialize(serializer),
            Self::Flows(v) => v.serialize(serializer),
            Self::GroupLifecycleUpdate(v)
            | Self::GroupParticipantsUpdate(v)
            | Self::GroupSettingsUpdate(v)
            | Self::GroupStatusUpdate(v) => v.serialize(serializer),
            Self::History(v) => v.serialize(serializer),
            Self::MessageTemplateComponentsUpdate(v) => v.serialize(serializer),
            Self::MessageTemplateQualityUpdate(v) => v.serialize(serializer),
            Self::MessageTemplateStatusUpdate(v) => v.serialize(serializer),
            Self::PartnerSolutions(v) => v.serialize(serializer),
            Self::PaymentConfigurationUpdate(v) => v.serialize(serializer),
            Self::PhoneNumberNameUpdate(v) => v.serialize(serializer),
            Self::PhoneNumberQualityUpdate(v) => v.serialize(serializer),
            Self::Security(v) => v.serialize(serializer),
            Self::SmbAppStateSync(v) => v.serialize(serializer),
            Self::SmbMessageEchoes(v) => v.serialize(serializer),
            Self::TemplateCategoryUpdate(v) => v.serialize(serializer),
            Self::TemplateCorrectCategoryDetection(v) => v.serialize(serializer),
            Self::UserIdUpdate(v) => v.serialize(serializer),
            Self::UserPreferences(v) => v.serialize(serializer),
            Self::Unknown(v) => v.serialize(serializer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_field_is_kept_verbatim() {
        let c: Change =
            serde_json::from_value(json!({"field": "brand_new", "value": {"a": 1}})).unwrap();
        assert_eq!(c.value, ChangeValue::Unknown(json!({"a": 1})));
        assert!(c.parse_error.is_none());
    }

    #[test]
    fn known_field_with_bad_value_falls_back_with_the_reason() {
        let c: Change =
            serde_json::from_value(json!({"field": "security", "value": {"event": "PIN_CHANGED"}}))
                .unwrap();
        assert!(c.value.is_unknown());
        let error = c.parse_error.as_deref().unwrap();
        assert!(error.contains("display_phone_number"), "{error}");
        // The error survives our own serialization.
        let back: Value = serde_json::to_value(&c).unwrap();
        assert_eq!(back["parse_error"], json!(error));
    }

    #[test]
    fn payload_envelope_is_required() {
        assert!(WebhookPayload::from_slice(br#"{"entry": []}"#).is_err());
        assert!(WebhookPayload::from_slice(b"not json").is_err());
        let p = WebhookPayload::from_slice(br#"{"object": "x", "entry": []}"#).unwrap();
        assert!(p.entry.is_empty());
    }

    #[test]
    fn a_numeric_entry_id_does_not_unparse_the_whole_batch() {
        let p = WebhookPayload::from_slice(
            br#"{"object": "whatsapp_business_account",
                 "entry": [{"id": 102290129340398, "changes": []}]}"#,
        )
        .unwrap();
        assert_eq!(p.entry[0].id, "102290129340398");
    }
}
