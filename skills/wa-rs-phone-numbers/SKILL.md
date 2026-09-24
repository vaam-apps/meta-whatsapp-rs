---
name: wa-rs-phone-numbers
description: "Managing WhatsApp business phone numbers and their WABA with wa-rs - registering a number for Cloud API with its two-step verification PIN (TwoStepPin, the 10-per-72-hours limit), changing the PIN and the display name, the business profile (about, email, websites), conversational components (ice breakers, commands), subscribing the app to a WABA's webhooks and per-number callback overrides, number health, and the coexistence data sync. Load when registering or configuring a number, editing the WhatsApp business profile, checking a number's quality, or making sure webhooks arrive for a WABA."
---

# wa-rs-phone-numbers

> **Verified against wa-rs 3a3db05aa425c1737d8bb9239206036dbc81969f (2026-09-24).** On another revision, trust the code over this page.

Reference code: [examples/numbers.rs](examples/numbers.rs), compiled and
tested by wa-rs's own gate.

## When to use

Everything about one business number (`client.phone_number(pnid)`), its
profile (`client.business_profile(pnid)`) and its WABA
(`client.waba(waba_id)`). For a merchant's number, use their client
(`with_token`, `wa-rs-token-vault`).

## Register a number

```rust
let pin = TwoStepPin::new(pin_typed_by_the_merchant)?; // exactly 6 digits; never log it
let number = client.phone_number(phone_number_id);
number.register(&pin, None).await // 10 (de)registrations per 72 h: never in a retry loop
```

The PIN becomes the number's two-step verification PIN, or must match the
one it has (wrong PIN: `ErrorKind::TwoStepVerification`, 133005). The
second argument is an optional `DataLocalizationRegion` (local storage;
changing it means deregistering). A number added by Embedded Signup must
be registered within 14 days. Before registration, a number you added
yourself is verified with `request_code(CodeMethod::Sms, "en_US")` and
`verify_code(&VerificationCode::new(code)?)`. `set_two_step_pin(&pin)`
changes the PIN; there is no API to turn two-step verification off.

## Webhooks for a WABA

```rust
let waba = client.waba(waba_id);
let apps = waba.subscribed_apps().await?;
if !apps
    .data
    .iter()
    .any(|a| a.whatsapp_business_api_data.id == app_id)
{
    waba.subscribe_app(None).await?; // Some(&CallbackOverride) sends them elsewhere
}
```

Embedded Signup subscribes merchants' WABAs itself. A callback override
is per WABA (`subscribe_app(Some(&callback))`) or per number
(`set_webhook_override`, `clear_webhook_override`); precedence is number,
then WABA, then the app. Only some fields follow an override (`messages`,
calls, coexistence, groups…): template and account webhooks always go to
the app's callback, so that endpoint must exist anyway.

## Profile and first screen

```rust
let update = ProfileUpdate {
    about: Some("Linen and leather, made in Berlin".into()),
    email: Some("hello@shop.example".into()),
    websites: Some(vec!["https://shop.example".into()]),
    ..ProfileUpdate::default() // `None` fields are left as they are
};
```

`business_profile(pnid).update(&update)` sends only the set fields
(address ≤ 256 characters; `Vertical::Undefined` and `NotABiz` cannot be
set); `get(&[ProfileField::About, …])` reads it back. A profile picture is
a Resumable Upload handle (`profile_picture_handle`, `wa-rs-media`).
Conversational components:

```rust
let components = ConversationalAutomationConfig::new()
    .prompts(["Where is my order?", "Opening hours"]) // at most 4 ice breakers
    .commands([BotCommand::new("track", "Track an order")]);
```

## Display name, health, coexistence

- `request_display_name_change(name)`: reviewed by Meta
  (`WebhookEvent::PhoneNumberNameUpdated`); once approved, `register`
  again to apply it.
- `get(&["quality_rating", "name_status", "status"])` returns a
  `PhoneNumberInfo`; `PhoneNumberQualityUpdated` webhooks report changes.
- Coexistence numbers (the merchant keeps the WhatsApp Business app) are
  already registered; start the data sync once, within 24 hours:

```rust
let number = merchant.phone_number(phone_number_id);
number
    .sync_smb_app_data(SmbSyncType::SmbAppStateSync)
    .await?; // contacts
number.sync_smb_app_data(SmbSyncType::History).await?; // a second call: SyncNotAllowed
```

## Pitfalls

- `register` and `deregister` share 10 requests per number per 72 hours;
  the 11th is 133016 (`ErrorKind::Registration`) and locks the number for
  72 hours. Neither is replayed; never loop on them.
- `sync_smb_app_data` is never replayed either: after a timeout, look at
  the `history` / `smb_app_state_sync` webhooks instead of asking again.
- Ids are path segments: a phone number id containing `/` cannot address
  another object (the client percent-encodes it).

## What wa-rs does not do

- No PIN policy (who chooses it, storage, recovery)
  ([open question 4](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#embedded-signup-onboarding-merchants)).
- Not wrapped: payload-encryption settings, WABA creation, system users.
- Nothing stores the synced contacts (`smb_app_state_sync`); the inbox
  records the synced history and the app's echoes (`wa-rs-cms-inbox`).

## Related skills

`wa-rs-embedded-signup` (numbers of merchants), `wa-rs-token-vault`,
`wa-rs-webhook-endpoint`, `wa-rs-webhook-events` (number and account
webhooks), `wa-rs-media` (profile picture handle).
