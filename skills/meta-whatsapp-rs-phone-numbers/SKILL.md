---
name: meta-whatsapp-rs-phone-numbers
description: "Managing WhatsApp business phone numbers and their WABA with meta-whatsapp-rs - registering a number for Cloud API with its two-step verification PIN (TwoStepPin, the 10-per-72-hours limit), changing the PIN and the display name, the business profile (about, email, websites), conversational components (ice breakers, commands), subscribing the app to a WABA's webhooks and per-number callback overrides, number health (quality, health status), the coexistence data sync, the business username, deleting a contact book entry, the Official Business Account (blue check) and compliance information. Load when registering or configuring a number, editing the WhatsApp business profile, checking a number's quality or health, setting a business username, or making sure webhooks arrive for a WABA."
---

# meta-whatsapp-rs-phone-numbers

> **Verified against meta-whatsapp-rs b7438e26d691338af798e102e24637744aabf31b (2026-09-27).** On another revision, trust the code over this page.

Reference code: [examples/numbers.rs](examples/numbers.rs), compiled and
tested by meta-whatsapp-rs's own gate.

## When to use

Everything about one business number (`client.phone_number(pnid)`), its
profile (`client.business_profile(pnid)`) and its WABA
(`client.waba(waba_id)`). For a merchant's number, use their client
(`with_token`, `meta-whatsapp-rs-token-vault`).

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
a Resumable Upload handle (`profile_picture_handle`, `meta-whatsapp-rs-media`).
Conversational components:

```rust
let components = ConversationalAutomationConfig::new()
    .prompts(["Where is my order?", "Opening hours"]) // at most 4 ice breakers
    .commands([BotCommand::new("track", "Track an order")]);
```

A bot built with `meta-whatsapp-rs-bot` publishes its own commands as this
menu: `Bot::sync_command_menu` (only `commands`, checked the same way).

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

## Username, contact book, blue check

```rust
let number = client.phone_number(phone_number_id);
match number.set_username(username, None).await {
    // 147001–147005 are `ErrorKind::Unknown` for now: branch on the code
    Err(e) if e.graph().map(|g| g.code) == Some(147005) && take_it_from_our_other_number => {
        let transfer = Some(&TransferAction::ForceTransfer);
        number.set_username(username, transfer).await
    }
    other => other, // `Reserved`: approved, visible once users have usernames
}
```

- `set_username` checks the format first (`validate_username`); its
  errors `147001`–`147005` are `ErrorKind::Unknown` for now (branch on
  the `code` of `Error::graph`, never on the message). `username()`,
  `username_suggestions()` (reserved for you), `delete_username()`.
- `delete_contact_book_entry(&bsuid)` erases a user's entry in the
  portfolio's contact book at Meta: no undo, never called for you (erasure: `meta-whatsapp-rs-production`).
- `official_business_account()` reads the blue check;
  `request_official_business_account(&ObaApplication::new(url, country))`
  applies, never replayed after a timeout, only when Meta refused it
  before processing it (`Error::may_resend`); 30 days after a rejection.
- Also on the number: `set_search_visibility`, `set_security_notifications`,
  `set_notify_user_change_number`, `business_compliance_info`,
  `set_business_compliance_info` (India), `health_status()` (a WABA's
  too; the portfolio's own: `health.entity(&HealthEntityType::Business)`);
  bot details: `Client::waba_bot(bot_id).get(&[])`.

## Pitfalls

- `register` and `deregister` share 10 requests per number per 72 hours;
  the 11th is 133016 (`ErrorKind::Registration`) and locks the number for
  72 hours. Neither is replayed; never loop on them.
- `sync_smb_app_data` is never replayed either: after a timeout, look at
  the `history` / `smb_app_state_sync` webhooks instead of asking again.
- Ids are path segments: a phone number id containing `/` cannot address
  another object (the client percent-encodes it).

## What meta-whatsapp-rs does not do

- No PIN storage or recovery: the caller supplies the PIN on every
  attempt
  ([OPEN_QUESTIONS.md #4](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/OPEN_QUESTIONS.md#embedded-signup-onboarding-merchants),
  decided on 2026-09-26).
- Not wrapped: payload-encryption settings (roadmap L26), WABA creation,
  system users, withdrawing an Official Business Account application (no
  documented field).
- Nothing stores the synced contacts (`smb_app_state_sync`); the inbox
  records the synced history and the app's echoes (`meta-whatsapp-rs-cms-inbox`).
  ~~The inbox does not record coexistence echoes or synced history~~:
  true until a3582b8 (2026-09-24).

## Related skills

`meta-whatsapp-rs-embedded-signup` (numbers of merchants), `meta-whatsapp-rs-token-vault`,
`meta-whatsapp-rs-webhook-endpoint`, `meta-whatsapp-rs-webhook-events` (number and account
webhooks), `meta-whatsapp-rs-media` (profile picture handle).
