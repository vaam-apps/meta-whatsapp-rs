---
name: wa-rs-embedded-signup
description: "Letting each merchant of a multi-tenant CMS connect their own WhatsApp number with Embedded Signup and wa-rs (Tech Provider flow) - Meta prerequisites, LaunchOptions for FB.login, SignupSessions binding the attempt to the merchant (start, redeem), parsing the WA_EMBEDDED_SIGNUP event, EmbeddedSignup::onboard (code exchange, token checks, encrypted storage, subscribe, register with the merchant's PIN), the Error::Step names, resume after a failed register, coexistence. Load when building the Connect WhatsApp button, its callback endpoint, or recovering a half-finished onboarding."
---

# wa-rs-embedded-signup

> **Verified against wa-rs 4eb93c9bd63812221e75ad0920b6e2cb98ea0dd6 (2026-09-24).** On another revision, trust the code over this page.

Reference code: [examples/onboarding.rs](examples/onboarding.rs), compiled
and tested by wa-rs's own gate. The page side:
[references/frontend.md](references/frontend.md). A full server with
authentication: [`embedded_signup.rs`](https://github.com/vaam-apps/wa-rs/blob/main/crates/wa-rs/examples/embedded_signup.rs).

## When to use

A merchant clicks "Connect WhatsApp", goes through Meta's popup, and your
backend ends up holding their business token — verified, encrypted, and
routable by phone number id. Module `wa_rs::client::embedded_signup`.

## On Meta's side first

A **Tech Provider**: verified business, Advanced access to
`whatsapp_business_messaging` and `whatsapp_business_management`, the
page's HTTPS domains allowed in Facebook Login for Business, a Login
configuration (its **configuration id**), the app subscribed to
`messages` and `account_update`. Merchants add a payment method before
their number can send. Steps:
[embedded-signup guide](https://github.com/vaam-apps/wa-rs/blob/main/docs/guides/embedded-signup.md).

## Wire once, on a shared store

```rust
let client = wa_rs::client_builder()?.build()?; // no default token
let es = client.embedded_signup(AppCredentials::new(app_id, app_secret));
let vault = TokenVault::new(
    kv.clone(),
    VaultKeys::new(VaultKey::from_base64("2026-09", vault_key_b64)?),
)?;
let sessions = SignupSessions::new(kv);
```

The callback may land on another instance than the start, and the vault
holds every merchant's token: Postgres, or Redis with persistence and
`noeviction` (`wa-rs-storage`); never `MemoryKvStore` in production.

## Start: bind the attempt to the merchant

```rust
let state = signup
    .sessions
    .start(merchant_id, Duration::from_mins(15))
    .await?; // minutes: several screens, maybe an SMS
let options = LaunchOptions::new(signup.config_id.as_str()).to_json()?; // .coexistence() for WhatsApp Business app users
```

The page runs `FB.login(callback, options)`, collects the code and the
message event, asks the merchant for their number's two-step PIN, and
posts all of it at once: the code lives 30 seconds.

## Complete: local checks, redeem, onboard

```rust
// Local checks first: a malformed post must not burn the single-use state.
let state = SignupState::parse(state)?;
let code = SignupCode::new(code)?;
let pin = pin.map(TwoStepPin::new).transpose()?;
let event = EmbeddedSignupEvent::from_json(event)?;
if !matches!(event, EmbeddedSignupEvent::Finish { .. }) {
    return Ok(Completion::Cancelled); // Cancel / Error / Unknown: nothing to onboard
}
let mut request = OnboardingRequest::from_event(code, &event)?;
if let (Some(FinishKind::Finish), Some(pin)) = (event.finish_kind(), pin) {
    request = request.register_with_pin(pin); // never for coexistence numbers
}
if !signup.sessions.redeem(&state, merchant_id).await? {
    return Ok(Completion::Stale);
}
// Never retry `onboard`: its first step spends the code.
match signup.es.onboard(&request, &signup.vault).await {
    Ok(done) => Ok(Completion::Connected(Box::new(done))), // save done.waba_id for merchant_id
    Err(
        e @ Error::Step {
            step: steps::SUBSCRIBE_APP | steps::REGISTER_PHONE,
            ..
        },
    ) => match request.session.primary_waba_id() {
        Some(waba) => Ok(Completion::Resumable(waba.clone(), Box::new(e))),
        None => Ok(Completion::StartOver(Box::new(e))),
    },
    Err(e) => Ok(Completion::StartOver(Box::new(e))),
}
```

`merchant_id` comes from **your** session — never from the page, the body
or the URL. `redeem` checks the tenant inside the library, succeeds once,
and a wrong tenant does not burn the state (prefer it to `consume`). Save
`Onboarded::waba_id` and `phone_number_ids` against the merchant.

## What `onboard` does, and when a step fails

Every failure after input checks is `Error::Step { step, source }`; the
names are constants in `embedded_signup::steps`:

| Step | Then |
| --- | --- |
| `exchange_code`, `debug_token`, `verify_assets` | start over: the code is spent |
| `store_token` | fix the store, start over |
| `subscribe_app`, `register_phone` | the token is stored: fix the cause, then **`resume`** |

The browser's ids are claims: `verify_assets` checks them with Meta. The
token is stored **before** subscribe and register on purpose: those fail
for fixable reasons (a wrong PIN, `ErrorKind::TwoStepVerification`).

```rust
let request = OnboardingRequest::new(SignupCode::new("unused")?, saved_session)
    .register_with_pin(TwoStepPin::new(corrected_pin)?);
signup.es.resume(waba_id, &request, &signup.vault).await
```

**Check that `waba_id` belongs to the calling merchant before `resume`**:
it acts with whatever token is stored for that WABA.

## Pitfalls

- The PIN is the merchant's: it becomes the number's two-step PIN or
  must match it. Parse it before `redeem`; never log or store it; never
  one PIN for every merchant.
- `register_phone` counts against 10 (de)registrations per 72 h; 133016
  locks the number for 72 h (`ErrorKind::Registration`, never retried).
- Never write tokens yourself instead of `onboard`: `TokenVault::store`
  trusts its phone number ids, so browser-supplied ids would route one
  merchant's customers to another (`wa-rs-token-vault`).
- The app secret stays on the server; limit the callback's body size;
  never log the code, the event body, tokens or the PIN.
- **Coexistence** (merchants keeping the WhatsApp Business app): no
  `register`; if `onboarded.needs_coexistence_sync()`, call
  `sync_smb_app_data` once per `SmbSyncType` within 24 hours
  (`wa-rs-phone-numbers`).

~~`redeem` first, then parse the event, code and PIN~~ (until 2026-09-24):
a malformed post spent the attempt. Check everything local first.

## What wa-rs does not do

- Only the Tech Provider flow: no Solution Partner credit lines,
  pre-verified number pools or multi-WABA onboarding; no token refresh
  (an expired token means running the flow again)
  ([open questions 3–12](https://github.com/vaam-apps/wa-rs/blob/main/OPEN_QUESTIONS.md#embedded-signup-onboarding-merchants)).
- No PIN policy (who chooses it, recovery) and no code-less
  `OnboardingRequest` for `resume` after a restart (hence the placeholder
  code above, open question 10).
- No tenant model: which of your merchants owns a WABA is your table.

## Related skills

`wa-rs-token-vault` (the tokens afterwards), `wa-rs-phone-numbers`
(register, PIN, profile), `wa-rs-webhook-endpoint` (one callback for every
merchant), `wa-rs-cms-inbox`, `wa-rs-storage`.
