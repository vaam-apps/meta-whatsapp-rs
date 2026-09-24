---
name: wa-rs-embedded-signup
description: "Onboarding merchants' own WhatsApp numbers in a multi-tenant CMS with wa-rs Embedded Signup - LaunchOptions for FB.login, SignupSessions state bound to the tenant, EmbeddedSignupEvent parsing, EmbeddedSignup::onboard (step order, why the token is stored before subscribe/register, resume after a failed register, never retrying onboard), the encrypted TokenVault (keys, rotation, phone number to WABA routing) and per-merchant clients via with_token. Load when building the Connect WhatsApp flow, its callback, token storage, or anything that acts as a merchant."
---

# wa-rs-embedded-signup

> **Verified against wa-rs 91431ae (2026-09-24).** On another revision, trust
> the code over this page (see `skills/README.md`).

Module: `wa_rs::client::embedded_signup`. It implements Meta's **Tech
Provider** flow for Embedded Signup v4: the merchant clicks "Connect
WhatsApp", Meta's popup creates or selects their WABA and number, and your
backend ends up holding a business token scoped to that WABA, encrypted at
rest, routable by phone number id.

## The pieces

| Piece | Role |
| --- | --- |
| `LaunchOptions` | the JSON for `FB.login(callback, options)` (config id, `featureType`, pre-fill) |
| `SignupSessions` | a single-use `SignupState` binding one attempt to the merchant who started it |
| `EmbeddedSignupEvent` | parses the `WA_EMBEDDED_SIGNUP` message event the page forwards |
| `EmbeddedSignup::onboard` | code → verified, stored, subscribed, registered business |
| `TokenVault` | AES-256-GCM encrypted tokens on any `KvStore`, keyed by WABA, indexed by phone number id |

## Wiring (once, at startup)

```rust
use std::sync::Arc;
use wa_rs::AppCredentials;
use wa_rs::client::embedded_signup::{SignupSessions, TokenVault, VaultKey, VaultKeys};
use wa_rs::core::store::KvStore;

let kv: Arc<dyn KvStore> = Arc::new(wa_rs::adapters::store::PostgresKvStore::new(pool.clone()));
let es = client.embedded_signup(AppCredentials::new(app_id, app_secret)); // app secret: server only
let vault = TokenVault::new(
    kv.clone(),
    VaultKeys::new(VaultKey::from_base64("2026-09", &vault_key_b64)?), // 32 random bytes, base64
)?;
let sessions = SignupSessions::new(kv);
```

Use a **shared, persistent** `KvStore` (Postgres, or Redis with persistence
and a `noeviction` or `volatile-*` eviction policy) in production: the vault holds every merchant's token and sessions must be
visible to whichever instance receives the callback. `MemoryKvStore` loses
all tokens on restart.

## The flow

**1. Start** (your authenticated "connect" endpoint): bind an attempt to the
merchant and hand the page the launch options.

```rust
use std::time::Duration;
use wa_rs::client::embedded_signup::LaunchOptions;

let state = sessions.start(&merchant_id, Duration::from_secs(900)).await?; // minutes, not seconds
let options = LaunchOptions::new(config_id).to_json()?; // serde_json::Value for FB.login
// respond with { "state": state.as_str(), "options": options }
```

**2. Frontend**: `FB.login(callback, options)`; collect `authResponse.code`
from the callback and the `WA_EMBEDDED_SIGNUP` message event (check
`event.origin`), then POST `{state, code, event}` **immediately**: the code
is single-use and lives 30 seconds. Sketch: [references/frontend.md](references/frontend.md).

**3. Callback** (authenticated as the same merchant):

```rust
use wa_rs::client::embedded_signup::{
    EmbeddedSignupEvent, OnboardingRequest, SignupCode, SignupState,
};
use wa_rs::client::phone_numbers::TwoStepPin;

let state = SignupState::parse(&body.state)?;
if !sessions.redeem(&state, &merchant_id).await? {
    return Err(forbidden()); // expired, replayed, or another merchant's attempt
}
let event = EmbeddedSignupEvent::from_json(&body.event)?;
let EmbeddedSignupEvent::Finish { .. } = &event else {
    return Ok(cancelled(&event)); // Cancel / Error / Unknown: nothing to onboard
};
let request = OnboardingRequest::from_event(SignupCode::new(body.code.as_str())?, &event)?
    .register_with_pin(TwoStepPin::new(pin_for(&merchant_id))?); // 6 digits
let onboarded = es.onboard(&request, &vault).await?;
save_mapping(&merchant_id, &onboarded.waba_id, &onboarded.phone_number_ids);
```

`redeem` checks the tenant **inside the library**, is single-use, and a
wrong tenant does **not** burn the state. Do not use `consume` unless you
really want to compare tenants yourself. `EmbeddedSignupEvent` is
`#[non_exhaustive]`; `from_event` also refuses anything but `Finish`.

**4. Act as the merchant**, any time later:

```rust
let stored = vault.get_by_phone_number(&phone_number_id).await?.ok_or_else(not_connected)?;
let merchant = client.with_token(stored.token);
merchant.messages(phone_number_id.clone()).send(&msg).await?;
```

Webhooks carry the phone number id, so `get_by_phone_number` is the usual
lookup; `vault.get(&waba_id)` also works.

## `onboard`: step order, and what to do when a step fails

Every failure after input validation is `Error::Step { step, source }`; the
names are constants in `embedded_signup::steps`.

| Step | What | If it fails |
| --- | --- | --- |
| `exchange_code` | code → business token | Start over: the code is spent |
| `debug_token` | `GET debug_token` on the new token | Start over |
| `verify_assets` | token valid and issued to this app; WABA ∈ its grants; owner business read from Meta; claimed number ∈ the WABA's numbers | Start over; a mismatch means the browser lied or the merchant picked other assets |
| `store_token` | encrypt into the vault, index every number of the WABA | Fix the store, start over |
| `subscribe_app` | `POST /{waba}/subscribed_apps` | Fix, then **`resume`** |
| `register_phone` | `POST /{phone}/register` with the PIN | Fix (e.g. the right PIN), then **`resume`** |

- **Never retry `onboard` itself.** The first step spends the code; a second
  call fails at `exchange_code` and hides the real error.
- **The token is stored before subscribe/register on purpose.** Those two can
  fail for fixable reasons (a number that already has a different two-step
  PIN: `ErrorKind::TwoStepVerification`, 133005; a Meta hiccup). Storing last
  would lose the only token and force the merchant through the whole popup
  again.
- `resume(&waba_id, &request, &vault)` loads the stored token (`load_token`),
  checks the request against what onboarding verified (`verify_assets`), and
  reruns only `subscribe_app` and `register_phone`. `request.code` is ignored:
  to resume later, rebuild the request with the saved `SessionInfo` (it is
  `Serialize`) or `SessionInfo::default()` (registers the onboarded number).
  **Check that `waba_id` belongs to the calling merchant first** — `resume`
  acts with whatever token is stored for the WABA you name.
- `register_phone` counts against 10 registrations per 72 h; 133016 locks the
  number for 72 h (`ErrorKind::Registration`, never auto-retried).

```rust
use wa_rs::Error;
use wa_rs::client::embedded_signup::steps;

match es.onboard(&request, &vault).await {
    Ok(done) => Ok(done),
    Err(Error::Step { step, source }) if step == steps::REGISTER_PHONE || step == steps::SUBSCRIBE_APP => {
        // token is safe in the vault; show the cause (source.kind()), let the merchant fix it,
        // then: es.resume(&waba_id, &fixed_request, &vault).await
        Err(Error::Step { step, source })
    }
    Err(e) => Err(e), // earlier step: the merchant must run the popup again
}
```

## Never trust the browser — the library already checks

The message event's ids are claims. `onboard` refuses a `debug_token` answer
without this app's id, requires the WABA to be in the token's
`whatsapp_business_management` target ids, reads the owner business from
Meta (the browser's `business_id` is ignored), and requires the claimed
number to be one Meta lists on that WABA. Keep it that way:

- Do not skip `onboard` and write tokens yourself. `TokenVault::store`
  **trusts its input**: every phone number id in the record is routed to that
  WABA, so storing browser-supplied ids lets one merchant capture another's
  customers.
- The **app secret never leaves the server**; the page needs only the app id
  and the configuration id.
- Never log the code, the posted event body, tokens or the PIN (their types
  redact `Debug`; `expose_secret()` does not).
- Limit the callback's request body size in your HTTP layer.

## The vault

- **Key**: 32 random bytes (e.g. `openssl rand -base64 32`) in your secret
  manager, *not* in the database that holds the `KvStore`. Key ids are 1–64
  chars of `A-Z a-z 0-9 - _ . :`, stored with each record. `VaultKey::generate`
  cannot export its bytes — only for tests. Lose the key and every merchant
  must onboard again.
- **Rotation**: `VaultKeys::new(new_key).with_previous(old_key)`. Reads
  re-encrypt old records under the active key (`rotate_on_read`, default
  `true`; turn off on read-only replicas). The vault cannot list its records:
  iterate *your* WABA table calling `vault.rotate(&waba_id)`, then drop the old
  key.
- Records are bound to their WABA id (associated data): a record copied under
  another WABA's key fails with `CryptoError::Decrypt` instead of leaking.
- `get_by_phone_number` returns a token only if the WABA's authenticated
  record lists the number; a stale index yields `None`.
- Offboarding: `vault.delete(&waba_id)` (also unlinks its numbers).
- The vault knows WABAs, not tenants. Re-onboarding a WABA replaces its entry;
  if a second merchant onboards a WABA already mapped to another, deciding
  what that means is your product's call.

## Coexistence (merchant keeps the WhatsApp Business app)

`LaunchOptions::new(config_id).coexistence()`. The flow ends with
`FinishKind::WhatsappBusinessAppOnboarding` and only a WABA id; do **not**
call `register_with_pin` (validation refuses it). If
`onboarded.needs_coexistence_sync()`, within 24 hours and once each call
`merchant.phone_number(pnid).sync_smb_app_data(SmbSyncType::SmbAppStateSync)`
and `…(SmbSyncType::History)` (`wa_rs::client::phone_numbers::SmbSyncType`;
a second call fails with `ErrorKind::SyncNotAllowed`), and subscribe to the `history`,
`smb_app_state_sync` and `smb_message_echoes` webhook fields. The inbox does
not record echoes or history yet (`wa-rs-cms-inbox`).

## Not handled by the library (open product decisions)

- **Tech Provider vs Solution Partner.** Only the Tech Provider flow is
  implemented. Solution Partner credit-line sharing, pre-verified number
  pools and partner APIs are not (`docs/coverage.md` rows 1, 28).
- **PIN policy.** `register` needs a 6-digit PIN that becomes (or must match)
  the number's two-step PIN. Who chooses it, whether you store it (encrypted,
  your code) and how a merchant recovers it are yours to decide.
- **Token refresh.** `Onboarded::token_expires_at` / `StoredBusinessToken::expires_at`
  record Meta's expiry when there is one; nothing refreshes a token. An
  `ErrorKind::Authentication` (190) on a merchant's calls means: run Embedded
  Signup again for that merchant.
- **Multi-WABA flows.** Only `SessionInfo::primary_waba_id()` is onboarded.
- **Tenant ownership** of WABAs and phone numbers: keep your own mapping and
  check it before `resume`, `vault.get*`, and every send.
