---
name: meta-whatsapp-rs-webhook-events
description: "Understanding and handling meta-whatsapp-rs WebhookEvent values - inbound customer messages (text, media, button and list replies, orders, reactions) keyed by BSUID, delivery statuses with callback data, pricing and errors (131049, 131050 arrive here), user preference opt-outs, BSUID changes, template and account updates, and forward compatibility (Unknown fields, Invalid and Unknown message types, open enums, the required wildcard arm). Load when writing the code that reacts to WhatsApp webhooks - routing messages, tracking delivery, recording opt-outs, alerting on account or template changes."
---

# meta-whatsapp-rs-webhook-events

> **Verified against meta-whatsapp-rs 6d04f3da9c504cffac32f7dbe05869adcaf1957e (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/events.rs](examples/events.rs), compiled and
tested by meta-whatsapp-rs's own gate with Meta-shaped payloads. Every variant and
payload type: [references/events.md](references/events.md).

## When to use

Anything that consumes the events the endpoint (`meta-whatsapp-rs-webhook-endpoint`)
hands to your sink. Types: `meta_whatsapp_rs::webhooks::WebhookEvent` and the
payloads in `meta_whatsapp_rs::webhooks::fields`.

## Route each event

A customer message (`MessageReceived`): key the customer, then match
`message.content` (`Inbound::Text(text)`, `Inbound::Interactive(..)`, …,
with a `_` arm for the types Meta adds):

```rust
fn customer(message: &meta_whatsapp_rs::webhooks::fields::InboundMessage) -> Option<String> {
    message
        .from_user_id
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| message.from.as_ref().map(ToString::to_string))
}
```

Your own messages' statuses, and identity changes:

```rust
WebhookEvent::StatusUpdated { status, .. } => match status.status {
    MessageStatus::Delivered | MessageStatus::Read => Action::Delivered {
        id: status.id.clone(),
        callback: status.biz_opaque_callback_data.clone(), // your OutboundMessage::callback_data
    },
    MessageStatus::Failed => Action::Failed {
        id: status.id.clone(),
        kinds: status
            .errors
            .iter()
            .map(meta_whatsapp_rs::GraphApiError::kind)
            .collect(), // 131049/131050 arrive here
    },
    _ => Action::Ignore, // sent, played, and values Meta adds (Other)
},
WebhookEvent::UserIdChanged { update, .. } => Action::CustomerRenamed {
    previous: update.user_id.previous.clone(),
    current: update.user_id.current.clone(),
},
```

`WebhookEvent` is `#[non_exhaustive]`: keep a `_` arm. Serialized, it is
tagged `"event"` with the same snake-case name as `event.kind()`.

## The events that matter

| Event | Use |
| --- | --- |
| `MessageReceived` | a customer wrote; opens the 24-hour window; `message.content` is `Text`, `Image`/`Document`/… (`MediaContent`), `Interactive` (button/list/Flow replies), `Button` (template quick reply), `Order`, `Location`, `Reaction`, … |
| `StatusUpdated` | `sent`/`delivered`/`read`/`failed` of **your** messages, keyed by the wamid `send` returned; `errors` on failures; `pricing` |
| `UserPreferenceChanged` | marketing stop/resume (`meta-whatsapp-rs-marketing`) |
| `UserIdChanged` | a customer's BSUID changed: re-key what you stored |
| `TemplateStatusUpdated`, `TemplateQualityUpdated`, `TemplateCategoryUpdated` | template review and health (`meta-whatsapp-rs-templates`) |
| `AccountUpdated`, `PhoneNumberQualityUpdated`, `PhoneNumberNameUpdated`, `AccountAlert` | account restrictions, limits, names |
| `MessageEchoed`, `HistorySynced`, `AppStateSynced` | coexistence (WhatsApp Business app) |
| `ErrorReported` | app- or system-level errors |
| `Unknown` | a field or shape this meta-whatsapp-rs version does not type: log `field` |
| `Unparsed` | a signed body that is not a webhook envelope: alert |

Helpers: `event.phone_number_id()` (route to the merchant),
`event.waba_id()`, `event.contact()`, `event.kind()` (metrics).

## Identity: key customers by BSUID

Since 2026 every messages webhook carries `user_id` (a BSUID such as
`US.13491208655302741918`); `wa_id` (the phone number, digits only) may be
absent. Key by `message.from_user_id` or `contact.user_id`, else `from`.
To answer by phone, prepend `+` to the `wa_id`.

## Pitfalls

- Statuses arrive out of order and more than once: never move a message
  backwards (`DeliveryStatus::supersedes` is the rule the inbox uses) and
  make every handler idempotent (by message id or callback data).
- Opt-outs (131050) and per-user limits (131049) often arrive as a
  `failed` status, not as a send error: check both.
- **Forward compatibility**: unknown fields → `Unknown`; unknown message
  types → `MessageContent::Unknown`, bad shapes → `MessageContent::Invalid`;
  enum values → `Other(String)`. Handle them; never fail on them (Meta
  retries a failed batch for 7 days, then drops it). If you mirror payloads
  into your own types, never use `deny_unknown_fields`.
- Name clashes: `webhooks::fields::MessageContent` (inbound) vs
  `client::messages::MessageContent` (outbound), `MessageStatus`, and
  `TemplateCategory` exist on both sides: alias on import (`as Inbound`).
- **One quality scale, two types**, kept apart on purpose (the owner's
  decision, 2026-09-25): `TemplateQualityUpdated` carries a
  `TemplateQualityScore`, the client's templates and phone numbers a
  `QualityRating`. Both have `Green`, `Yellow`, `Red` and `Unknown` (not
  rated yet). Only `QualityRating` has `NotApplicable` (`NA`, in the phone
  number examples): the webhook documents none, so `NA` there is
  `Other("NA")`. `QualityRating` matches case-insensitively (`green` is
  `Green`); `TemplateQualityScore` exactly (`green` is `Other("green")`).
  Convert through the wire value, never by variant name:

```rust
pub fn quality(score: &TemplateQualityScore) -> QualityRating {
    match score.as_str().parse::<QualityRating>() {
        Ok(rating) => rating,
        Err(never) => match never {},
    }
}
```

- Media ids in webhooks live 7 days: download what you need
  (`meta-whatsapp-rs-media`).
- Customer data: never log `WebhookEvent`'s `Debug` (names, numbers, text).

## What meta-whatsapp-rs does not do

- Messaging handovers, `message_echoes` and `consumer_profile` have no
  documented payload: they arrive as `Unknown`.
- It does not merge conversations when a BSUID changes. The inbox records
  `MessageEchoed` and `HistorySynced` (`meta-whatsapp-rs-cms-inbox`), but not the
  contacts of `AppStateSynced`. ~~Nor does it record coexistence echoes
  and history in the inbox~~: true until a3582b8 (2026-09-24).

## Related skills

`meta-whatsapp-rs-webhook-endpoint`, `meta-whatsapp-rs-live-updates` (sinks), `meta-whatsapp-rs-cms-inbox`,
`meta-whatsapp-rs-marketing` (opt-outs), `meta-whatsapp-rs-errors` (error kinds),
`meta-whatsapp-rs-interactive-messages` (replies to buttons and lists).
