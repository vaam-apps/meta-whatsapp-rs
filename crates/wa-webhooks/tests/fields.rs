//! Every non-`messages` field, from its reference page's example.

// Test code may unwrap (clippy.toml); `allow-unwrap-in-tests` only reaches
// `#[test]` fns, not the helpers of an integration test crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use pretty_assertions::assert_eq;
use serde_json::json;
use time::OffsetDateTime;
use wa_webhooks::fields::{
    AccountUpdateEvent, AlertEntityType, AlertSeverity, AlertStatus, AlertType, AutomaticEventName,
    BusinessUsernameStatus, CallDirection, CallEventType, CallStatusValue, CallTerminateStatus,
    CertificationStatus, DisconnectionInitiator, DisconnectionReason, FlowAlertState, FlowEvent,
    FlowStatus, GroupRemovalInitiator, GroupUpdateType, HistoryMessageStatus, MessageContent,
    MessagingLimit, MessagingLimitTier, NameRejectionReason, PartnerSolutionEvent,
    PartnerSolutionStatus, PaymentConfigurationStatus, PhoneNumberQualityEvent, PreferenceCategory,
    PreferenceValue, RestrictionType, ReviewDecision, SecurityEvent, StateSyncAction,
    StateSyncType, TemplateButtonType, TemplateCategory, TemplatePauseTitle, TemplateQualityScore,
    TemplateRejectionReason, TemplateStatusEvent, WabaBanState,
};
use wa_webhooks::{WebhookEvent, WebhookPayload};

fn ts(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).unwrap()
}

fn one(name: &str) -> WebhookEvent {
    let mut events = common::events(name);
    assert_eq!(events.len(), 1, "{name}: {events:?}");
    events.remove(0)
}

#[test]
fn account_alerts() {
    let WebhookEvent::AccountAlert {
        waba_id,
        time,
        alert,
    } = one("fields/account_alerts.json")
    else {
        panic!()
    };
    assert_eq!(waba_id.as_str(), "102290129340398");
    assert_eq!(time, Some(ts(1745612159)));
    assert_eq!(alert.entity_type, AlertEntityType::Business);
    assert_eq!(alert.entity_id, "506914307656634");
    assert_eq!(
        alert.alert_info.alert_severity,
        Some(AlertSeverity::Warning)
    );
    assert_eq!(alert.alert_info.alert_status, Some(AlertStatus::Active));
    assert_eq!(
        alert.alert_info.alert_type,
        AlertType::IncreasedCapabilitiesEligibilityDeferred
    );
    assert!(alert.alert_info.alert_description.is_some());
}

#[test]
fn account_review_update() {
    let WebhookEvent::AccountReviewUpdated { update, time, .. } =
        one("fields/account_review_update.json")
    else {
        panic!()
    };
    assert_eq!(update.decision, ReviewDecision::Approved);
    assert_eq!(time, Some(ts(1739321024)));
}

fn account_update(name: &str) -> wa_webhooks::fields::AccountUpdateValue {
    match one(name) {
        WebhookEvent::AccountUpdated { update, .. } => *update,
        other => panic!("{name}: {other:?}"),
    }
}

fn account_update_value(value: &serde_json::Value) -> wa_webhooks::fields::AccountUpdateValue {
    let body = json!({"object": "whatsapp_business_account", "entry": [{
        "id": "102290129340398", "time": 1739321024,
        "changes": [{"field": "account_update", "value": value}]
    }]});
    let mut events = WebhookPayload::from_slice(body.to_string().as_bytes())
        .unwrap()
        .into_events();
    match events.remove(0) {
        WebhookEvent::AccountUpdated { update, .. } => *update,
        other => panic!("{other:?}"),
    }
}

/// `embedded-signup/website-optional` and `marketing-messages/onboarding`
/// document `account_update` shapes the reference page does not show.
#[test]
fn account_update_shapes_from_other_pages() {
    let u = account_update_value(&json!({
        "event": "PARTNER_CLIENT_CERTIFICATION_NEEDED",
        "partner_client_certification_needed_info": {"client_business_id": "2729063490586005"}
    }));
    assert_eq!(
        u.event,
        AccountUpdateEvent::PartnerClientCertificationNeeded
    );
    assert_eq!(
        u.partner_client_certification_needed_info
            .unwrap()
            .client_business_id
            .unwrap()
            .as_str(),
        "2729063490586005"
    );

    let u = account_update_value(&json!({
        "event": "AD_ACCOUNT_LINKED",
        "waba_info": {"waba_id": "980198427658004", "ad_account_id": "633456882212545",
                      "owner_business_id": "131426832456945"}
    }));
    let info = u.waba_info.unwrap();
    assert_eq!(info.ad_account_id.as_deref(), Some("633456882212545"));
    assert_eq!(info.ad_account_linked, None);
}

#[test]
fn account_update_every_example() {
    let u = account_update("fields/account_update_deleted.json");
    assert_eq!(u.event, AccountUpdateEvent::AccountDeleted);

    let u = account_update("fields/account_update_restriction.json");
    assert_eq!(u.event, AccountUpdateEvent::AccountRestriction);
    assert_eq!(u.restriction_info.len(), 2);
    assert_eq!(
        u.restriction_info[0].restriction_type,
        RestrictionType::RestrictedBizInitiatedMessaging
    );
    assert_eq!(u.restriction_info[1].expiration, Some(ts(1641330498)));
    assert!(u.restriction_info[0].remediation.is_none());

    let u = account_update("fields/account_update_violation.json");
    assert_eq!(
        u.violation_info.unwrap().violation_type.as_deref(),
        Some("ADULT")
    );

    let u = account_update("fields/account_update_ad_account_linked.json");
    assert_eq!(u.event, AccountUpdateEvent::AdAccountLinked);
    let info = u.waba_info.unwrap();
    assert_eq!(info.ad_account_linked.as_deref(), Some("980198427534243"));
    assert_eq!(info.waba_id.unwrap().as_str(), "980198427658004");

    let u = account_update("fields/account_update_auth_intl.json");
    let elig = u.auth_international_rate_eligibility.unwrap();
    assert_eq!(elig.start_time, Some(ts(1748780624)));
    assert_eq!(elig.exception_countries[0].country_code, "ID");
    assert_eq!(elig.exception_countries[0].start_time, Some(ts(1751347424)));

    let u = account_update("fields/account_update_disabled.json");
    let ban = u.ban_info.unwrap();
    assert_eq!(ban.waba_ban_state, Some(WabaBanState::Reinstate));
    assert_eq!(ban.waba_ban_date.as_deref(), Some("April 17, 2025"));

    let u = account_update("fields/account_update_mm_lite_terms.json");
    assert_eq!(u.event, AccountUpdateEvent::MmLiteTermsSigned);

    let u = account_update("fields/account_update_partner_added.json");
    let info = u.waba_info.unwrap();
    assert_eq!(info.solution_id.as_deref(), Some("1715120619246906"));
    assert_eq!(info.solution_partner_business_ids.len(), 2);

    let u = account_update("fields/account_update_partner_app_installed.json");
    assert_eq!(u.event, AccountUpdateEvent::PartnerAppInstalled);
    assert_eq!(
        u.waba_info.unwrap().partner_app_id.unwrap().as_str(),
        "5731794616896507"
    );

    let u = account_update("fields/account_update_partner_app_uninstalled.json");
    assert_eq!(u.event, AccountUpdateEvent::PartnerAppUninstalled);

    let u = account_update("fields/account_update_certification.json");
    let cert = u.partner_client_certification_info.unwrap();
    assert_eq!(cert.status, Some(CertificationStatus::Approved));
    assert_eq!(cert.rejection_reasons, ["NONE"]);
    assert_eq!(
        cert.client_business_id.unwrap().as_str(),
        "2729063490586005"
    );

    let u = account_update("fields/account_update_partner_removed.json");
    assert_eq!(u.event, AccountUpdateEvent::PartnerRemoved);
    assert!(u.disconnection_info.is_none());

    let u = account_update("fields/account_update_partner_removed_disconnection.json");
    let d = u.disconnection_info.unwrap();
    assert_eq!(d.reason, Some(DisconnectionReason::PrimaryInactivity));
    assert_eq!(d.initiated_by, Some(DisconnectionInitiator::System));

    let u = account_update("fields/account_update_coexistence_partner_removed.json");
    assert_eq!(u.phone_number.as_deref(), Some("15550783881"));
    assert!(u.disconnection_info.is_some());

    let u = account_update("fields/account_update_primary_location.json");
    assert_eq!(u.country.as_deref(), Some("IN"));

    let u = account_update("fields/account_update_volume_tier.json");
    let tier = u.volume_tier_info.unwrap();
    assert_eq!(tier.tier.as_deref(), Some("25000001:50000000"));
    assert_eq!(tier.effective_month.as_deref(), Some("2025-11"));
    assert_eq!(tier.tier_update_time, Some(ts(1743451903)));

    assert_eq!(
        account_update("fields/account_update_offboarded.json").event,
        AccountUpdateEvent::AccountOffboarded
    );
    assert_eq!(
        account_update("fields/account_update_reconnected.json").event,
        AccountUpdateEvent::AccountReconnected
    );
}

#[test]
fn account_settings_update_has_the_phone_number_id() {
    let event = one("fields/account_settings_update.json");
    assert_eq!(
        event
            .phone_number_id()
            .map(wa_core::ids::PhoneNumberId::as_str),
        Some("106540352242922")
    );
    let WebhookEvent::AccountSettingsUpdated { update, .. } = event else {
        panic!()
    };
    assert_eq!(
        update.settings_type.as_deref(),
        Some("PHONE_NUMBER_SETTINGS")
    );
    assert_eq!(update.timestamp, Some(ts(1671644824)));
    let calling = update.phone_number_settings.unwrap().calling.unwrap();
    assert_eq!(calling["status"], "ENABLED");
}

#[test]
fn business_capability_update_accepts_the_numeric_example() {
    let WebhookEvent::BusinessCapabilityUpdated { update, .. } =
        one("fields/business_capability_update.json")
    else {
        panic!()
    };
    assert_eq!(
        update.max_daily_conversations_per_business,
        Some(MessagingLimit::Count(2000))
    );
    assert_eq!(update.max_phone_numbers_per_waba, Some(25));
}

#[test]
fn business_username_updates() {
    let events = common::events("fields/business_username_updates.json");
    let statuses: Vec<_> = events
        .iter()
        .map(|e| match e {
            WebhookEvent::BusinessUsernameUpdated { update, .. } => {
                (update.status.clone(), update.username.clone())
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        statuses,
        [
            (BusinessUsernameStatus::Approved, Some("luckyshrub".into())),
            (BusinessUsernameStatus::Deleted, None)
        ]
    );
}

#[test]
fn partner_solutions_is_keyed_by_business_not_waba() {
    let event = one("fields/partner_solutions.json");
    assert_eq!(event.waba_id(), None);
    let WebhookEvent::PartnerSolutionUpdated {
        business_id,
        update,
        ..
    } = event
    else {
        panic!()
    };
    assert_eq!(business_id.as_str(), "506914307656634");
    assert_eq!(update.event, PartnerSolutionEvent::SolutionCreated);
    assert_eq!(update.solution_id, "774485461512159");
    assert_eq!(
        update.solution_status,
        Some(PartnerSolutionStatus::Initiated)
    );
}

#[test]
fn payment_configuration_update() {
    let WebhookEvent::PaymentConfigurationUpdated { update, .. } =
        one("fields/payment_configuration_update.json")
    else {
        panic!()
    };
    assert_eq!(update.configuration_name, "razorpay-prod");
    assert_eq!(update.provider_name.as_deref(), Some("razorpay"));
    assert_eq!(update.provider_mid.as_deref(), Some("acc_GP4lfNA0iIMn5B"));
    assert_eq!(
        update.status,
        Some(PaymentConfigurationStatus::NeedsTesting)
    );
    assert_eq!(update.created_timestamp, Some(ts(1748827100)));
    assert_eq!(update.updated_timestamp, Some(ts(1749320300)));
}

#[test]
fn phone_number_name_and_quality_updates() {
    let event = one("fields/phone_number_name_update.json");
    assert_eq!(
        event.phone_number_id(),
        None,
        "only a display number is sent"
    );
    let WebhookEvent::PhoneNumberNameUpdated { update, .. } = event else {
        panic!()
    };
    assert_eq!(update.display_phone_number, "15550783881");
    assert_eq!(update.decision, ReviewDecision::Approved);
    assert_eq!(
        update.requested_verified_name.as_deref(),
        Some("Lucky Shrub")
    );
    assert_eq!(update.rejection_reason, None);
    assert_eq!(
        serde_json::from_value::<NameRejectionReason>(json!("NAME_FORMAT_UNACCEPTABLE")).unwrap(),
        NameRejectionReason::NameFormatUnacceptable
    );

    let WebhookEvent::PhoneNumberQualityUpdated { update, .. } =
        one("fields/phone_number_quality_update.json")
    else {
        panic!()
    };
    assert_eq!(update.event, PhoneNumberQualityEvent::ThroughputUpgrade);
    assert_eq!(
        update.current_limit,
        Some(MessagingLimitTier::TierUnlimited)
    );
    assert_eq!(update.old_limit, None);
}

#[test]
fn security() {
    let WebhookEvent::SecurityUpdated { update, .. } = one("fields/security.json") else {
        panic!()
    };
    assert_eq!(update.event, SecurityEvent::PinResetRequest);
    assert_eq!(update.requester.as_deref(), Some("61555822107539"));
}

#[test]
fn template_fields() {
    let WebhookEvent::TemplateComponentsUpdated { update, .. } =
        one("fields/message_template_components_update.json")
    else {
        panic!()
    };
    assert_eq!(update.message_template_id.as_str(), "1315502779341834");
    assert_eq!(update.message_template_name, "order_confirmation");
    assert_eq!(
        update.message_template_title.as_deref(),
        Some("Your order is confirmed!")
    );
    assert_eq!(update.message_template_buttons.len(), 2);
    assert_eq!(
        update.message_template_buttons[0].message_template_button_type,
        Some(TemplateButtonType::PhoneNumber)
    );
    assert_eq!(
        update.message_template_buttons[1]
            .message_template_button_url
            .as_deref(),
        Some("https://www.luckyshrub.com/support")
    );

    let WebhookEvent::TemplateQualityUpdated { update, .. } =
        one("fields/message_template_quality_update.json")
    else {
        panic!()
    };
    assert_eq!(
        update.previous_quality_score,
        Some(TemplateQualityScore::Green)
    );
    assert_eq!(update.new_quality_score, TemplateQualityScore::Yellow);
    assert_eq!(update.message_template_id.as_str(), "806312974732579");

    let WebhookEvent::TemplateStatusUpdated { update, .. } =
        one("fields/message_template_status_update_approved.json")
    else {
        panic!()
    };
    assert_eq!(update.event, TemplateStatusEvent::Approved);
    assert_eq!(update.reason, Some(TemplateRejectionReason::None));
    assert_eq!(
        update.message_template_category,
        Some(TemplateCategory::Utility)
    );

    let WebhookEvent::TemplateStatusUpdated { update, .. } =
        one("fields/message_template_status_update_rejected.json")
    else {
        panic!()
    };
    assert_eq!(update.event, TemplateStatusEvent::Rejected);
    assert_eq!(update.reason, Some(TemplateRejectionReason::InvalidFormat));
    assert!(update.rejection_info.unwrap().recommendation.is_some());

    let WebhookEvent::TemplateCategoryUpdated { update, .. } =
        one("fields/template_category_update_impending.json")
    else {
        panic!()
    };
    assert!(update.is_impending());
    assert_eq!(update.new_category, Some(TemplateCategory::Utility));
    assert_eq!(update.correct_category, Some(TemplateCategory::Marketing));
    assert_eq!(update.category_update_timestamp, Some(ts(1746169200)));

    let WebhookEvent::TemplateCategoryUpdated { update, .. } =
        one("fields/template_category_update_completed.json")
    else {
        panic!()
    };
    assert!(!update.is_impending());
    assert_eq!(update.previous_category, Some(TemplateCategory::Utility));

    let WebhookEvent::TemplateCategoryMisuseDetected { update, .. } =
        one("fields/template_correct_category_detection.json")
    else {
        panic!()
    };
    assert_eq!(update.category, Some(TemplateCategory::Utility));
    assert_eq!(update.correct_category, Some(TemplateCategory::Marketing));
}

#[test]
fn template_pause_and_disable_details() {
    let value = json!({
        "event": "PAUSED", "message_template_id": 1, "message_template_name": "n",
        "message_template_language": "en", "reason": null,
        "disable_info": {"disable_date": 1751234563},
        "other_info": {"title": "FIRST_PAUSE", "description": "Paused for low quality."}
    });
    let v: wa_webhooks::fields::TemplateStatusUpdateValue = serde_json::from_value(value).unwrap();
    assert_eq!(v.event, TemplateStatusEvent::Paused);
    assert_eq!(v.reason, None);
    assert_eq!(v.disable_info.unwrap().disable_date, Some(ts(1751234563)));
    assert_eq!(
        v.other_info.unwrap().title,
        Some(TemplatePauseTitle::FirstPause)
    );
}

#[test]
fn user_preferences_and_user_id_update() {
    let WebhookEvent::UserPreferenceChanged {
        contact,
        preference,
        ..
    } = one("fields/user_preferences.json")
    else {
        panic!()
    };
    assert_eq!(contact.unwrap().wa_id.unwrap().as_str(), "16505551234");
    assert_eq!(preference.category, PreferenceCategory::MarketingMessages);
    assert_eq!(preference.value, PreferenceValue::Resume);
    assert_eq!(preference.timestamp, ts(1731705721));

    let WebhookEvent::UserPreferenceChanged {
        contact,
        preference,
        ..
    } = one("bsuid/user_preferences_bsuid.json")
    else {
        panic!()
    };
    assert_eq!(contact.unwrap().username(), Some("realsheenanelson"));
    assert_eq!(preference.value, PreferenceValue::Stop);
    assert!(preference.wa_id.is_none());

    let WebhookEvent::UserIdChanged {
        contact, update, ..
    } = one("fields/user_id_update.json")
    else {
        panic!()
    };
    assert_eq!(contact.unwrap().name(), Some("Sheena Nelson"));
    assert_eq!(update.user_id.previous.as_str(), "US.13491208655302741918");
    assert_eq!(update.user_id.current.as_str(), "US.29847561203948576612");
    assert_eq!(
        update.parent_user_id.unwrap().current.as_str(),
        "US.ENT.22926810323997955941"
    );
    assert_eq!(update.timestamp, ts(1767168400));
}

#[test]
fn history_threads_media_and_declined() {
    let WebhookEvent::HistorySynced {
        history,
        phone_number_id,
        ..
    } = one("fields/history_threads.json")
    else {
        panic!()
    };
    assert_eq!(phone_number_id.as_str(), "106540352242922");
    let chunk = &history.history[0];
    let meta = chunk.metadata.as_ref().unwrap();
    assert_eq!(
        (meta.phase, meta.chunk_order, meta.progress),
        (Some(0), Some(1), Some(55))
    );
    assert_eq!(chunk.threads.len(), 2);
    let thread = &chunk.threads[0];
    assert_eq!(thread.id.as_deref(), Some("16505551234"));
    assert_eq!(thread.messages.len(), 3);
    assert!(matches!(
        thread.messages[1].content,
        MessageContent::MediaPlaceholder
    ));
    assert_eq!(
        thread.messages[1].history_context.as_ref().unwrap().status,
        HistoryMessageStatus::Played
    );
    let MessageContent::Text(text) = &thread.messages[2].content else {
        panic!()
    };
    assert_eq!(text.body, "Thanks!");

    let WebhookEvent::HistorySynced { history, .. } = one("fields/history_media.json") else {
        panic!()
    };
    let MessageContent::Image(img) = &history.messages[0].content else {
        panic!()
    };
    assert_eq!(img.id.as_ref().unwrap().as_str(), "24230790383178626");

    let WebhookEvent::HistorySynced { history, .. } = one("fields/history_declined.json") else {
        panic!()
    };
    assert_eq!(history.history[0].errors[0].code, 2593109);
    assert!(history.history[0].threads.is_empty());

    let WebhookEvent::HistorySynced { history, .. } = one("bsuid/history_thread_context.json")
    else {
        panic!()
    };
    let ctx = history.history[0].threads[0].context.as_ref().unwrap();
    assert_eq!(ctx.username.as_deref(), Some("realsheenanelson"));
    assert!(history.history[0].threads[0].id.is_none());
    assert_eq!(
        history.message_echoes[0]
            .to_user_id
            .as_ref()
            .unwrap()
            .as_str(),
        "US.13491208655302741918"
    );
}

#[test]
fn smb_app_state_sync_one_event_per_item() {
    let WebhookEvent::AppStateSynced { item, .. } = one("fields/smb_app_state_sync.json") else {
        panic!()
    };
    assert_eq!(item.sync_type, StateSyncType::Contact);
    assert_eq!(item.action, StateSyncAction::Add);
    let contact = item.contact.as_ref().unwrap();
    assert_eq!(contact.full_name.as_deref(), Some("Pablo Morales"));
    assert_eq!(contact.phone_number.as_deref(), Some("16505551234"));
    assert_eq!(
        item.metadata.as_ref().unwrap().timestamp,
        Some(ts(1739321024))
    );

    let events = common::events("bsuid/smb_app_state_sync_bsuid.json");
    assert_eq!(events.len(), 2);
    let WebhookEvent::AppStateSynced { item, .. } = &events[1] else {
        panic!()
    };
    assert_eq!(item.action, StateSyncAction::Remove);
    assert!(item.contact.as_ref().unwrap().full_name.is_none());
}

#[test]
fn smb_message_echoes() {
    let WebhookEvent::MessageEchoed { echo, contact, .. } =
        one("fields/smb_message_echoes_text.json")
    else {
        panic!()
    };
    assert!(contact.is_none());
    assert_eq!(echo.from.as_deref(), Some("15550783881"));
    assert_eq!(echo.to.as_ref().unwrap().as_str(), "16505551234");
    assert!(matches!(echo.content, MessageContent::Text(_)));

    let WebhookEvent::MessageEchoed { echo, .. } = one("fields/smb_message_echoes_revoke.json")
    else {
        panic!()
    };
    assert!(matches!(echo.content, MessageContent::Revoke(_)));

    let WebhookEvent::MessageEchoed { echo, .. } = one("fields/smb_message_echoes_edit.json")
    else {
        panic!()
    };
    let MessageContent::Edit(edit) = &echo.content else {
        panic!()
    };
    assert!(matches!(*edit.message.content, MessageContent::Image(_)));

    let event = one("bsuid/smb_message_echoes_bsuid.json");
    assert!(event.dedup_key().unwrap().starts_with("echo:wamid."));
    let WebhookEvent::MessageEchoed { echo, contact, .. } = event else {
        panic!()
    };
    assert!(echo.to.is_none());
    assert_eq!(contact.unwrap().username(), Some("realsheenanelson"));
}

#[test]
fn calls() {
    let event = one("fields/calls_connect.json");
    assert_eq!(
        event.dedup_key().as_deref(),
        Some("call:wacid.ABGGFjFVU2AfAgo6V-Hc5eCgK5Gh:connect")
    );
    let WebhookEvent::CallUpdated { call, contact, .. } = event else {
        panic!()
    };
    assert_eq!(call.event, CallEventType::Connect);
    assert_eq!(call.direction, Some(CallDirection::BusinessInitiated));
    assert_eq!(call.session.as_ref().unwrap().sdp_type, "answer");
    assert_eq!(contact.unwrap().username(), Some("pablomorales"));

    let WebhookEvent::CallUpdated { call, .. } = one("fields/calls_connect_sample.json") else {
        panic!()
    };
    assert!(call.connection.is_some());

    let WebhookEvent::CallUpdated { call, .. } = one("fields/calls_created.json") else {
        panic!()
    };
    assert_eq!(call.event, CallEventType::CallCreated);
    assert!(call.session.is_none());

    let events = common::events("fields/calls_terminate.json");
    let [
        WebhookEvent::CallUpdated { call, .. },
        WebhookEvent::ErrorReported { field, error, .. },
    ] = events.as_slice()
    else {
        panic!("{events:?}")
    };
    assert_eq!(call.status, Some(CallTerminateStatus::Completed));
    assert_eq!(call.duration, Some(120));
    assert_eq!(call.start_time, Some(ts(1671644824)));
    assert_eq!(call.end_time, Some(ts(1671644944)));
    assert_eq!(field, "calls");
    assert_eq!(error.code, 131000);
    assert_eq!(
        serde_json::from_value::<CallTerminateStatus>(json!("Failed")).unwrap(),
        CallTerminateStatus::Failed
    );

    let WebhookEvent::CallStatusUpdated {
        status, contact, ..
    } = one("fields/calls_status.json")
    else {
        panic!()
    };
    assert_eq!(status.status, CallStatusValue::Ringing);
    assert_eq!(
        status.biz_opaque_callback_data.as_deref(),
        Some("random_string")
    );
    assert!(contact.is_some());

    let WebhookEvent::CallUpdated { call, .. } = one("fields/calls_recording_available.json")
    else {
        panic!()
    };
    let audio = call.call_recording.unwrap().audio.unwrap();
    assert_eq!(audio.id.unwrap().as_str(), "1002764438271669");

    let WebhookEvent::CallUpdated { call, .. } = one("fields/calls_transcription_available.json")
    else {
        panic!()
    };
    assert_eq!(call.event, CallEventType::CallTranscriptionAvailable);
    let doc = call.call_transcript.unwrap().document.unwrap();
    assert_eq!(doc.mime_type.as_deref(), Some("application/json"));
}

#[test]
fn flows() {
    let WebhookEvent::FlowUpdated { update, time, .. } = one("fields/flows_status_change.json")
    else {
        panic!()
    };
    assert_eq!(time, Some(ts(1684969340)));
    assert_eq!(update.event, FlowEvent::FlowStatusChange);
    assert_eq!(update.flow_id.as_str(), "6627390910605886");
    assert_eq!(update.old_status, Some(FlowStatus::Draft));
    assert_eq!(update.new_status, Some(FlowStatus::Published));

    let WebhookEvent::FlowUpdated { update, .. } = one("fields/flows_client_error_rate.json")
    else {
        panic!()
    };
    assert_eq!(update.error_rate, Some(14.28));
    assert_eq!(update.alert_state, Some(FlowAlertState::Activated));
    assert_eq!(
        update.errors[0].error_type.as_deref(),
        Some("INVALID_SCREEN_TRANSITION")
    );
    assert_eq!(update.errors[0].error_count, Some(2));

    let WebhookEvent::FlowUpdated { update, .. } = one("fields/flows_endpoint_latency.json") else {
        panic!()
    };
    assert_eq!(update.p90_latency, Some(8000.0));
    assert_eq!(update.requests_count, Some(34));

    let WebhookEvent::FlowUpdated { update, .. } = one("fields/flows_endpoint_availability.json")
    else {
        panic!()
    };
    assert_eq!(update.availability, Some(75.0));
    assert_eq!(update.threshold, Some(90.0));

    let WebhookEvent::FlowUpdated { update, .. } = one("fields/flows_endpoint_error_rate.json")
    else {
        panic!()
    };
    assert_eq!(update.event, FlowEvent::EndpointErrorRate);
}

#[test]
fn groups() {
    let events = common::events("fields/group_lifecycle_update.json");
    assert_eq!(events.len(), 3);
    let kinds: Vec<_> = events
        .iter()
        .map(|e| match e {
            WebhookEvent::GroupUpdated { field, update, .. } => {
                assert_eq!(field, "group_lifecycle_update");
                (update.update_type.clone(), update.errors.len())
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (GroupUpdateType::GroupCreate, 0),
            (GroupUpdateType::GroupCreate, 1),
            (GroupUpdateType::GroupDelete, 0)
        ]
    );

    let events = common::events("fields/group_participants_update.json");
    let updates: Vec<_> = events
        .iter()
        .map(|e| match e {
            WebhookEvent::GroupUpdated { update, .. } => update.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(updates[0].update_type, GroupUpdateType::ParticipantsAdd);
    assert_eq!(
        updates[0].added_participants[0].username.as_deref(),
        Some("pablomorales")
    );
    assert_eq!(updates[1].update_type, GroupUpdateType::JoinRequestCreated);
    assert_eq!(
        updates[1].join_request_id.as_deref(),
        Some("MTIzNDU2Nzg5MDEyMzQ1Ng")
    );
    assert_eq!(
        updates[2].initiated_by,
        Some(GroupRemovalInitiator::Business)
    );
    assert_eq!(updates[2].failed_participants[0].errors[0].code, 131009);
    assert_eq!(
        updates[3].initiated_by,
        Some(GroupRemovalInitiator::Participant)
    );
    assert_eq!(
        serde_json::from_value::<GroupUpdateType>(json!("group_add_participants")).unwrap(),
        GroupUpdateType::ParticipantsAdd
    );

    let WebhookEvent::GroupUpdated { update, .. } = one("fields/group_settings_update.json") else {
        panic!()
    };
    assert_eq!(
        update.group_subject.as_ref().unwrap().update_successful,
        Some(false)
    );
    assert_eq!(
        update
            .profile_picture
            .as_ref()
            .unwrap()
            .mime_type
            .as_deref(),
        Some("image/jpeg")
    );

    let events = common::events("fields/group_status_update.json");
    let WebhookEvent::GroupUpdated { update, .. } = &events[1] else {
        panic!()
    };
    assert_eq!(update.update_type, GroupUpdateType::SuspendCleared);
}

#[test]
fn group_participant_update_singular_spelling_is_accepted() {
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "1", "changes": [{
        "field": "group_participant_update",
        "value": {"messaging_product": "whatsapp",
                  "metadata": {"display_phone_number": "1", "phone_number_id": "2"},
                  "groups": [{"timestamp": 1, "group_id": "g", "type": "group_remove_participants",
                              "request_id": "r", "removed_participants": [{"wa_id": "1"}]}]}
    }]}]});
    let events = WebhookPayload::from_slice(body.to_string().as_bytes())
        .unwrap()
        .into_events();
    let [WebhookEvent::GroupUpdated { update, .. }] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(update.update_type, GroupUpdateType::ParticipantsRemove);
}

#[test]
fn automatic_events() {
    let event = one("fields/automatic_events_purchase.json");
    assert_eq!(
        event.dedup_key().as_deref(),
        Some("auto:wamid.HBgLMTIwNjY3NzQ3OTgVAgARGBIwRkU4NDI5Nzk3RjZDMzE2RUMA:Purchase")
    );
    let WebhookEvent::AutomaticEventDetected { detected, .. } = event else {
        panic!()
    };
    assert_eq!(detected.event_name, AutomaticEventName::Purchase);
    let data = detected.custom_data.as_ref().unwrap();
    assert_eq!(data.currency.as_deref(), Some("USD"));
    assert_eq!(data.value, Some(25000.0));

    let WebhookEvent::AutomaticEventDetected { detected, .. } =
        one("fields/automatic_events_lead.json")
    else {
        panic!()
    };
    assert_eq!(detected.event_name, AutomaticEventName::LeadSubmitted);
    assert!(detected.custom_data.is_none());
}

#[test]
fn unknown_field_parses_to_unknown_event() {
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
        "time": 1767168000, "changes": [{"field": "brand_new_field", "value": {"x": [1, 2]}}]}]});
    let events = WebhookPayload::from_slice(body.to_string().as_bytes())
        .unwrap()
        .into_events();
    let [
        WebhookEvent::Unknown {
            waba_id,
            field,
            time,
            raw,
            parse_error,
        },
    ] = events.as_slice()
    else {
        panic!("{events:?}")
    };
    assert_eq!(waba_id.as_str(), "102290129340398");
    assert_eq!(field, "brand_new_field");
    assert_eq!(*time, Some(ts(1767168000)));
    assert_eq!(raw, &json!({"x": [1, 2]}));
    assert_eq!(parse_error, &None);
    assert!(events[0].dedup_key().unwrap().starts_with("unknown:"));
}

#[test]
fn known_field_with_malformed_value_does_not_fail_the_batch() {
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "1", "changes": [
        {"field": "security", "value": {"event": "PIN_CHANGED"}},
        {"field": "account_review_update", "value": {"decision": "APPROVED"}}
    ]}]});
    let events = WebhookPayload::from_slice(body.to_string().as_bytes())
        .unwrap()
        .into_events();
    assert_eq!(events.len(), 2);
    let WebhookEvent::Unknown {
        field, parse_error, ..
    } = &events[0]
    else {
        panic!("{events:?}")
    };
    assert_eq!(field, "security");
    assert!(
        parse_error
            .as_deref()
            .unwrap()
            .contains("display_phone_number")
    );
    assert!(matches!(
        events[1],
        WebhookEvent::AccountReviewUpdated { .. }
    ));
}

#[test]
fn unknown_enum_values_parse() {
    let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "1", "time": 1, "changes": [
        {"field": "account_update", "value": {"event": "SOMETHING_NEW", "restriction_info": [
            {"restriction_type": "RESTRICTED_EVERYTHING"}]}}
    ]}]});
    let events = WebhookPayload::from_slice(body.to_string().as_bytes())
        .unwrap()
        .into_events();
    let [WebhookEvent::AccountUpdated { update, .. }] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert_eq!(
        update.event,
        AccountUpdateEvent::Other("SOMETHING_NEW".into())
    );
    assert_eq!(
        update.restriction_info[0].restriction_type,
        RestrictionType::Other("RESTRICTED_EVERYTHING".into())
    );
}

#[test]
fn management_dedup_keys_include_entry_time() {
    let at = |time: i64| {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "1", "time": time,
            "changes": [{"field": "account_review_update", "value": {"decision": "APPROVED"}}]}]});
        WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0)
            .dedup_key()
            .unwrap()
    };
    assert_eq!(at(1), at(1), "a retry is the same event");
    assert_ne!(at(1), at(2), "a re-issued notification is a new event");
    assert!(at(1).starts_with("account_review_updated:"));
}
