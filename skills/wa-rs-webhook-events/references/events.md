# `WebhookEvent` reference

> **Verified against wa-rs b3d2dcad64bc5f0dd9374dc84a387ec978707ab0 (2026-09-25).** Source: `crates/wa-webhooks/src/event.rs`,
> `crates/wa-webhooks/src/fields/*`. The enum is `#[non_exhaustive]`.

Payload types live in `wa_rs::webhooks::fields` (flat re-exports of every
field module). Payloads are boxed.

## Variants

| Variant (`kind()`) | Webhook field | Fields besides `waba_id` |
| --- | --- | --- |
| `MessageReceived` (`message_received`) | `messages` | `phone_number_id`, `display_phone_number`, `contact: Option<Contact>`, `message: Box<InboundMessage>` |
| `StatusUpdated` (`status_updated`) | `messages` | `phone_number_id`, `display_phone_number`, `contact`, `status: Box<Status>` |
| `ErrorReported` (`error_reported`) | `messages`, `calls` | `field`, `phone_number_id`, `display_phone_number`, `error: Box<GraphApiError>` |
| `MessageEchoed` (`message_echoed`) | `smb_message_echoes` | `phone_number_id`, `display_phone_number`, `contact`, `echo: Box<MessageEcho>` |
| `HistorySynced` (`history_synced`) | `history` | `phone_number_id`, `display_phone_number`, `history: Box<HistoryValue>` |
| `AppStateSynced` (`app_state_synced`) | `smb_app_state_sync` | `phone_number_id`, `display_phone_number`, `item: Box<StateSyncItem>` |
| `CallUpdated` (`call_updated`) | `calls` | `phone_number_id`, `display_phone_number`, `contact`, `call: Box<Call>` |
| `CallStatusUpdated` (`call_status_updated`) | `calls` | `phone_number_id`, `display_phone_number`, `contact`, `status: Box<CallStatus>` |
| `UserPreferenceChanged` (`user_preference_changed`) | `user_preferences` | `phone_number_id`, `display_phone_number`, `contact`, `preference: Box<UserPreference>` |
| `UserIdChanged` (`user_id_changed`) | `user_id_update` | `phone_number_id`, `display_phone_number`, `contact`, `update: Box<UserIdUpdate>` |
| `AutomaticEventDetected` (`automatic_event_detected`) | `automatic_events` | `phone_number_id`, `display_phone_number`, `detected: Box<AutomaticEvent>` |
| `GroupUpdated` (`group_updated`) | `group_lifecycle_update`, `group_participants_update`, `group_settings_update`, `group_status_update` | `phone_number_id`, `display_phone_number`, `field`, `update: Box<GroupUpdate>` |
| `FlowUpdated` | `flows` | `time`, `update: Box<FlowsValue>` |
| `AccountAlert` | `account_alerts` | `time`, `alert` |
| `AccountReviewUpdated` | `account_review_update` | `time`, `update` |
| `AccountUpdated` | `account_update` | `waba_id` is an **`Option`**: `waba_info.waba_id` for updates with a `waba_info` (Meta's PARTNER_* events, whose entry id is a business portfolio), the entry id otherwise; `entry_id` (verbatim), `time`, `update`. An event you stored before `entry_id` existed reads back the same way |
| `AccountSettingsUpdated` | `account_settings_update` | `time`, `update` |
| `BusinessCapabilityUpdated` | `business_capability_update` | `time`, `update` |
| `BusinessUsernameUpdated` | `business_username_updates` | `time`, `update` |
| `PartnerSolutionUpdated` | `partner_solutions` | **`business_id`** (not a WABA), `time`, `update` |
| `PaymentConfigurationUpdated` | `payment_configuration_update` | `time`, `update` |
| `PhoneNumberNameUpdated` | `phone_number_name_update` | `time`, `update` |
| `PhoneNumberQualityUpdated` | `phone_number_quality_update` | `time`, `update` |
| `SecurityUpdated` | `security` | `time`, `update` |
| `TemplateComponentsUpdated` | `message_template_components_update` | `time`, `update` |
| `TemplateQualityUpdated` | `message_template_quality_update` | `time`, `update` |
| `TemplateStatusUpdated` | `message_template_status_update` | `time`, `update: Box<TemplateStatusUpdateValue>` |
| `TemplateCategoryUpdated` | `template_category_update` | `time`, `update` |
| `TemplateCategoryMisuseDetected` | `template_correct_category_detection` | `time`, `update` |
| `Unknown` | any other, or a known field whose value did not parse | `waba_id` (the entry id; for an `account_update` that did not parse, its raw `waba_info.waba_id` when that is a non-blank string, since the entry id of such updates is a business portfolio), `field`, `time`, `raw: Value`, `parse_error: Option<String>` |
| `Unparsed` | a signed body that is not a webhook envelope | `raw: Value`, `error: String` (no `waba_id`) |

Undocumented or unavailable at 2026-09-24, so they arrive as `Unknown`:
messaging handovers / standby, `message_echoes`, `consumer_profile`.

## Helpers on `WebhookEvent`

| Method | Returns |
| --- | --- |
| `kind()` | the snake-case tag, stable, for logs and metrics |
| `waba_id()` | `Option<&WabaId>`; `None` for `PartnerSolutionUpdated`, `Unparsed`, and an `AccountUpdated` whose `waba_info` names no WABA |
| `phone_number_id()` | `Option<&PhoneNumberId>` when the field has one (not for fields that only carry a display number) |
| `contact()` | `Option<&Contact>` |
| `dedup_key()` | what `DedupGuard` keys on; `None` for `ErrorReported` and `Unparsed` |

## `Contact` (the WhatsApp user)

`wa_id: Option<WaId>` (phone number, digits only, **may be absent**),
`user_id: Option<UserId>` (BSUID, present in every messages webhook since
April 2026), `parent_user_id`, `identity_key_hash`, `profile`, and
`name()` / `username()`. Key customers by `user_id`; to send to `wa_id`,
prepend `+`.

## `InboundMessage`

`id: MessageId`, `timestamp`, `from: Option<WaId>`,
`from_user_id: Option<UserId>`, `group_id`, `context` (the message it
replies to, forwarded flags, referred product), `referral`
(click-to-WhatsApp ads), `errors`, and `content: MessageContent`:

`Text`, `Image`/`Audio`/`Video`/`Document`/`Sticker` (`MediaContent`: `id`,
`mime_type`, `sha256`, `caption`, `filename`, …), `Location`, `Contacts`,
`Interactive` (`InteractiveReply::ButtonReply`, `ListReply`, `NfmReply` —
a Flow response, read with `.response::<T>()` — and
`CallPermissionReply`), `Button` (template
quick-reply tap: `payload`, `text`), `Order` (cart: `catalog_id`,
`product_items`), `Reaction`, `System`, `Unsupported`, `Edit`, `Revoke`,
`MediaPlaceholder`, plus `Invalid { message_type, error, raw }` and
`Unknown { message_type, raw }`. `message.message_type()` gives the wire
`type`.

Download inbound media with `client.media(pnid).download(&media_id)` and
verify it (`wa-rs-media`).

## `Status` (your outbound messages)

`id` (the `wamid` your send returned), `status: MessageStatus` (`Sent`,
`Delivered`, `Read`, `Played`, `Failed`, `Other(String)`), `timestamp`,
`recipient_id`, `recipient_user_id`, group participant fields,
`biz_opaque_callback_data` (what you set with
`OutboundMessage::callback_data`), `conversation`, `pricing`
(`effective_pricing()`), `errors: Vec<GraphApiError>` (on `failed`).
Statuses can arrive out of order; `DeliveryStatus::supersedes` in
`wa_rs::core::store` is the "never move backwards" rule the inbox uses.
