---
name: meta-whatsapp-rs-embedded-signup
description: "Letting each merchant of a multi-tenant CMS connect their own WhatsApp number with Embedded Signup and meta-whatsapp-rs (Tech Provider, or Solution Partner funding merchants with its credit line, chosen per deployment) - Meta prerequisites, LaunchOptions for FB.login, SignupSessions binding the attempt to the merchant (start, redeem), parsing the WA_EMBEDDED_SIGNUP event, EmbeddedSignup::onboard and onboard_with_approval (code exchange, token checks, your approval gate, encrypted storage, subscribe, credit line sharing, register with the merchant's PIN), the Error::Step names, resume after a failed step, offboarding and revoking the credit line when a merchant leaves, partner-led business verification of a merchant's business, coexistence. Load when building the Connect WhatsApp button, its callback endpoint, offboarding, or recovering a half-finished onboarding."
---

# meta-whatsapp-rs-embedded-signup

> **Verified against meta-whatsapp-rs 42e6fd1c486feffcde54d676b7e799a618e1a94b (2026-09-25).** On another revision, trust the code over this page.

Reference code, compiled and tested by meta-whatsapp-rs's own gate:
[examples/onboarding.rs](examples/onboarding.rs), [examples/solution_partner.rs](examples/solution_partner.rs), [examples/business_verification.rs](examples/business_verification.rs). The page side: [references/frontend.md](references/frontend.md). A full
**Tech Provider** server: [`embedded_signup.rs`](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/crates/meta-whatsapp-rs/examples/embedded_signup.rs).

## When to use

A merchant clicks "Connect WhatsApp", goes through Meta's popup, and your
backend ends up holding their business token — verified, encrypted, and
routable by phone number id. Module `meta_whatsapp_rs::client::embedded_signup`.

## On Meta's side first

A **Tech Provider**: verified business, Advanced access to
`whatsapp_business_messaging` and `whatsapp_business_management`, the
page's HTTPS domains allowed in Facebook Login for Business, a Login
configuration (its **configuration id**), the app subscribed to
`messages` and `account_update`. Merchants add a payment method before
their number can send, unless you are a **Solution Partner** sharing your
credit line ([references/solution-partner.md](references/solution-partner.md):
one `SolutionPartner` per deployment, a currency per merchant, approval
required; partner-led business verification). [Guide](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/embedded-signup.md).

## Wire once, on a shared store

```rust
let client = meta_whatsapp_rs::client_builder()?.build()?; // no default token
let es = client.embedded_signup(AppCredentials::new(app_id, app_secret));
let vault = TokenVault::new(
    kv.clone(),
    VaultKeys::new(VaultKey::from_base64("2026-09", vault_key_b64)?),
)?;
let sessions = SignupSessions::new(kv);
```

The callback may land on another instance than the start, and the vault
holds every merchant's token: Postgres, or Redis with persistence and
`noeviction` (`meta-whatsapp-rs-storage`); never `MemoryKvStore` in production.

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
    Err(e @ Error::Step { step, .. }) if AFTER_STORE.contains(&step) => {
        match request.session.primary_waba_id() {
            Some(waba) => Ok(Completion::Resumable(waba.clone(), Box::new(e))),
            None => Ok(Completion::StartOver(Box::new(e))),
        }
    }
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
| `approve` (`onboard_with_approval`, `resume_with_approval`) | your refusal: nothing stored, subscribed or shared; a Solution Partner's `resume` of an unapproved WABA |
| `store_token` | fix the store, start over |
| `subscribe_app`, `assign_system_user`, `share_credit_line`, `register_phone` (`AFTER_STORE`) | the token is stored: fix the cause, then **`resume`** |

The browser's ids are claims: `verify_assets` checks them with Meta. The
token is stored first: later steps fail for fixable reasons (a wrong PIN,
`ErrorKind::TwoStepVerification`). Credit steps check before they post,
refuse a revoked business, answer a lost share with `Reconcile` (look
first; one Meta never lists, staff clear: `clear_pending_share`). Refuse
others' WABAs in `onboard_with_approval`; revoke at once on every
`PartnerRemoved`; `reshare_after_revocation` is business-wide: gate it.

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
  merchant's customers to another (`meta-whatsapp-rs-token-vault`).
- The app secret stays on the server; limit the callback's body size;
  never log the code, the event body, tokens or the PIN.
- **Coexistence** (merchants keeping the WhatsApp Business app): no
  `register`; if `onboarded.needs_coexistence_sync()`, call
  `sync_smb_app_data` once per `SmbSyncType` within 24 hours
  (`meta-whatsapp-rs-phone-numbers`).

~~`redeem` first, then parse the event, code and PIN~~ (until 2026-09-24):
a malformed post spent the attempt. Check everything local first.

## What meta-whatsapp-rs does not do

- ~~Only the Tech Provider flow~~ (before 581f9b1). No pre-verified
  number pools or multi-WABA onboarding; no token refresh (an expired
  token means running the flow again), no PIN policy, no code-less
  `OnboardingRequest` for `resume` after a restart (hence the placeholder
  code above) ([OPEN_QUESTIONS.md #4–#12](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/OPEN_QUESTIONS.md#embedded-signup-onboarding-merchants), all decided: roadmap L4, L11).
- No tenant model: which of your merchants owns a WABA is your table.

## Related skills

`meta-whatsapp-rs-token-vault` (the tokens afterwards), `meta-whatsapp-rs-phone-numbers`
(register, PIN, profile), `meta-whatsapp-rs-webhook-endpoint` (one callback for every
merchant), `meta-whatsapp-rs-cms-inbox`, `meta-whatsapp-rs-storage`.
