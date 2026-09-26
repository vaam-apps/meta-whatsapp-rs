//! Normalized events: one [`WebhookEvent`] per message, status, error and
//! field change, whatever envelope it arrived in.
//!
//! This is what sinks receive. Each event carries the WABA id (unless the
//! payload does not name one: see [`WebhookEvent::waba_id`]) and, when the
//! field has one, the business phone number id, plus the WhatsApp user the
//! item is about ([`Contact`], matched from the change's `contacts` by BSUID
//! first and phone number second).
//!
//! Serialized with an `"event"` tag (`{"event": "message_received", …}`), the
//! same JSON the SSE helper streams.

use meta_whatsapp_core::GraphApiError;
use meta_whatsapp_core::ids::{BusinessId, PhoneNumberId, WaId, WabaId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::fields::common::find_contact;
use crate::fields::{
    AccountAlertsValue, AccountReviewUpdateValue, AccountSettingsUpdateValue, AccountUpdateEvent,
    AccountUpdateValue, AutomaticEvent, BusinessCapabilityUpdateValue, BusinessUsernameUpdateValue,
    Call, CallStatus, Contact, ConversationContext, FlowsValue, GroupUpdate, HistoryValue,
    InboundMessage, MessageEcho, MessagingHandoversValue, Metadata, PartnerSolutionsValue,
    PaymentConfigurationUpdateValue, PhoneNumberNameUpdateValue, PhoneNumberQualityUpdateValue,
    SecurityValue, StandbyItem, StateSyncItem, Status, TemplateCategoryUpdateValue,
    TemplateComponentsUpdateValue, TemplateCorrectCategoryDetectionValue,
    TemplateQualityUpdateValue, TemplateStatusUpdateValue, UserAction, UserIdUpdate,
    UserPreference,
};
use crate::payload::{Change, ChangeValue, WebhookPayload};

/// One normalized webhook event. See the [module docs](self).
///
/// Payloads are boxed so that a `Vec<WebhookEvent>` of small status events
/// does not pay for the largest variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WebhookEvent {
    /// A WhatsApp user sent the business a message (`messages`).
    MessageReceived {
        /// WABA.
        waba_id: WabaId,
        /// Receiving business phone number.
        phone_number_id: PhoneNumberId,
        /// Receiving business number, as displayed.
        display_phone_number: String,
        /// The sender, when the change listed them.
        contact: Option<Contact>,
        /// The message.
        message: Box<InboundMessage>,
        /// Conversation Routing's summary of the conversation so far, when
        /// the change carried one (the same on every message of the change).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conversation_context: Option<Box<ConversationContext>>,
    },
    /// A message the business sent changed status (`messages`).
    StatusUpdated {
        /// WABA.
        waba_id: WabaId,
        /// Sending business phone number.
        phone_number_id: PhoneNumberId,
        /// Sending business number, as displayed.
        display_phone_number: String,
        /// The recipient (omitted by Meta for `failed` statuses).
        contact: Option<Contact>,
        /// The status.
        status: Box<Status>,
    },
    /// A system-, app- or account-level error (`messages`, `calls`).
    ErrorReported {
        /// WABA.
        waba_id: WabaId,
        /// Field the error arrived in.
        field: String,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The error.
        error: Box<GraphApiError>,
    },
    /// The business sent a message from the WhatsApp Business app or a
    /// companion device (`smb_message_echoes`).
    MessageEchoed {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The recipient, when listed.
        contact: Option<Contact>,
        /// The echoed message.
        echo: Box<MessageEcho>,
    },
    /// A chunk of synchronized chat history, media content for it, or the
    /// "sharing declined" error (`history`). One event per change: chunks
    /// carry ordering metadata that must stay together.
    HistorySynced {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The whole history value.
        history: Box<HistoryValue>,
    },
    /// An address book contact changed in the WhatsApp Business app
    /// (`smb_app_state_sync`).
    AppStateSynced {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The change.
        item: Box<StateSyncItem>,
    },
    /// A call event (`calls`).
    CallUpdated {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The WhatsApp user on the call, when listed.
        contact: Option<Contact>,
        /// The call event.
        call: Box<Call>,
    },
    /// A business-initiated call changed status (`calls`).
    CallStatusUpdated {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The callee, when listed.
        contact: Option<Contact>,
        /// The status.
        status: Box<CallStatus>,
    },
    /// A user stopped or resumed marketing messages (`user_preferences`).
    UserPreferenceChanged {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The user, when listed.
        contact: Option<Contact>,
        /// The change.
        preference: Box<UserPreference>,
    },
    /// A user's BSUID changed (`user_id_update`).
    UserIdChanged {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The user, when listed.
        contact: Option<Contact>,
        /// The change.
        update: Box<UserIdUpdate>,
    },
    /// A user clicked a marketing message's body or call-to-action
    /// (`messages`, `user_actions`; `marketing-messages/track-click-events`).
    /// Meta names neither the user nor the message: see
    /// [`crate::fields::UserAction`].
    UserActionReported {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The action.
        action: Box<UserAction>,
    },
    /// A thread changed owner under Conversation Routing
    /// (`messaging_handovers`): you gained it (`control_passed`, reply to
    /// the user) or lost it (`control_taken`, stop replying). No endpoint
    /// reports the owner: keep it yourself from these, from which field a
    /// user's messages arrive on (`messages` or `standby`), from your own
    /// `release` (no event) and from the 24-hour idle timeout (see
    /// [`crate::fields::routing`]).
    ThreadControlChanged {
        /// WABA.
        waba_id: WabaId,
        /// The business phone number the thread belongs to.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The notification.
        update: Box<MessagingHandoversValue>,
    },
    /// A copy of a thread you observe without owning it (`standby`): an
    /// inbound message, the owner's send, or its status. Kept apart from
    /// [`Self::MessageReceived`] and [`Self::StatusUpdated`] on purpose: a
    /// standby partner must not reply.
    StandbyObserved {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The WhatsApp user, when the change listed them (messages only).
        contact: Option<Contact>,
        /// The copy.
        item: Box<StandbyItem>,
    },
    /// A purchase or lead was detected in a CTWA chat (`automatic_events`).
    AutomaticEventDetected {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// The event.
        detected: Box<AutomaticEvent>,
    },
    /// A group event (`group_lifecycle_update`, `group_participants_update`,
    /// `group_settings_update`, `group_status_update`).
    GroupUpdated {
        /// WABA.
        waba_id: WabaId,
        /// Business phone number owning the group.
        phone_number_id: PhoneNumberId,
        /// Business number, as displayed.
        display_phone_number: String,
        /// Which of the four fields.
        field: String,
        /// The event.
        update: Box<GroupUpdate>,
    },
    /// Flow status change or endpoint alert (`flows`).
    FlowUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The notification.
        update: Box<FlowsValue>,
    },
    /// `account_alerts`.
    AccountAlert {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The alert.
        alert: Box<AccountAlertsValue>,
    },
    /// `account_review_update`.
    AccountReviewUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The decision.
        update: Box<AccountReviewUpdateValue>,
    },
    /// `account_update`.
    ///
    /// Its entry id is not always the WABA. In Meta's examples an update
    /// with a `waba_info` (`PARTNER_ADDED`, `PARTNER_REMOVED`,
    /// `PARTNER_APP_INSTALLED`, `PARTNER_APP_UNINSTALLED`,
    /// `AD_ACCOUNT_LINKED`, `MM_LITE_TERMS_SIGNED`) names the customer's
    /// WABA in `waba_info.waba_id` and a business portfolio as the entry id
    /// (for `PARTNER_ADDED`, one of `solution_partner_business_ids`); every
    /// update without one carries the WABA as the entry id, except a
    /// `PARTNER_APP_INSTALLED` / `PARTNER_APP_UNINSTALLED`, which always
    /// goes to the partner's business (`embedded-signup/app-only-install`).
    ///
    /// An event serialized by a revision before `entry_id` existed (its
    /// `waba_id` was the entry id) is read back the same way: `waba_id`
    /// from its `waba_info`, the old value as `entry_id`, so a stored
    /// `PARTNER_*` event is never keyed by a business portfolio.
    #[serde(deserialize_with = "account_updated_stored")]
    AccountUpdated {
        /// The WABA the update is about: `waba_info.waba_id` when the update
        /// has a `waba_info`, else the entry id. `None` when a `waba_info`
        /// names no WABA, or a partner app event has no `waba_info`: the
        /// entry id is then not one.
        waba_id: Option<WabaId>,
        /// The entry's `id`, verbatim: the WABA for updates without a
        /// `waba_info`, a business portfolio for those with one and for
        /// the partner app events.
        entry_id: String,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<AccountUpdateValue>,
    },
    /// `account_settings_update`.
    AccountSettingsUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<AccountSettingsUpdateValue>,
    },
    /// `business_capability_update`.
    BusinessCapabilityUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<BusinessCapabilityUpdateValue>,
    },
    /// `business_username_updates`.
    BusinessUsernameUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<BusinessUsernameUpdateValue>,
    },
    /// `partner_solutions`. Its entry id is a business portfolio, not a WABA.
    PartnerSolutionUpdated {
        /// Business portfolio.
        business_id: BusinessId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<PartnerSolutionsValue>,
    },
    /// `payment_configuration_update`.
    PaymentConfigurationUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<PaymentConfigurationUpdateValue>,
    },
    /// `phone_number_name_update`.
    PhoneNumberNameUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<PhoneNumberNameUpdateValue>,
    },
    /// `phone_number_quality_update`.
    PhoneNumberQualityUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<PhoneNumberQualityUpdateValue>,
    },
    /// `security`.
    SecurityUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<SecurityValue>,
    },
    /// `message_template_components_update`.
    TemplateComponentsUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<TemplateComponentsUpdateValue>,
    },
    /// `message_template_quality_update`.
    TemplateQualityUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<TemplateQualityUpdateValue>,
    },
    /// `message_template_status_update`.
    TemplateStatusUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<TemplateStatusUpdateValue>,
    },
    /// `template_category_update`.
    TemplateCategoryUpdated {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The update.
        update: Box<TemplateCategoryUpdateValue>,
    },
    /// `template_correct_category_detection` (Direct Send category misuse).
    TemplateCategoryMisuseDetected {
        /// WABA.
        waba_id: WabaId,
        /// When.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The detection.
        update: Box<TemplateCorrectCategoryDetectionValue>,
    },
    /// A field this crate does not type, a known field whose value did not
    /// parse (`parse_error` says why), or a typed list field that produced
    /// no event (a shape Meta added after this crate was written).
    Unknown {
        /// The entry id, except for an `account_update` whose raw value has
        /// a non-blank `waba_info.waba_id`: that WABA (the entry id of such
        /// an update is a business portfolio, see [`Self::AccountUpdated`]).
        /// So a WABA id for every documented field but `partner_solutions`
        /// and an `account_update` whose `waba_info` names no WABA; for
        /// those two it is a business portfolio id.
        waba_id: WabaId,
        /// The field.
        field: String,
        /// When, if the entry said.
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        /// The change value as received.
        raw: Value,
        /// Why a known field was not typed.
        parse_error: Option<String>,
    },
    /// A body that passed signature verification but is not a webhook
    /// envelope at all. The handler acknowledges it (so Meta stops
    /// retrying) and hands it on as this event, with a `tracing::error!`.
    Unparsed {
        /// The body: its JSON when it is JSON, else a lossy UTF-8 string.
        raw: Value,
        /// The parse error.
        error: String,
    },
}

impl WebhookEvent {
    /// Every value [`Self::kind`] returns, in the order of its variants: to
    /// check a kind named in configuration (a listener's, a filter's) when
    /// it is read rather than never matching.
    pub const KINDS: &'static [&'static str] = &[
        "message_received",
        "status_updated",
        "error_reported",
        "message_echoed",
        "history_synced",
        "app_state_synced",
        "call_updated",
        "call_status_updated",
        "user_preference_changed",
        "user_id_changed",
        "user_action_reported",
        "thread_control_changed",
        "standby_observed",
        "automatic_event_detected",
        "group_updated",
        "flow_updated",
        "account_alert",
        "account_review_updated",
        "account_updated",
        "account_settings_updated",
        "business_capability_updated",
        "business_username_updated",
        "partner_solution_updated",
        "payment_configuration_updated",
        "phone_number_name_updated",
        "phone_number_quality_updated",
        "security_updated",
        "template_components_updated",
        "template_quality_updated",
        "template_status_updated",
        "template_category_updated",
        "template_category_misuse_detected",
        "unknown",
        "unparsed",
    ];

    /// Stable snake-case name of the variant; equals the serialized `event` tag.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::MessageReceived { .. } => "message_received",
            Self::StatusUpdated { .. } => "status_updated",
            Self::ErrorReported { .. } => "error_reported",
            Self::MessageEchoed { .. } => "message_echoed",
            Self::HistorySynced { .. } => "history_synced",
            Self::AppStateSynced { .. } => "app_state_synced",
            Self::CallUpdated { .. } => "call_updated",
            Self::CallStatusUpdated { .. } => "call_status_updated",
            Self::UserPreferenceChanged { .. } => "user_preference_changed",
            Self::UserIdChanged { .. } => "user_id_changed",
            Self::UserActionReported { .. } => "user_action_reported",
            Self::ThreadControlChanged { .. } => "thread_control_changed",
            Self::StandbyObserved { .. } => "standby_observed",
            Self::AutomaticEventDetected { .. } => "automatic_event_detected",
            Self::GroupUpdated { .. } => "group_updated",
            Self::FlowUpdated { .. } => "flow_updated",
            Self::AccountAlert { .. } => "account_alert",
            Self::AccountReviewUpdated { .. } => "account_review_updated",
            Self::AccountUpdated { .. } => "account_updated",
            Self::AccountSettingsUpdated { .. } => "account_settings_updated",
            Self::BusinessCapabilityUpdated { .. } => "business_capability_updated",
            Self::BusinessUsernameUpdated { .. } => "business_username_updated",
            Self::PartnerSolutionUpdated { .. } => "partner_solution_updated",
            Self::PaymentConfigurationUpdated { .. } => "payment_configuration_updated",
            Self::PhoneNumberNameUpdated { .. } => "phone_number_name_updated",
            Self::PhoneNumberQualityUpdated { .. } => "phone_number_quality_updated",
            Self::SecurityUpdated { .. } => "security_updated",
            Self::TemplateComponentsUpdated { .. } => "template_components_updated",
            Self::TemplateQualityUpdated { .. } => "template_quality_updated",
            Self::TemplateStatusUpdated { .. } => "template_status_updated",
            Self::TemplateCategoryUpdated { .. } => "template_category_updated",
            Self::TemplateCategoryMisuseDetected { .. } => "template_category_misuse_detected",
            Self::Unknown { .. } => "unknown",
            Self::Unparsed { .. } => "unparsed",
        }
    }

    /// The WABA the event belongs to. `None` for `partner_solutions` (a
    /// business portfolio, see [`Self::PartnerSolutionUpdated`]), for an
    /// [`Self::AccountUpdated`] whose `waba_info` names no WABA, and for
    /// [`Self::Unparsed`].
    pub fn waba_id(&self) -> Option<&WabaId> {
        match self {
            Self::MessageReceived { waba_id, .. }
            | Self::StatusUpdated { waba_id, .. }
            | Self::ErrorReported { waba_id, .. }
            | Self::MessageEchoed { waba_id, .. }
            | Self::HistorySynced { waba_id, .. }
            | Self::AppStateSynced { waba_id, .. }
            | Self::CallUpdated { waba_id, .. }
            | Self::CallStatusUpdated { waba_id, .. }
            | Self::UserPreferenceChanged { waba_id, .. }
            | Self::UserIdChanged { waba_id, .. }
            | Self::UserActionReported { waba_id, .. }
            | Self::ThreadControlChanged { waba_id, .. }
            | Self::StandbyObserved { waba_id, .. }
            | Self::AutomaticEventDetected { waba_id, .. }
            | Self::GroupUpdated { waba_id, .. }
            | Self::FlowUpdated { waba_id, .. }
            | Self::AccountAlert { waba_id, .. }
            | Self::AccountReviewUpdated { waba_id, .. }
            | Self::AccountSettingsUpdated { waba_id, .. }
            | Self::BusinessCapabilityUpdated { waba_id, .. }
            | Self::BusinessUsernameUpdated { waba_id, .. }
            | Self::PaymentConfigurationUpdated { waba_id, .. }
            | Self::PhoneNumberNameUpdated { waba_id, .. }
            | Self::PhoneNumberQualityUpdated { waba_id, .. }
            | Self::SecurityUpdated { waba_id, .. }
            | Self::TemplateComponentsUpdated { waba_id, .. }
            | Self::TemplateQualityUpdated { waba_id, .. }
            | Self::TemplateStatusUpdated { waba_id, .. }
            | Self::TemplateCategoryUpdated { waba_id, .. }
            | Self::TemplateCategoryMisuseDetected { waba_id, .. }
            | Self::Unknown { waba_id, .. } => Some(waba_id),
            Self::AccountUpdated { waba_id, .. } => waba_id.as_ref(),
            Self::PartnerSolutionUpdated { .. } | Self::Unparsed { .. } => None,
        }
    }

    /// The business phone number the event is about, when the field has
    /// one. Fields that only carry a display number
    /// (`phone_number_name_update`, `security`, …) return `None`.
    pub fn phone_number_id(&self) -> Option<&PhoneNumberId> {
        match self {
            Self::MessageReceived {
                phone_number_id, ..
            }
            | Self::StatusUpdated {
                phone_number_id, ..
            }
            | Self::ErrorReported {
                phone_number_id, ..
            }
            | Self::MessageEchoed {
                phone_number_id, ..
            }
            | Self::HistorySynced {
                phone_number_id, ..
            }
            | Self::AppStateSynced {
                phone_number_id, ..
            }
            | Self::CallUpdated {
                phone_number_id, ..
            }
            | Self::CallStatusUpdated {
                phone_number_id, ..
            }
            | Self::UserPreferenceChanged {
                phone_number_id, ..
            }
            | Self::UserIdChanged {
                phone_number_id, ..
            }
            | Self::UserActionReported {
                phone_number_id, ..
            }
            | Self::ThreadControlChanged {
                phone_number_id, ..
            }
            | Self::StandbyObserved {
                phone_number_id, ..
            }
            | Self::AutomaticEventDetected {
                phone_number_id, ..
            }
            | Self::GroupUpdated {
                phone_number_id, ..
            } => Some(phone_number_id),
            Self::AccountSettingsUpdated { update, .. } => update
                .phone_number_settings
                .as_ref()
                .map(|s| &s.phone_number_id),
            _ => None,
        }
    }

    /// The WhatsApp user the event is about, when the change listed them.
    pub fn contact(&self) -> Option<&Contact> {
        match self {
            Self::MessageReceived { contact, .. }
            | Self::StatusUpdated { contact, .. }
            | Self::MessageEchoed { contact, .. }
            | Self::CallUpdated { contact, .. }
            | Self::CallStatusUpdated { contact, .. }
            | Self::UserPreferenceChanged { contact, .. }
            | Self::UserIdChanged { contact, .. }
            | Self::StandbyObserved { contact, .. } => contact.as_ref(),
            _ => None,
        }
    }

    /// Key under which [`crate::DedupGuard`] remembers this event, so Meta's
    /// retries (up to 7 days) are delivered once.
    ///
    /// - Inbound messages: the message id (`wamid.…`).
    /// - Statuses: `{message id}:{status}`, plus `:{participant}` for group
    ///   messages, since Meta aggregates one status per participant for the
    ///   same message id into a single webhook (`groups/webhooks`).
    /// - Echoes: `echo:{id}`; calls: `call:{id}:{event}`; call statuses:
    ///   `call:{id}:{status}`; automatic events: `auto:{id}:{event_name}`.
    /// - Standby copies: `standby:` followed by the key the same item has
    ///   outside standby (`standby:{id}`, `standby:echo:{id}`,
    ///   `standby:{id}:{status}[:{participant}]`), so a copy never collides
    ///   with the owner-side event of the same message.
    /// - Every other event: `{kind}:{sha256 of its canonical JSON}`. The
    ///   hash covers the entry `time` where Meta sends one, so an identical
    ///   notification re-issued later is a new event, while a retry of the
    ///   same body is not. The hash is of this crate's typed form: an event
    ///   parsed by two different versions of the crate may hash differently.
    /// - `None` (never deduplicated): [`Self::ErrorReported`] (the same
    ///   error legitimately recurs and `messages` entries carry no time) and
    ///   [`Self::Unparsed`].
    pub fn dedup_key(&self) -> Option<String> {
        match self {
            Self::MessageReceived { message, .. } => Some(message.id.to_string()),
            Self::StatusUpdated { status, .. } => Some(status_key(status)),
            Self::StandbyObserved { item, .. } => Some(match item.as_ref() {
                StandbyItem::Message(message) => format!("standby:{}", message.id),
                StandbyItem::Echo(echo) => format!("standby:echo:{}", echo.id),
                StandbyItem::Status(status) => format!("standby:{}", status_key(status)),
            }),
            Self::MessageEchoed { echo, .. } => Some(format!("echo:{}", echo.id)),
            Self::CallUpdated { call, .. } => Some(format!("call:{}:{}", call.id, call.event)),
            Self::CallStatusUpdated { status, .. } => {
                Some(format!("call:{}:{}", status.id, status.status))
            }
            Self::AutomaticEventDetected { detected, .. } => {
                Some(format!("auto:{}:{}", detected.id, detected.event_name))
            }
            Self::ErrorReported { .. } | Self::Unparsed { .. } => None,
            other => {
                let value = serde_json::to_value(other).ok()?;
                let mut canonical = String::new();
                write_canonical(&value, &mut canonical);
                let digest = Sha256::digest(canonical.as_bytes());
                Some(format!("{}:{}", other.kind(), hex::encode(digest)))
            }
        }
    }
}

/// `{message id}:{status}[:{participant}]`: see [`WebhookEvent::dedup_key`].
fn status_key(status: &Status) -> String {
    let mut key = format!("{}:{}", status.id, status.status);
    let participant = status
        .recipient_participant_user_id
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| status.recipient_participant_id.clone());
    if let Some(participant) = participant {
        key.push(':');
        key.push_str(&participant);
    }
    key
}

/// JSON with object keys sorted, so the hash does not depend on whether
/// `serde_json`'s `preserve_order` feature is enabled somewhere in the build.
fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                if let Some(v) = map.get(key) {
                    write_canonical(v, out);
                }
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Flatten a payload into events, in payload order. See [`WebhookPayload::into_events`].
pub fn events(payload: &WebhookPayload) -> Vec<WebhookEvent> {
    payload.clone().into_events()
}

impl WebhookPayload {
    /// Flatten into events, in payload order: per entry, per change, per
    /// item. Consumes the payload so large bodies (history) are not copied.
    pub fn into_events(self) -> Vec<WebhookEvent> {
        let mut out = Vec::new();
        for entry in self.entry {
            let ctx = Ctx {
                entry_id: entry.id,
                time: entry.time,
            };
            for change in entry.changes {
                ctx.push(change, &mut out);
            }
        }
        out
    }
}

/// The WABA an `account_update` is about (`webhooks/reference/account_update`).
///
/// The reference's syntax calls the entry id the WABA id, but its examples
/// disagree for every update that has a `waba_info`: `PARTNER_ADDED`,
/// `PARTNER_REMOVED`, `PARTNER_APP_INSTALLED`, `PARTNER_APP_UNINSTALLED`,
/// `AD_ACCOUNT_LINKED` and `MM_LITE_TERMS_SIGNED` all show entry id
/// `2949482758682047`, which `PARTNER_ADDED` lists in
/// `solution_partner_business_ids` (a business portfolio), and the customer's
/// WABA in `waba_info.waba_id`. Every example without a `waba_info`
/// (`ACCOUNT_DELETED`, …, and the coexistence `PARTNER_REMOVED` of
/// `embedded-signup/onboarding-business-app-users`) shows the WABA as the
/// entry id, except the `PARTNER_APP_UNINSTALLED` of
/// `embedded-signup/app-only-install`, whose entry id is
/// `<PARTNER_BUSINESS_ID>`: the partner app events always go to the
/// partner's business. So: `waba_info.waba_id` when there is a `waba_info`,
/// `None` if it names none (the entry id is then a business, never a WABA)
/// and for a partner app event without one, and the entry id otherwise.
fn account_update_waba(update: &AccountUpdateValue, entry: WabaId) -> Option<WabaId> {
    match &update.waba_info {
        Some(info) => info
            .waba_id
            .as_ref()
            .filter(|id| !id.as_str().trim().is_empty())
            .cloned(),
        None if matches!(
            update.event,
            AccountUpdateEvent::PartnerAppInstalled | AccountUpdateEvent::PartnerAppUninstalled
        ) =>
        {
            None
        }
        None => Some(entry),
    }
}

/// The WABA of an `account_update` that did not parse into
/// [`AccountUpdateValue`]: its raw `waba_info.waba_id` when it is a
/// non-blank string (as [`account_update_waba`] would take it), else the
/// entry id.
fn unparsed_account_update_waba(raw: &Value, entry: WabaId) -> WabaId {
    raw.get("waba_info")
        .and_then(|info| info.get("waba_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map_or(entry, WabaId::new)
}

/// The fields of [`WebhookEvent::AccountUpdated`], as a variant
/// deserializer returns them.
type AccountUpdatedFields = (
    Option<WabaId>,
    String,
    Option<OffsetDateTime>,
    Box<AccountUpdateValue>,
);

/// Read a stored [`WebhookEvent::AccountUpdated`]. One written before
/// `entry_id` existed has none, and its `waba_id` is the entry id (a
/// business portfolio for the `PARTNER_*` updates): derive both as
/// flattening does now.
fn account_updated_stored<'de, D>(d: D) -> Result<AccountUpdatedFields, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Stored {
        #[serde(default)]
        waba_id: Option<WabaId>,
        #[serde(default)]
        entry_id: Option<String>,
        #[serde(default, with = "meta_whatsapp_core::timestamp::unix_option")]
        time: Option<OffsetDateTime>,
        update: Box<AccountUpdateValue>,
    }
    let stored = Stored::deserialize(d)?;
    if let Some(entry_id) = stored.entry_id {
        return Ok((stored.waba_id, entry_id, stored.time, stored.update));
    }
    let entry_id = stored.waba_id.map(WabaId::into_inner).unwrap_or_default();
    let waba_id = if entry_id.trim().is_empty() {
        // No entry id either: only `waba_info` can name the WABA.
        stored
            .update
            .waba_info
            .as_ref()
            .and_then(|i| i.waba_id.clone())
            .filter(|id| !id.as_str().trim().is_empty())
    } else {
        account_update_waba(&stored.update, WabaId::new(entry_id.clone()))
    };
    Ok((waba_id, entry_id, stored.time, stored.update))
}

/// Per-entry context while flattening.
struct Ctx {
    entry_id: String,
    time: Option<OffsetDateTime>,
}

impl Ctx {
    fn waba(&self) -> WabaId {
        WabaId::new(self.entry_id.clone())
    }

    /// Every change yields at least one event: list-bearing values with no
    /// recognized item were already turned into `Unknown` by
    /// [`ChangeValue::parse`].
    #[allow(clippy::too_many_lines)] // one arm per documented field
    fn push(&self, change: Change, out: &mut Vec<WebhookEvent>) {
        let Change {
            field,
            value,
            parse_error,
        } = change;
        let field = field.as_str();
        let waba_id = self.waba();
        let time = self.time;
        match value {
            ChangeValue::Messages(v) => {
                let v = *v;
                let Metadata {
                    display_phone_number,
                    phone_number_id,
                } = v.metadata;
                let conversation_context = v.conversation_context.map(Box::new);
                for message in v.messages {
                    let contact = find_contact(
                        &v.contacts,
                        &[message.from_user_id.as_ref()],
                        &[message.from.as_ref().map(WaId::as_str)],
                    )
                    .cloned();
                    out.push(WebhookEvent::MessageReceived {
                        waba_id: waba_id.clone(),
                        phone_number_id: phone_number_id.clone(),
                        display_phone_number: display_phone_number.clone(),
                        contact,
                        message: Box::new(message),
                        conversation_context: conversation_context.clone(),
                    });
                }
                for status in v.statuses {
                    let contact = find_contact(
                        &v.contacts,
                        &[
                            status.recipient_user_id.as_ref(),
                            status.recipient_participant_user_id.as_ref(),
                        ],
                        &[
                            status.recipient_id.as_deref(),
                            status.recipient_participant_id.as_deref(),
                        ],
                    )
                    .cloned();
                    out.push(WebhookEvent::StatusUpdated {
                        waba_id: waba_id.clone(),
                        phone_number_id: phone_number_id.clone(),
                        display_phone_number: display_phone_number.clone(),
                        contact,
                        status: Box::new(status),
                    });
                }
                for error in v.errors {
                    out.push(WebhookEvent::ErrorReported {
                        waba_id: waba_id.clone(),
                        field: field.to_owned(),
                        phone_number_id: phone_number_id.clone(),
                        display_phone_number: display_phone_number.clone(),
                        error: Box::new(error),
                    });
                }
                for action in v.user_actions {
                    out.push(WebhookEvent::UserActionReported {
                        waba_id: waba_id.clone(),
                        phone_number_id: phone_number_id.clone(),
                        display_phone_number: display_phone_number.clone(),
                        action: Box::new(action),
                    });
                }
            }
            ChangeValue::Calls(v) => {
                let v = *v;
                let Metadata {
                    display_phone_number,
                    phone_number_id,
                } = v.metadata;
                for call in v.calls {
                    let contact = find_contact(
                        &v.contacts,
                        &[call.to_user_id.as_ref(), call.from_user_id.as_ref()],
                        &[call.to.as_deref(), call.from.as_deref()],
                    )
                    .cloned();
                    out.push(WebhookEvent::CallUpdated {
                        waba_id: waba_id.clone(),
                        phone_number_id: phone_number_id.clone(),
                        display_phone_number: display_phone_number.clone(),
                        contact,
                        call: Box::new(call),
                    });
                }
                for status in v.statuses {
                    let contact = find_contact(
                        &v.contacts,
                        &[status.recipient_user_id.as_ref()],
                        &[status.recipient_id.as_deref()],
                    )
                    .cloned();
                    out.push(WebhookEvent::CallStatusUpdated {
                        waba_id: waba_id.clone(),
                        phone_number_id: phone_number_id.clone(),
                        display_phone_number: display_phone_number.clone(),
                        contact,
                        status: Box::new(status),
                    });
                }
                for error in v.errors {
                    out.push(WebhookEvent::ErrorReported {
                        waba_id: waba_id.clone(),
                        field: field.to_owned(),
                        phone_number_id: phone_number_id.clone(),
                        display_phone_number: display_phone_number.clone(),
                        error: Box::new(error),
                    });
                }
            }
            ChangeValue::SmbMessageEchoes(v) => {
                let v = *v;
                for echo in v.message_echoes {
                    let contact = find_contact(
                        &v.contacts,
                        &[echo.to_user_id.as_ref()],
                        &[echo.to.as_ref().map(WaId::as_str)],
                    )
                    .cloned();
                    out.push(WebhookEvent::MessageEchoed {
                        waba_id: waba_id.clone(),
                        phone_number_id: v.metadata.phone_number_id.clone(),
                        display_phone_number: v.metadata.display_phone_number.clone(),
                        contact,
                        echo: Box::new(echo),
                    });
                }
            }
            ChangeValue::History(v) => out.push(WebhookEvent::HistorySynced {
                waba_id,
                phone_number_id: v.metadata.phone_number_id.clone(),
                display_phone_number: v.metadata.display_phone_number.clone(),
                history: v,
            }),
            ChangeValue::SmbAppStateSync(v) => {
                let v = *v;
                for item in v.state_sync {
                    out.push(WebhookEvent::AppStateSynced {
                        waba_id: waba_id.clone(),
                        phone_number_id: v.metadata.phone_number_id.clone(),
                        display_phone_number: v.metadata.display_phone_number.clone(),
                        item: Box::new(item),
                    });
                }
            }
            ChangeValue::UserPreferences(v) => {
                let v = *v;
                for preference in v.user_preferences {
                    let contact = find_contact(
                        &v.contacts,
                        &[preference.user_id.as_ref()],
                        &[preference.wa_id.as_ref().map(WaId::as_str)],
                    )
                    .cloned();
                    out.push(WebhookEvent::UserPreferenceChanged {
                        waba_id: waba_id.clone(),
                        phone_number_id: v.metadata.phone_number_id.clone(),
                        display_phone_number: v.metadata.display_phone_number.clone(),
                        contact,
                        preference: Box::new(preference),
                    });
                }
            }
            ChangeValue::UserIdUpdate(v) => {
                let v = *v;
                for update in v.user_id_update {
                    let contact = find_contact(
                        &v.contacts,
                        &[
                            Some(&update.user_id.previous),
                            Some(&update.user_id.current),
                        ],
                        &[update.wa_id.as_ref().map(WaId::as_str)],
                    )
                    .cloned();
                    out.push(WebhookEvent::UserIdChanged {
                        waba_id: waba_id.clone(),
                        phone_number_id: v.metadata.phone_number_id.clone(),
                        display_phone_number: v.metadata.display_phone_number.clone(),
                        contact,
                        update: Box::new(update),
                    });
                }
            }
            ChangeValue::MessagingHandovers(update) => {
                out.push(WebhookEvent::ThreadControlChanged {
                    waba_id,
                    phone_number_id: update.recipient.phone_number_id.clone(),
                    display_phone_number: update.recipient.display_phone_number.clone(),
                    update,
                });
            }
            ChangeValue::Standby(v) => {
                let v = *v;
                let Metadata {
                    display_phone_number,
                    phone_number_id,
                } = v.metadata;
                let standby = v.standby;
                let mut push = |contact: Option<Contact>, item: StandbyItem| {
                    out.push(WebhookEvent::StandbyObserved {
                        waba_id: waba_id.clone(),
                        phone_number_id: phone_number_id.clone(),
                        display_phone_number: display_phone_number.clone(),
                        contact,
                        item: Box::new(item),
                    });
                };
                for message in standby.messages {
                    let contact = find_contact(
                        &standby.contacts,
                        &[message.from_user_id.as_ref()],
                        &[message.from.as_ref().map(WaId::as_str)],
                    )
                    .cloned();
                    push(contact, StandbyItem::Message(message));
                }
                for echo in standby.message_echoes {
                    push(None, StandbyItem::Echo(echo));
                }
                for status in standby.statuses {
                    let contact = find_contact(
                        &standby.contacts,
                        &[
                            status.recipient_user_id.as_ref(),
                            status.recipient_participant_user_id.as_ref(),
                        ],
                        &[
                            status.recipient_id.as_deref(),
                            status.recipient_participant_id.as_deref(),
                        ],
                    )
                    .cloned();
                    push(contact, StandbyItem::Status(status));
                }
            }
            ChangeValue::AutomaticEvents(v) => {
                let v = *v;
                for detected in v.automatic_events {
                    out.push(WebhookEvent::AutomaticEventDetected {
                        waba_id: waba_id.clone(),
                        phone_number_id: v.metadata.phone_number_id.clone(),
                        display_phone_number: v.metadata.display_phone_number.clone(),
                        detected: Box::new(detected),
                    });
                }
            }
            ChangeValue::GroupLifecycleUpdate(v)
            | ChangeValue::GroupParticipantsUpdate(v)
            | ChangeValue::GroupSettingsUpdate(v)
            | ChangeValue::GroupStatusUpdate(v) => {
                let v = *v;
                for update in v.groups {
                    out.push(WebhookEvent::GroupUpdated {
                        waba_id: waba_id.clone(),
                        phone_number_id: v.metadata.phone_number_id.clone(),
                        display_phone_number: v.metadata.display_phone_number.clone(),
                        field: field.to_owned(),
                        update: Box::new(update),
                    });
                }
            }
            ChangeValue::Flows(update) => out.push(WebhookEvent::FlowUpdated {
                waba_id,
                time,
                update,
            }),
            ChangeValue::AccountAlerts(alert) => out.push(WebhookEvent::AccountAlert {
                waba_id,
                time,
                alert,
            }),
            ChangeValue::AccountReviewUpdate(update) => {
                out.push(WebhookEvent::AccountReviewUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::AccountUpdate(update) => out.push(WebhookEvent::AccountUpdated {
                waba_id: account_update_waba(&update, waba_id),
                entry_id: self.entry_id.clone(),
                time,
                update,
            }),
            ChangeValue::AccountSettingsUpdate(update) => {
                out.push(WebhookEvent::AccountSettingsUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::BusinessCapabilityUpdate(update) => {
                out.push(WebhookEvent::BusinessCapabilityUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::BusinessUsernameUpdates(update) => {
                out.push(WebhookEvent::BusinessUsernameUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::PartnerSolutions(update) => {
                out.push(WebhookEvent::PartnerSolutionUpdated {
                    business_id: BusinessId::new(self.entry_id.clone()),
                    time,
                    update,
                });
            }
            ChangeValue::PaymentConfigurationUpdate(update) => {
                out.push(WebhookEvent::PaymentConfigurationUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::PhoneNumberNameUpdate(update) => {
                out.push(WebhookEvent::PhoneNumberNameUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::PhoneNumberQualityUpdate(update) => {
                out.push(WebhookEvent::PhoneNumberQualityUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::Security(update) => out.push(WebhookEvent::SecurityUpdated {
                waba_id,
                time,
                update,
            }),
            ChangeValue::MessageTemplateComponentsUpdate(update) => {
                out.push(WebhookEvent::TemplateComponentsUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::MessageTemplateQualityUpdate(update) => {
                out.push(WebhookEvent::TemplateQualityUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::MessageTemplateStatusUpdate(update) => {
                out.push(WebhookEvent::TemplateStatusUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::TemplateCategoryUpdate(update) => {
                out.push(WebhookEvent::TemplateCategoryUpdated {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::TemplateCorrectCategoryDetection(update) => {
                out.push(WebhookEvent::TemplateCategoryMisuseDetected {
                    waba_id,
                    time,
                    update,
                });
            }
            ChangeValue::Unknown(raw) => out.push(WebhookEvent::Unknown {
                waba_id: if field == "account_update" {
                    unparsed_account_update_waba(&raw, waba_id)
                } else {
                    waba_id
                },
                field: field.to_owned(),
                time,
                raw,
                parse_error,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(v: &Value) -> WebhookPayload {
        serde_json::from_value(v.clone()).unwrap()
    }

    #[test]
    fn list_value_with_only_new_arrays_surfaces_raw_instead_of_vanishing() {
        let value = json!({"messaging_product": "whatsapp",
                           "metadata": {"display_phone_number": "1", "phone_number_id": "2"},
                           "reactions_v2": [{"x": 1}]});
        let p = payload(
            &json!({"object": "whatsapp_business_account", "entry": [{"id": "1", "changes": [{
                "field": "messages", "value": value
            }]}]}),
        );
        let events = p.into_events();
        assert_eq!(events.len(), 1);
        let WebhookEvent::Unknown {
            field,
            parse_error,
            raw,
            ..
        } = &events[0]
        else {
            panic!("{events:?}")
        };
        assert_eq!(field, "messages");
        assert_eq!(raw, &value);
        assert_eq!(
            parse_error.as_deref(),
            Some(crate::payload::NO_RECOGNIZED_ITEMS)
        );
    }

    #[test]
    fn kind_matches_the_serialized_tag() {
        let e = WebhookEvent::Unparsed {
            raw: json!("x"),
            error: "e".into(),
        };
        assert_eq!(serde_json::to_value(&e).unwrap()["event"], e.kind());
        let e = WebhookEvent::Unknown {
            waba_id: "1".into(),
            field: "f".into(),
            time: None,
            raw: json!({}),
            parse_error: None,
        };
        assert_eq!(serde_json::to_value(&e).unwrap()["event"], e.kind());
    }

    /// `KINDS` is `kind()`'s arms, in order: read from this file's source
    /// the way `meta-whatsapp-server`'s `every_library_event_type_is_classified`
    /// reads it (`kind()` has no catch-all arm, so its arms are every
    /// variant). The fixtures sweep (`tests/fixtures.rs`) checks the kinds
    /// real events report.
    #[test]
    fn kinds_lists_every_arm_of_kind_in_order() {
        let source = include_str!("event.rs");
        let body = source
            .split("pub fn kind(&self) -> &'static str {")
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .expect("WebhookEvent::kind");
        let arms: Vec<&str> = body
            .split("=> \"")
            .skip(1)
            .map(|rest| &rest[..rest.find('"').unwrap()])
            .collect();
        assert!(arms.len() >= 34, "{arms:?}");
        assert_eq!(WebhookEvent::KINDS, arms.as_slice());
        let unique: std::collections::BTreeSet<_> = WebhookEvent::KINDS.iter().collect();
        assert_eq!(unique.len(), WebhookEvent::KINDS.len());
        // And for the two variants built here, the kind is listed.
        for e in [
            WebhookEvent::Unparsed {
                raw: json!("x"),
                error: "e".into(),
            },
            WebhookEvent::Unknown {
                waba_id: "1".into(),
                field: "f".into(),
                time: None,
                raw: json!({}),
                parse_error: None,
            },
        ] {
            assert!(WebhookEvent::KINDS.contains(&e.kind()), "{}", e.kind());
        }
    }

    #[test]
    fn canonical_json_ignores_key_order() {
        let mut a = String::new();
        let mut b = String::new();
        write_canonical(&json!({"b": [1, {"y": 2, "x": 1}], "a": "\"q"}), &mut a);
        write_canonical(&json!({"a": "\"q", "b": [1, {"x": 1, "y": 2}]}), &mut b);
        assert_eq!(a, b);
        assert_eq!(a, r#"{"a":"\"q","b":[1,{"x":1,"y":2}]}"#);
    }
}
