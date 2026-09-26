//! The conformance manifest: every page and every fixture. See `conformance.rs`.
//!
//! Adding a fixture: add a `Case` (the test lists unlisted files); its
//! `events` are what Meta's page says the payload means, written out by
//! hand, not copied from this crate's output.

use super::{Case, Origin, Page, Status, ev};

pub const PAGES: &[Page] = &[
    Page {
        path: "business-scoped-user-ids",
        status: Status::Typed,
        examples: &[
            "Contacts webhook",
            "business_username_updates webhook",
            "Status messages webhooks (1 of 4)",
            "Status messages webhooks (2 of 4)",
            "Status messages webhooks (3 of 4)",
            "Status messages webhooks (4 of 4)",
            "Incoming messages webhooks (1 of 2)",
            "Incoming messages webhooks (2 of 2)",
            "System messages webhooks",
            "user_preferences webhooks",
            "user_id_update webhooks",
            "Status messages webhooks for groups",
            "group_participants_update webhooks",
            "Call permission request webhooks",
            "Business-initiated connected calls webhooks",
            "User-initiated connected calls webhooks",
            "Business-initiated terminated calls webhooks",
            "User-initiated terminated calls webhooks",
            "Business-initiated calls status webhooks",
            "History webhooks (1 of 2)",
            "History webhooks (2 of 2)",
            "smb_message_echoes webhooks",
            "smb_app_state_sync webhooks",
            "Revoke messages webhooks (1 of 2)",
            "Revoke messages webhooks (2 of 2)",
            "Edit messages webhooks (1 of 2)",
            "Edit messages webhooks (2 of 2)",
        ],
    },
    Page {
        path: "calling/business-initiated-calls",
        status: Status::Typed,
        examples: &[
            "Part 3: You establish the call connection using webhook signaling (1 of 2)",
            "Part 3: You establish the call connection using webhook signaling (2 of 2)",
            "Part 4: Your business or the WhatsApp user terminates the call",
            "Call connect webhook",
            "Call status webhook",
            "Call terminate webhook",
        ],
    },
    Page {
        path: "calling/call-recording",
        status: Status::Typed,
        examples: &["Recording-available webhook"],
    },
    Page {
        path: "calling/call-settings",
        status: Status::Typed,
        examples: &[
            "Webhook payload (1 of 2)",
            "Webhook payload (2 of 2)",
            "Webhook (1 of 3)",
            "Webhook (2 of 3)",
            "Webhook (3 of 3)",
            "Warning webhook",
            "Enforcement webhook",
        ],
    },
    Page {
        path: "calling/call-transcription",
        status: Status::Typed,
        examples: &["Transcription-available webhook"],
    },
    Page {
        path: "calling/reference",
        status: Status::Typed,
        examples: &[
            "Call Connect webhook",
            "Call created webhook",
            "Call status webhook",
            "Call terminate webhook",
            "Call connect webhook",
        ],
    },
    Page {
        path: "calling/user-call-permissions",
        status: Status::Fragments(
            "shows the `call_permission_reply` message object, not a whole payload; the fixture wraps it",
        ),
        examples: &[],
    },
    Page {
        path: "calling/user-initiated-calls",
        status: Status::Typed,
        examples: &[
            "Part 1: A WhatsApp user calls your business from their client app",
            "Part 4: Your business or the WhatsApp user terminates the call",
            "Call Connect webhook",
            "Call Terminate webhook",
        ],
    },
    Page {
        path: "conversation-routing/conversation-context",
        status: Status::Typed,
        examples: &[
            "Incoming message webhook",
            "control_passed handover webhook",
        ],
    },
    Page {
        path: "conversation-routing/thread-control",
        status: Status::Typed,
        examples: &["control_passed", "control_taken"],
    },
    Page {
        path: "direct-send/integrity-and-content-guidelines",
        status: Status::Typed,
        examples: &[
            "Webhook for template pausing",
            "Webhook for category misuse",
            "Warning (no restriction)",
            "Active restriction (rate-limiting, 7-day, 30-day, or permanent)",
            "Restriction lifted",
        ],
    },
    Page {
        path: "embedded-signup/automatic-events-api",
        status: Status::Typed,
        examples: &["Lead gen event example", "Purchase event example"],
    },
    Page {
        path: "embedded-signup/onboarding-business-app-users",
        status: Status::Typed,
        examples: &[
            "Example payload (1 of 4)",
            "Example payload (2 of 4)",
            "Example payload (3 of 4)",
            "Example (1 of 2)",
            "Example (2 of 2)",
            "Example payload — chat history sharing approved {#example-history-approved}",
            "Example payload for media message asset {#example-media-asset}",
            "Example payload (4 of 4)",
        ],
    },
    Page {
        path: "flows/guides/flowswebhooks",
        status: Status::Typed,
        examples: &[
            "Status change webhook {#statuschange}",
            "Possible resolutions (1 of 4)",
            "Possible resolutions (2 of 4)",
            "Possible resolutions (3 of 4)",
            "Possible resolutions (4 of 4)",
        ],
    },
    Page {
        path: "groups/groups-messaging",
        status: Status::Typed,
        examples: &[
            "Group message sent example",
            "Group message failed example",
            "Receive group message webhook sample",
            "Receive unsupported group message webhook sample",
        ],
    },
    Page {
        path: "groups/webhooks",
        status: Status::Typed,
        examples: &[
            "Group create succeed",
            "Group create fail",
            "Delete group succeed",
            "Delete group fails",
            "User joined group using invite link succeed",
            "User accepts or cancels join request",
            "Join request approved",
            "Group participant remove succeed",
            "Group participant remove with participants partially fails",
            "Group participant remove fails",
            "Group participant leaves webhook",
            "Group settings update succeed",
            "Group settings update partial fail",
            "Group settings update total fail",
            "Group suspended",
            "Group suspension cleared",
            "Multiple participants, single message",
            "Multiple messages, single participant",
            "Group message delivered",
            "Group message read (_With pricing_)",
            "Group message read (_Without pricing_)",
        ],
    },
    Page {
        path: "messages/address-messages",
        status: Status::Fragments(
            "shows the `nfm_reply` message object, not a whole payload; the fixture wraps it",
        ),
        examples: &[],
    },
    Page {
        path: "solution-providers/partner-led-business-verification",
        status: Status::Typed,
        examples: &["Example webhook"],
    },
    Page {
        path: "webhooks/message_echoes",
        status: Status::Unreadable(
            "listed in llms.txt; `.md` and HTML both answer \"Page Not Found\" (2026-09-26)",
        ),
        examples: &[],
    },
    Page {
        path: "webhooks/overview",
        status: Status::Typed,
        examples: &["Webhooks (payload example)"],
    },
    Page {
        path: "webhooks/reference/account_alerts",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/account_review_update",
        status: Status::Typed,
        examples: &["Example payload"],
    },
    Page {
        path: "webhooks/reference/account_update",
        status: Status::Typed,
        examples: &[
            "Account deleted",
            "Account restriction",
            "Account violation",
            "Ad account linked",
            "Authentication-international eligibility",
            "Disabled update",
            "MM API for WhatsApp terms of service",
            "Partner added",
            "Partner app installed",
            "Partner app uninstalled",
            "Partner-led business verification status",
            "Partner removed",
            "Partner removed (WhatsApp Business app disconnection) {#partner-removed-disconnection}",
            "Primary business location set",
            "Pricing tiering update",
            "Account offboarded {#account-offboarded}",
            "Account reconnected {#account-reconnected}",
        ],
    },
    Page {
        path: "webhooks/reference/automatic_events",
        status: Status::Typed,
        examples: &["Lead gen event", "Purchase event"],
    },
    Page {
        path: "webhooks/reference/business_capability_update",
        status: Status::Typed,
        examples: &["Payload example"],
    },
    Page {
        path: "webhooks/reference/history",
        status: Status::Typed,
        examples: &["Examples (1 of 2)", "Examples (2 of 2)"],
    },
    Page {
        path: "webhooks/reference/message_template_components_update",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/message_template_quality_update",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/message_template_status_update",
        status: Status::Typed,
        examples: &["Example (1 of 2)", "Example (2 of 2)"],
    },
    Page {
        path: "webhooks/reference/messages",
        status: Status::Typed,
        examples: &["Incoming messages", "Outgoing messages"],
    },
    Page {
        path: "webhooks/reference/messages/audio",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/button",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/contacts",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/document",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/edit",
        status: Status::Typed,
        examples: &["Sample webhooks"],
    },
    Page {
        path: "webhooks/reference/messages/errors",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/group",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/image",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/interactive",
        status: Status::Typed,
        examples: &["Examples (1 of 2)", "Examples (2 of 2)"],
    },
    Page {
        path: "webhooks/reference/messages/location",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/order",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/reaction",
        status: Status::Typed,
        examples: &["Sample Webhooks (1 of 2)", "Sample Webhooks (2 of 2)"],
    },
    Page {
        path: "webhooks/reference/messages/revoke",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/status",
        status: Status::Typed,
        examples: &[
            "Examples (1 of 3)",
            "Examples (2 of 3)",
            "Examples (3 of 3)",
        ],
    },
    Page {
        path: "webhooks/reference/messages/sticker",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/system",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/text",
        status: Status::Typed,
        examples: &[
            "Text message",
            "Message business button",
            "Click to WhatsApp ad",
        ],
    },
    Page {
        path: "webhooks/reference/messages/unsupported",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/messages/video",
        status: Status::Typed,
        examples: &["Example video message webhook"],
    },
    Page {
        path: "webhooks/reference/messaging-handovers",
        status: Status::Typed,
        examples: &["Example payload (1 of 2)", "Example payload (2 of 2)"],
    },
    Page {
        path: "webhooks/reference/partner_solutions",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/payment_configuration_update",
        status: Status::Typed,
        examples: &["Example payload"],
    },
    Page {
        path: "webhooks/reference/phone_number_name_update",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/phone_number_quality_update",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/pricing",
        status: Status::Unreadable(
            "listed in llms.txt; `.md` and HTML both answer \"Page Not Found\" (2026-09-26)",
        ),
        examples: &[],
    },
    Page {
        path: "webhooks/reference/security",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/smb_app_state_sync",
        status: Status::Typed,
        examples: &["Example"],
    },
    Page {
        path: "webhooks/reference/smb_message_echoes",
        status: Status::Typed,
        examples: &["Text message", "Revoke message", "Edit message"],
    },
    Page {
        path: "webhooks/reference/standby",
        status: Status::Partial(
            "an echo's `message` (the Send API request body), `template` and `flow` (full definitions) stay JSON; the \"Common envelope\" shows an empty `standby` and is no example",
        ),
        examples: &[
            "Example payload (1 of 2)",
            "Text message echo",
            "Template message echo",
            "Interactive flow message echo",
            "Example payload (2 of 2)",
        ],
    },
    Page {
        path: "webhooks/reference/template_category_update",
        status: Status::Typed,
        examples: &["Examples (1 of 2)", "Examples (2 of 2)"],
    },
    Page {
        path: "webhooks/reference/user_preferences",
        status: Status::Typed,
        examples: &["Example"],
    },
];

pub const CASES: &[Case] = &[
    Case {
        fixture: "bsuid/contacts_shared_vcard.json",
        sources: &[(
            Origin::Composed,
            "business-scoped-user-ids",
            "Contacts webhook",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            None,
        )],
    },
    Case {
        fixture: "bsuid/history_thread_context.json",
        sources: &[(
            Origin::Composed,
            "business-scoped-user-ids",
            "History webhooks",
        )],
        events: &[ev(
            "history_synced",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "bsuid/smb_app_state_sync_bsuid.json",
        sources: &[(
            Origin::Composed,
            "business-scoped-user-ids",
            "smb_app_state_sync webhooks",
        )],
        events: &[
            ev(
                "app_state_synced",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "app_state_synced",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "bsuid/smb_message_echoes_bsuid.json",
        sources: &[(
            Origin::Composed,
            "business-scoped-user-ids",
            "smb_message_echoes webhooks",
        )],
        events: &[ev(
            "message_echoed",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            None,
        )],
    },
    Case {
        fixture: "bsuid/status_delivered_bsuid_only.json",
        sources: &[(
            Origin::Verbatim,
            "business-scoped-user-ids",
            "Status messages webhooks (3 of 4)",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            None,
        )],
    },
    Case {
        fixture: "bsuid/status_delivered_parent_bsuid.json",
        sources: &[(
            Origin::Verbatim,
            "business-scoped-user-ids",
            "Status messages webhooks (2 of 4)",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "bsuid/status_failed_no_contacts.json",
        sources: &[(
            Origin::Verbatim,
            "business-scoped-user-ids",
            "Status messages webhooks (4 of 4)",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "bsuid/system_user_changed_user_id.json",
        sources: &[(
            Origin::Composed,
            "business-scoped-user-ids",
            "System messages webhooks",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "bsuid/text_username_no_wa_id.json",
        sources: &[(
            Origin::Verbatim,
            "business-scoped-user-ids",
            "Incoming messages webhooks (2 of 2)",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            None,
        )],
    },
    Case {
        fixture: "bsuid/user_preferences_bsuid.json",
        sources: &[(
            Origin::Composed,
            "business-scoped-user-ids",
            "user_preferences webhooks",
        )],
        events: &[ev(
            "user_preference_changed",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            None,
        )],
    },
    Case {
        fixture: "fields/account_alerts.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_alerts",
            "Example",
        )],
        events: &[ev(
            "account_alert",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_review_update.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_review_update",
            "Example payload",
        )],
        events: &[ev(
            "account_review_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_settings_update.json",
        sources: &[(
            Origin::Composed,
            "calling/call-settings",
            "Settings update webhooks",
        )],
        events: &[ev(
            "account_settings_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_ad_account_linked.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Ad account linked",
        )],
        events: &[ev(
            "account_updated",
            Some("980198427658004"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_auth_intl.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Authentication-international eligibility",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_certification.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Partner-led business verification status",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_coexistence_partner_removed.json",
        sources: &[(
            Origin::Verbatim,
            "embedded-signup/onboarding-business-app-users",
            "Example payload (1 of 4)",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_deleted.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Account deleted",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_disabled.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Disabled update",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_mm_lite_terms.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "MM API for WhatsApp terms of service",
        )],
        events: &[ev(
            "account_updated",
            Some("980198427658004"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_offboarded.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Account offboarded {#account-offboarded}",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_partner_added.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Partner added",
        )],
        events: &[ev(
            "account_updated",
            Some("980198427658004"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_partner_app_installed.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Partner app installed",
        )],
        events: &[ev(
            "account_updated",
            Some("1191624265890717"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_partner_app_uninstalled.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Partner app uninstalled",
        )],
        events: &[ev(
            "account_updated",
            Some("184943124712545"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_partner_led_verification.json",
        sources: &[(
            Origin::Filled,
            "solution-providers/partner-led-business-verification",
            "Example webhook",
        )],
        events: &[ev(
            "account_updated",
            Some("486585971195941"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_partner_removed.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Partner removed",
        )],
        events: &[ev(
            "account_updated",
            Some("980198427658004"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_partner_removed_disconnection.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Partner removed (WhatsApp Business app disconnection) {#partner-removed-disconnection}",
        )],
        events: &[ev(
            "account_updated",
            Some("980198427658004"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_primary_location.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Primary business location set",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_reconnected.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Account reconnected {#account-reconnected}",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_restriction.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Account restriction",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_violation.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Account violation",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/account_update_volume_tier.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/account_update",
            "Pricing tiering update",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/automatic_events_lead.json",
        sources: &[
            (
                Origin::Verbatim,
                "webhooks/reference/automatic_events",
                "Lead gen event",
            ),
            (
                Origin::Verbatim,
                "embedded-signup/automatic-events-api",
                "Lead gen event example",
            ),
        ],
        events: &[ev(
            "automatic_event_detected",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/automatic_events_purchase.json",
        sources: &[
            (
                Origin::Verbatim,
                "webhooks/reference/automatic_events",
                "Purchase event",
            ),
            (
                Origin::Verbatim,
                "embedded-signup/automatic-events-api",
                "Purchase event example",
            ),
        ],
        events: &[ev(
            "automatic_event_detected",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/business_capability_update.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/business_capability_update",
            "Payload example",
        )],
        events: &[ev(
            "business_capability_updated",
            Some("524126980791429"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/business_username_updates.json",
        sources: &[(
            Origin::Composed,
            "business-scoped-user-ids",
            "business_username_updates webhook",
        )],
        events: &[
            ev(
                "business_username_updated",
                Some("102290129340398"),
                None,
                None,
                None,
            ),
            ev(
                "business_username_updated",
                Some("102290129340398"),
                None,
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "fields/calls_connect.json",
        sources: &[(
            Origin::Composed,
            "calling/reference",
            "Call Connect webhook",
        )],
        events: &[ev(
            "call_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16315553602"),
        )],
    },
    Case {
        fixture: "fields/calls_connect_sample.json",
        sources: &[(
            Origin::Composed,
            "calling/reference",
            "Sample call connect webhook",
        )],
        events: &[ev(
            "call_updated",
            Some("112735964992110"),
            Some("105615555715855"),
            Some("US.13491208655302741918"),
            Some("16501230000"),
        )],
    },
    Case {
        fixture: "fields/calls_created.json",
        sources: &[(
            Origin::Composed,
            "calling/reference",
            "Call created webhook",
        )],
        events: &[ev(
            "call_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16315553602"),
        )],
    },
    Case {
        fixture: "fields/calls_recording_available.json",
        sources: &[(
            Origin::Composed,
            "calling/call-recording",
            "Recording-available webhook",
        )],
        events: &[ev(
            "call_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/calls_status.json",
        sources: &[(
            Origin::Composed,
            "calling/business-initiated-calls",
            "Call status webhook",
        )],
        events: &[ev(
            "call_status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("163155536021"),
        )],
    },
    Case {
        fixture: "fields/calls_terminate.json",
        sources: &[(
            Origin::Composed,
            "calling/reference",
            "Call terminate webhook",
        )],
        events: &[
            ev(
                "call_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                Some("US.13491208655302741918"),
                Some("16315553601"),
            ),
            ev(
                "error_reported",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "fields/calls_transcription_available.json",
        sources: &[(
            Origin::Composed,
            "calling/call-transcription",
            "Transcription-available webhook",
        )],
        events: &[ev(
            "call_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/flows_client_error_rate.json",
        sources: &[(
            Origin::Verbatim,
            "flows/guides/flowswebhooks",
            "Possible resolutions (1 of 4)",
        )],
        events: &[ev(
            "flow_updated",
            Some("106181168862417"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/flows_endpoint_availability.json",
        sources: &[(
            Origin::Verbatim,
            "flows/guides/flowswebhooks",
            "Possible resolutions (4 of 4)",
        )],
        events: &[ev(
            "flow_updated",
            Some("106181168862417"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/flows_endpoint_error_rate.json",
        sources: &[(
            Origin::Verbatim,
            "flows/guides/flowswebhooks",
            "Possible resolutions (2 of 4)",
        )],
        events: &[ev(
            "flow_updated",
            Some("106181168862417"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/flows_endpoint_latency.json",
        sources: &[(
            Origin::Verbatim,
            "flows/guides/flowswebhooks",
            "Possible resolutions (3 of 4)",
        )],
        events: &[ev(
            "flow_updated",
            Some("106181168862417"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/flows_status_change.json",
        sources: &[(
            Origin::Verbatim,
            "flows/guides/flowswebhooks",
            "Status change webhook {#statuschange}",
        )],
        events: &[ev(
            "flow_updated",
            Some("644600416743275"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/group_lifecycle_update.json",
        sources: &[(
            Origin::Composed,
            "groups/webhooks",
            "Group create succeed, Group create fail, Delete group succeed",
        )],
        events: &[
            ev(
                "group_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "group_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "group_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "fields/group_participants_update.json",
        sources: &[(
            Origin::Composed,
            "groups/webhooks",
            "participant add, join request, participant remove examples",
        )],
        events: &[
            ev(
                "group_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "group_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "group_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "group_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "fields/group_settings_update.json",
        sources: &[(
            Origin::Composed,
            "groups/webhooks",
            "Group settings update succeed",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/group_status_update.json",
        sources: &[(
            Origin::Composed,
            "groups/webhooks",
            "Group suspended, Group suspension cleared",
        )],
        events: &[
            ev(
                "group_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "group_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "fields/history_declined.json",
        sources: &[(
            Origin::Composed,
            "webhooks/reference/history",
            "history sharing declined (error 2593109)",
        )],
        events: &[ev(
            "history_synced",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/history_media.json",
        sources: &[
            (
                Origin::Verbatim,
                "webhooks/reference/history",
                "Examples (2 of 2)",
            ),
            (
                Origin::Verbatim,
                "embedded-signup/onboarding-business-app-users",
                "Example payload for media message asset {#example-media-asset}",
            ),
        ],
        events: &[ev(
            "history_synced",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/history_threads.json",
        sources: &[
            (
                Origin::Verbatim,
                "webhooks/reference/history",
                "Examples (1 of 2)",
            ),
            (
                Origin::Verbatim,
                "embedded-signup/onboarding-business-app-users",
                "Example payload — chat history sharing approved {#example-history-approved}",
            ),
        ],
        events: &[ev(
            "history_synced",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/message_template_components_update.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/message_template_components_update",
            "Example",
        )],
        events: &[ev(
            "template_components_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/message_template_quality_update.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/message_template_quality_update",
            "Example",
        )],
        events: &[ev(
            "template_quality_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/message_template_status_update_approved.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/message_template_status_update",
            "Example (1 of 2)",
        )],
        events: &[ev(
            "template_status_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/message_template_status_update_rejected.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/message_template_status_update",
            "Example (2 of 2)",
        )],
        events: &[ev(
            "template_status_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/partner_solutions.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/partner_solutions",
            "Example",
        )],
        events: &[ev("partner_solution_updated", None, None, None, None)],
    },
    Case {
        fixture: "fields/payment_configuration_update.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/payment_configuration_update",
            "Example payload",
        )],
        events: &[ev(
            "payment_configuration_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/phone_number_name_update.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/phone_number_name_update",
            "Example",
        )],
        events: &[ev(
            "phone_number_name_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/phone_number_quality_update.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/phone_number_quality_update",
            "Example",
        )],
        events: &[ev(
            "phone_number_quality_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/security.json",
        sources: &[(Origin::Verbatim, "webhooks/reference/security", "Example")],
        events: &[ev(
            "security_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/smb_app_state_sync.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/smb_app_state_sync",
            "Example",
        )],
        events: &[ev(
            "app_state_synced",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/smb_message_echoes_edit.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/smb_message_echoes",
            "Edit message",
        )],
        events: &[ev(
            "message_echoed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/smb_message_echoes_revoke.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/smb_message_echoes",
            "Revoke message",
        )],
        events: &[ev(
            "message_echoed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/smb_message_echoes_text.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/smb_message_echoes",
            "Text message",
        )],
        events: &[ev(
            "message_echoed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/template_category_update_completed.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/template_category_update",
            "Examples (2 of 2)",
        )],
        events: &[ev(
            "template_category_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/template_category_update_impending.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/template_category_update",
            "Examples (1 of 2)",
        )],
        events: &[ev(
            "template_category_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/template_correct_category_detection.json",
        sources: &[(
            Origin::Composed,
            "direct-send/integrity-and-content-guidelines",
            "template_correct_category_detection webhook",
        )],
        events: &[ev(
            "template_category_misuse_detected",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "fields/user_id_update.json",
        sources: &[(
            Origin::Composed,
            "business-scoped-user-ids",
            "user_id_update webhooks",
        )],
        events: &[ev(
            "user_id_changed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "fields/user_preferences.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/user_preferences",
            "Example",
        )],
        events: &[ev(
            "user_preference_changed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/audio.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/audio",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/button.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/button",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/contacts.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/contacts",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/document.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/document",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/edit.json",
        sources: &[
            (
                Origin::Verbatim,
                "webhooks/reference/messages/edit",
                "Sample webhooks",
            ),
            (
                Origin::Verbatim,
                "embedded-signup/onboarding-business-app-users",
                "Example (1 of 2)",
            ),
        ],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/errors.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/errors",
            "Example",
        )],
        events: &[ev(
            "error_reported",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "messages/group_statuses_aggregated.json",
        sources: &[(
            Origin::Composed,
            "groups/webhooks",
            "Multiple participants, single message",
        )],
        events: &[
            ev(
                "status_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "status_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "status_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "messages/group_text.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/group",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/image.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/image",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/interactive_button_reply.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/interactive",
            "Examples (2 of 2)",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/interactive_call_permission_reply.json",
        sources: &[(
            Origin::Composed,
            "calling/user-call-permissions",
            "call_permission_reply webhook",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/interactive_list_reply.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/interactive",
            "Examples (1 of 2)",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/interactive_nfm_reply_address.json",
        sources: &[(
            Origin::Composed,
            "messages/address-messages",
            "address message reply webhook",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("IN.13491208655302741918"),
            Some("919876543210"),
        )],
    },
    Case {
        fixture: "messages/interactive_nfm_reply_flow.json",
        sources: &[(
            Origin::Composed,
            "flows/guides/flowswebhooks",
            "Flow completion (nfm_reply) webhook",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/location.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/location",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/order.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/order",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/overview_text.json",
        sources: &[
            (
                Origin::Verbatim,
                "webhooks/reference/messages",
                "Incoming messages",
            ),
            (
                Origin::Verbatim,
                "webhooks/reference/messages/text",
                "Text message",
            ),
            (
                Origin::Verbatim,
                "webhooks/overview",
                "Webhooks (payload example)",
            ),
        ],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/reaction.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/reaction",
            "Sample Webhooks (1 of 2)",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/reaction_removed.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/reaction",
            "Sample Webhooks (2 of 2)",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/revoke.json",
        sources: &[
            (
                Origin::Verbatim,
                "webhooks/reference/messages/revoke",
                "Example",
            ),
            (
                Origin::Verbatim,
                "embedded-signup/onboarding-business-app-users",
                "Example (2 of 2)",
            ),
        ],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/status_delivered_cbp.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages",
            "Outgoing messages",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "messages/status_failed.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/status",
            "Examples (3 of 3)",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "messages/status_free_customer_service.json",
        sources: &[(
            Origin::Composed,
            "webhooks/reference/messages/status",
            "free customer service status",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/status_sent.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/status",
            "Examples (1 of 3)",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "messages/status_sent_v24.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/status",
            "Examples (2 of 3)",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "messages/sticker.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/sticker",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/system.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/system",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "messages/text.json",
        sources: &[
            (
                Origin::Verbatim,
                "webhooks/reference/messages",
                "Incoming messages",
            ),
            (
                Origin::Verbatim,
                "webhooks/reference/messages/text",
                "Text message",
            ),
            (
                Origin::Verbatim,
                "webhooks/overview",
                "Webhooks (payload example)",
            ),
        ],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/text_ctwa_referral.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/text",
            "Click to WhatsApp ad",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/text_message_business_button.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/text",
            "Message business button",
        )],
        events: &[ev(
            "message_received",
            Some("419561257915477"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/unsupported.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/unsupported",
            "Example",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "messages/video.json",
        sources: &[(
            Origin::Verbatim,
            "webhooks/reference/messages/video",
            "Example video message webhook",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__business_initiated_calls_status_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Business-initiated calls status webhooks",
        )],
        events: &[ev(
            "call_status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__business_initiated_connected_calls_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Business-initiated connected calls webhooks",
        )],
        events: &[ev(
            "call_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__business_initiated_terminated_calls_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Business-initiated terminated calls webhooks",
        )],
        events: &[ev(
            "call_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__business_username_updates_webhook.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "business_username_updates webhook",
        )],
        events: &[ev(
            "business_username_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__call_permission_request_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Call permission request webhooks",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__contacts_webhook.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Contacts webhook",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__edit_messages_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Edit messages webhooks (1 of 2)",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__edit_messages_webhooks_2.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Edit messages webhooks (2 of 2)",
        )],
        events: &[ev(
            "message_echoed",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__group_participants_update.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "group_participants_update webhooks",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__group_participants_update_invite_link_join.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "group_participants_update webhooks",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__group_participants_update_join_request_created.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "group_participants_update webhooks",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__group_participants_update_join_request_revoked.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "group_participants_update webhooks",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__group_participants_update_participant_leaves.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "group_participants_update webhooks",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__history_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "History webhooks (1 of 2)",
        )],
        events: &[ev(
            "history_synced",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__history_webhooks_2.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "History webhooks (2 of 2)",
        )],
        events: &[ev(
            "history_synced",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__history_webhooks_2_message_echoes.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "History webhooks (2 of 2)",
        )],
        events: &[ev(
            "history_synced",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__incoming_messages_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Incoming messages webhooks (1 of 2)",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__revoke_messages_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Revoke messages webhooks (1 of 2)",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__revoke_messages_webhooks_2.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Revoke messages webhooks (2 of 2)",
        )],
        events: &[ev(
            "message_echoed",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__smb_app_state_sync_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "smb_app_state_sync webhooks",
        )],
        events: &[ev(
            "app_state_synced",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__smb_message_echoes_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "smb_message_echoes webhooks",
        )],
        events: &[ev(
            "message_echoed",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__status_messages_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Status messages webhooks (1 of 4)",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__status_messages_webhooks_for_groups.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "Status messages webhooks for groups",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__system_messages_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "System messages webhooks",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__user_id_update_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "user_id_update webhooks",
        )],
        events: &[ev(
            "user_id_changed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__user_initiated_connected_calls_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "User-initiated connected calls webhooks",
        )],
        events: &[ev(
            "call_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__user_initiated_terminated_calls_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "User-initiated terminated calls webhooks",
        )],
        events: &[ev(
            "call_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/business-scoped-user-ids__user_preferences_webhooks.json",
        sources: &[(
            Origin::Filled,
            "business-scoped-user-ids",
            "user_preferences webhooks",
        )],
        events: &[ev(
            "user_preference_changed",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/calling.business-initiated-calls__call_connect_webhook.json",
        sources: &[(
            Origin::Filled,
            "calling/business-initiated-calls",
            "Call connect webhook",
        )],
        events: &[ev(
            "call_updated",
            Some("366634483210360"),
            Some("436666719526789"),
            Some("US.13491208655302741918"),
            Some("16315553602"),
        )],
    },
    Case {
        fixture: "pages/calling.business-initiated-calls__call_status_webhook.json",
        sources: &[(
            Origin::Filled,
            "calling/business-initiated-calls",
            "Call status webhook",
        )],
        events: &[ev(
            "call_status_updated",
            Some("366634483210360"),
            Some("436666719526789"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/calling.business-initiated-calls__call_terminate_webhook.json",
        sources: &[(
            Origin::Filled,
            "calling/business-initiated-calls",
            "Call terminate webhook",
        )],
        events: &[
            ev(
                "call_updated",
                Some("366634483210360"),
                Some("436666719526789"),
                Some("US.13491208655302741918"),
                Some("12185552828"),
            ),
            ev(
                "error_reported",
                Some("366634483210360"),
                Some("436666719526789"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "pages/calling.business-initiated-calls__part_3_call_connection.json",
        sources: &[(
            Origin::Filled,
            "calling/business-initiated-calls",
            "Part 3: You establish the call connection using webhook signaling (1 of 2)",
        )],
        events: &[ev(
            "call_updated",
            Some("366634483210360"),
            Some("436666719526789"),
            Some("US.13491208655302741918"),
            Some("12185552828"),
        )],
    },
    Case {
        fixture: "pages/calling.business-initiated-calls__part_3_call_connection_2.json",
        sources: &[(
            Origin::Filled,
            "calling/business-initiated-calls",
            "Part 3: You establish the call connection using webhook signaling (2 of 2)",
        )],
        events: &[ev(
            "call_status_updated",
            Some("366634483210360"),
            Some("436666719526789"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/calling.business-initiated-calls__part_4_terminate_call.json",
        sources: &[(
            Origin::Filled,
            "calling/business-initiated-calls",
            "Part 4: Your business or the WhatsApp user terminates the call",
        )],
        events: &[ev(
            "call_updated",
            Some("366634483210360"),
            Some("436666719526789"),
            Some("US.13491208655302741918"),
            Some("12185552828"),
        )],
    },
    Case {
        fixture: "pages/calling.call-recording__recording_available_webhook.json",
        sources: &[(
            Origin::Filled,
            "calling/call-recording",
            "Recording-available webhook",
        )],
        events: &[ev(
            "call_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/calling.call-settings__early_warning_webhook.json",
        sources: &[(Origin::Filled, "calling/call-settings", "Webhook (1 of 3)")],
        events: &[ev("account_updated", Some("0"), None, None, None)],
    },
    Case {
        fixture: "pages/calling.call-settings__enforcement_webhook.json",
        sources: &[(
            Origin::Filled,
            "calling/call-settings",
            "Enforcement webhook",
        )],
        events: &[ev("account_updated", Some("0"), None, None, None)],
    },
    Case {
        fixture: "pages/calling.call-settings__pause_in_calling_webhook.json",
        sources: &[(Origin::Filled, "calling/call-settings", "Webhook (2 of 3)")],
        events: &[ev("account_updated", Some("0"), None, None, None)],
    },
    Case {
        fixture: "pages/calling.call-settings__pause_in_user_initiated_calling_webhook.json",
        sources: &[(Origin::Filled, "calling/call-settings", "Webhook (3 of 3)")],
        events: &[ev("account_updated", Some("0"), None, None, None)],
    },
    Case {
        fixture: "pages/calling.call-settings__settings_update_webhook_payload.json",
        sources: &[(
            Origin::Filled,
            "calling/call-settings",
            "Webhook payload (2 of 2)",
        )],
        events: &[ev(
            "account_settings_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/calling.call-settings__voicemail_webhook_payload.json",
        sources: &[(
            Origin::Filled,
            "calling/call-settings",
            "Webhook payload (1 of 2)",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            Some("US.13491208655302741918"),
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/calling.call-settings__warning_webhook.json",
        sources: &[(Origin::Filled, "calling/call-settings", "Warning webhook")],
        events: &[ev("account_updated", Some("0"), None, None, None)],
    },
    Case {
        fixture: "pages/calling.call-transcription__transcription_available_webhook.json",
        sources: &[(
            Origin::Filled,
            "calling/call-transcription",
            "Transcription-available webhook",
        )],
        events: &[ev(
            "call_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/calling.reference__call_connect_webhook.json",
        sources: &[(Origin::Filled, "calling/reference", "Call Connect webhook")],
        events: &[ev(
            "call_updated",
            Some("112735964992110"),
            Some("105615555715855"),
            Some("US.13491208655302741918"),
            Some("16315553602"),
        )],
    },
    Case {
        fixture: "pages/calling.reference__call_created_webhook.json",
        sources: &[(Origin::Filled, "calling/reference", "Call created webhook")],
        events: &[ev(
            "call_updated",
            Some("112735964992110"),
            Some("105615555715855"),
            Some("US.13491208655302741918"),
            Some("16315553602"),
        )],
    },
    Case {
        fixture: "pages/calling.reference__call_status_webhook.json",
        sources: &[(Origin::Filled, "calling/reference", "Call status webhook")],
        events: &[ev(
            "call_status_updated",
            Some("112735964992110"),
            Some("105615555715855"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/calling.reference__call_terminate_webhook.json",
        sources: &[(
            Origin::Filled,
            "calling/reference",
            "Call terminate webhook",
        )],
        events: &[
            ev(
                "call_updated",
                Some("112735964992110"),
                Some("105615555715855"),
                Some("US.13491208655302741918"),
                Some("16315553601"),
            ),
            ev(
                "error_reported",
                Some("112735964992110"),
                Some("105615555715855"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "pages/calling.reference__sample_call_connect_webhook.json",
        sources: &[(Origin::Filled, "calling/reference", "Call connect webhook")],
        events: &[ev(
            "call_updated",
            Some("112735964992110"),
            Some("105615555715855"),
            Some("US.13491208655302741918"),
            Some("16501230000"),
        )],
    },
    Case {
        fixture: "pages/calling.user-initiated-calls__call_connect_webhook.json",
        sources: &[(
            Origin::Filled,
            "calling/user-initiated-calls",
            "Call Connect webhook",
        )],
        events: &[ev(
            "call_updated",
            Some("366634483210360"),
            Some("436666719526789"),
            Some("US.13491208655302741918"),
            Some("16315553602"),
        )],
    },
    Case {
        fixture: "pages/calling.user-initiated-calls__call_terminate_webhook.json",
        sources: &[(
            Origin::Filled,
            "calling/user-initiated-calls",
            "Call Terminate webhook",
        )],
        events: &[
            ev(
                "call_updated",
                Some("366634483210360"),
                Some("436666719526789"),
                Some("US.13491208655302741918"),
                Some("16315553602"),
            ),
            ev(
                "error_reported",
                Some("366634483210360"),
                Some("436666719526789"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "pages/calling.user-initiated-calls__part_1_user_calls_business.json",
        sources: &[(
            Origin::Filled,
            "calling/user-initiated-calls",
            "Part 1: A WhatsApp user calls your business from their client app",
        )],
        events: &[ev(
            "call_updated",
            Some("366634483210360"),
            Some("436666719526789"),
            Some("US.13491208655302741918"),
            Some("16315553602"),
        )],
    },
    Case {
        fixture: "pages/calling.user-initiated-calls__part_4_terminate_call.json",
        sources: &[(
            Origin::Filled,
            "calling/user-initiated-calls",
            "Part 4: Your business or the WhatsApp user terminates the call",
        )],
        events: &[ev(
            "call_updated",
            Some("366634483210360"),
            Some("436666719526789"),
            Some("US.13491208655302741918"),
            Some("16315553602"),
        )],
    },
    Case {
        fixture: "pages/conversation-routing.conversation-context__control_passed.json",
        sources: &[(
            Origin::Filled,
            "conversation-routing/conversation-context",
            "control_passed handover webhook",
        )],
        events: &[ev(
            "thread_control_changed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/conversation-routing.conversation-context__incoming_message.json",
        sources: &[(
            Origin::Filled,
            "conversation-routing/conversation-context",
            "Incoming message webhook",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/conversation-routing.thread-control__control_passed.json",
        sources: &[(
            Origin::Filled,
            "conversation-routing/thread-control",
            "control_passed",
        )],
        events: &[ev(
            "thread_control_changed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/conversation-routing.thread-control__control_taken.json",
        sources: &[(
            Origin::Filled,
            "conversation-routing/thread-control",
            "control_taken",
        )],
        events: &[ev(
            "thread_control_changed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/direct-send.integrity-and-content-guidelines__active_restriction.json",
        sources: &[(
            Origin::Filled,
            "direct-send/integrity-and-content-guidelines",
            "Active restriction (rate-limiting, 7-day, 30-day, or permanent)",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/direct-send.integrity-and-content-guidelines__category_misuse.json",
        sources: &[(
            Origin::Filled,
            "direct-send/integrity-and-content-guidelines",
            "Webhook for category misuse",
        )],
        events: &[ev(
            "template_category_misuse_detected",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/direct-send.integrity-and-content-guidelines__restriction_lifted.json",
        sources: &[(
            Origin::Filled,
            "direct-send/integrity-and-content-guidelines",
            "Restriction lifted",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/direct-send.integrity-and-content-guidelines__template_pausing.json",
        sources: &[(
            Origin::Verbatim,
            "direct-send/integrity-and-content-guidelines",
            "Webhook for template pausing",
        )],
        events: &[ev(
            "template_status_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/direct-send.integrity-and-content-guidelines__warning.json",
        sources: &[(
            Origin::Filled,
            "direct-send/integrity-and-content-guidelines",
            "Warning (no restriction)",
        )],
        events: &[ev(
            "account_updated",
            Some("102290129340398"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/embedded-signup.onboarding-business-app-users__account_offboarded.json",
        sources: &[(
            Origin::Verbatim,
            "embedded-signup/onboarding-business-app-users",
            "Example payload (2 of 4)",
        )],
        events: &[ev(
            "account_updated",
            Some("862475293675413"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/embedded-signup.onboarding-business-app-users__account_reconnected.json",
        sources: &[(
            Origin::Verbatim,
            "embedded-signup/onboarding-business-app-users",
            "Example payload (3 of 4)",
        )],
        events: &[ev(
            "account_updated",
            Some("862475293675413"),
            None,
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/embedded-signup.onboarding-business-app-users__smb_message_echoes.json",
        sources: &[(
            Origin::Verbatim,
            "embedded-signup/onboarding-business-app-users",
            "Example payload (4 of 4)",
        )],
        events: &[ev(
            "message_echoed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.groups-messaging__group_message_failed.json",
        sources: &[(
            Origin::Filled,
            "groups/groups-messaging",
            "Group message failed example",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.groups-messaging__group_message_sent.json",
        sources: &[(
            Origin::Filled,
            "groups/groups-messaging",
            "Group message sent example",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.groups-messaging__receive_group_message.json",
        sources: &[(
            Origin::Filled,
            "groups/groups-messaging",
            "Receive group message webhook sample",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/groups.groups-messaging__receive_unsupported_group_message.json",
        sources: &[(
            Origin::Filled,
            "groups/groups-messaging",
            "Receive unsupported group message webhook sample",
        )],
        events: &[ev(
            "message_received",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__delete_group_fails.json",
        sources: &[(Origin::Filled, "groups/webhooks", "Delete group fails")],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__delete_group_succeed.json",
        sources: &[(Origin::Filled, "groups/webhooks", "Delete group succeed")],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_create_fail.json",
        sources: &[(Origin::Filled, "groups/webhooks", "Group create fail")],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_create_succeed.json",
        sources: &[(Origin::Filled, "groups/webhooks", "Group create succeed")],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_message_delivered.json",
        sources: &[(Origin::Filled, "groups/webhooks", "Group message delivered")],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_message_read_with_pricing.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Group message read (_With pricing_)",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_message_read_without_pricing.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Group message read (_Without pricing_)",
        )],
        events: &[ev(
            "status_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_participant_leaves.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Group participant leaves webhook",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_participant_remove_fails.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Group participant remove fails",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_participant_remove_partially_fails.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Group participant remove with participants partially fails",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_participant_remove_succeed.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Group participant remove succeed",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_settings_update_partial_fail.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Group settings update partial fail",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_settings_update_succeed.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Group settings update succeed",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_settings_update_total_fail.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Group settings update total fail",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_suspended.json",
        sources: &[(Origin::Filled, "groups/webhooks", "Group suspended")],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__group_suspension_cleared.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Group suspension cleared",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__join_request_approved.json",
        sources: &[(Origin::Filled, "groups/webhooks", "Join request approved")],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__multiple_messages_single_participant.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Multiple messages, single participant",
        )],
        events: &[
            ev(
                "status_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "status_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "pages/groups.webhooks__multiple_participants_single_message.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "Multiple participants, single message",
        )],
        events: &[
            ev(
                "status_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
            ev(
                "status_updated",
                Some("102290129340398"),
                Some("106540352242922"),
                None,
                None,
            ),
        ],
    },
    Case {
        fixture: "pages/groups.webhooks__user_accepts_or_cancels_join_request.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "User accepts or cancels join request",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/groups.webhooks__user_joined_group_using_invite_link_succeed.json",
        sources: &[(
            Origin::Filled,
            "groups/webhooks",
            "User joined group using invite link succeed",
        )],
        events: &[ev(
            "group_updated",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/webhooks.reference.messaging-handovers__control_passed.json",
        sources: &[(
            Origin::Filled,
            "webhooks/reference/messaging-handovers",
            "Example payload (1 of 2)",
        )],
        events: &[ev(
            "thread_control_changed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/webhooks.reference.messaging-handovers__control_taken.json",
        sources: &[(
            Origin::Filled,
            "webhooks/reference/messaging-handovers",
            "Example payload (2 of 2)",
        )],
        events: &[ev(
            "thread_control_changed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/webhooks.reference.standby__inbound_message.json",
        sources: &[(
            Origin::Filled,
            "webhooks/reference/standby",
            "Example payload (1 of 2)",
        )],
        events: &[ev(
            "standby_observed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            Some("16505551234"),
        )],
    },
    Case {
        fixture: "pages/webhooks.reference.standby__interactive_flow_message_echo.json",
        sources: &[(
            Origin::Filled,
            "webhooks/reference/standby",
            "Interactive flow message echo",
        )],
        events: &[ev(
            "standby_observed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/webhooks.reference.standby__status_receipt.json",
        sources: &[(
            Origin::Filled,
            "webhooks/reference/standby",
            "Example payload (2 of 2)",
        )],
        events: &[ev(
            "standby_observed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/webhooks.reference.standby__template_message_echo.json",
        sources: &[(
            Origin::Filled,
            "webhooks/reference/standby",
            "Template message echo",
        )],
        events: &[ev(
            "standby_observed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
    Case {
        fixture: "pages/webhooks.reference.standby__text_message_echo.json",
        sources: &[(
            Origin::Filled,
            "webhooks/reference/standby",
            "Text message echo",
        )],
        events: &[ev(
            "standby_observed",
            Some("102290129340398"),
            Some("106540352242922"),
            None,
            None,
        )],
    },
];
