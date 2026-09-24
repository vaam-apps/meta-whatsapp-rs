# Embedded Signup: merchants connect their own number

**Goal:** a merchant of your CMS clicks "Connect WhatsApp", goes through
Meta's popup, and your backend ends up holding their business token,
encrypted, routable by phone number id, with webhooks flowing and the
number registered.

wa-rs implements the **Tech Provider** flow of Embedded Signup v4.
Example: [`embedded_signup.rs`](../../crates/wa-rs/examples/embedded_signup.rs).
Agent skills:
[`wa-rs-embedded-signup`](../../skills/wa-rs-embedded-signup/SKILL.md),
[`wa-rs-token-vault`](../../skills/wa-rs-token-vault/SKILL.md).
Run the example with a tenant bearer token (`WA_TENANTS`, a stand-in for
your CMS's own login; it refuses to start without one); it listens on
`127.0.0.1` unless `WA_BIND` names another address:

```text
TOKEN=$(openssl rand -hex 32)   # the demo tenant's bearer token: paste it into the page
WA_TENANTS='{"demo-merchant": {"token": "'"$TOKEN"'"}}' \
  WA_APP_ID=… WA_APP_SECRET=… WA_ES_CONFIG_ID=… \
  cargo run -p wa-rs --example embedded_signup --features axum
```

```text
browser (merchant, signed in to your CMS)      your backend                         Meta
POST /whatsapp/connect ───────────────────────► SignupSessions::start(merchant)
     ◄── {state, options} ─────────────────────┘
FB.login(callback, options) ─────────────────────────────────────────────────────► popup
     ◄── code (30 s, single use) + WA_EMBEDDED_SIGNUP message event ───────────────┘
POST /whatsapp/connect/callback {state, code, event, pin}
                                               ► redeem(state, merchant)
                                                 EmbeddedSignup::onboard ─────────► exchange code, debug_token,
                                                   └► TokenVault (by WABA, by number)  verify WABA + number,
                                                                                      subscribe app, register
```

## 1. On Meta's side

| # | Step | Meta's page |
| --- | --- | --- |
| 1 | Become a Tech Provider: verify your business, then pass App Review for **Advanced access** to `whatsapp_business_messaging` and `whatsapp_business_management`. Without it, merchants cannot grant your app those permissions in the popup, and calls on their WABAs fail with Graph error `200`. | [get-started-for-tech-providers](https://developers.facebook.com/documentation/business-messaging/whatsapp/solution-providers/get-started-for-tech-providers), [permissions](https://developers.facebook.com/documentation/business-messaging/whatsapp/permissions) |
| 2 | Facebook Login for Business → Settings → Client OAuth settings: switch on client OAuth login, web OAuth login, enforce HTTPS, embedded browser OAuth login, strict mode for redirect URIs and login with the JavaScript SDK. List every domain that serves the connect page (development ones too, HTTPS only) under **Allowed domains** and **Valid OAuth redirect URIs**; otherwise the popup never hands the code back. | [embedded-signup/implementation](https://developers.facebook.com/documentation/business-messaging/whatsapp/embedded-signup/implementation) |
| 3 | Facebook Login for Business → Configurations: create a configuration from Meta's WhatsApp Embedded Signup template (or a custom one using the WhatsApp Embedded Signup login variation). Ask only for the assets you use: every extra screen loses merchants. Keep the **configuration id**. | same |
| 4 | Configure your app's webhook callback ([webhooks.md](webhooks.md)) and subscribe to `messages` and `account_update` (Meta requires the latter for Embedded Signup). | [webhooks/overview](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/overview) |
| 5 | Tell each merchant to add a payment method in WhatsApp Manager after connecting: until then their number cannot send. | [onboarding-customers-as-a-tech-provider](https://developers.facebook.com/documentation/business-messaging/whatsapp/embedded-signup/onboarding-customers-as-a-tech-provider) |

The page needs only the **app id** and the **configuration id**; the **app
secret** stays on the server.

## 2. Wire the backend once

```rust
use std::sync::Arc;
use wa_rs::adapters::store::PostgresKvStore;
use wa_rs::client::embedded_signup::{SignupSessions, TokenVault, VaultKey, VaultKeys};
use wa_rs::prelude::*;

let kv: Arc<dyn KvStore> = Arc::new(PostgresKvStore::new(pool.clone())); // shared by every instance
let client = wa_rs::client_builder()?.build()?; // no default token: onboarding acts as the app, then as the merchant
let es = client.embedded_signup(AppCredentials::new(app_id, app_secret));
let vault = TokenVault::new(kv.clone(), VaultKeys::new(VaultKey::from_base64("2026-09", &vault_key_b64)?))?;
let sessions = SignupSessions::new(kv);
```

Use a shared, persistent `KvStore` (Postgres, or Redis with persistence and
the `noeviction` policy): the callback may land on another instance than the
start, and the vault holds every merchant's token. `MemoryKvStore` loses
them all on restart.

## 3. Start an attempt

Behind your own authentication, bind a single-use state to the merchant and
hand the page the `FB.login` options:

```rust
use std::time::Duration;
use wa_rs::client::embedded_signup::LaunchOptions;

let state = sessions.start(&merchant_id, Duration::from_mins(15)).await?; // minutes: several screens, maybe an SMS
let options = LaunchOptions::new(config_id.as_str()).to_json()?; // .coexistence() for WhatsApp Business app users
// respond {"state": state.as_str(), "options": options}
```

## 4. The page

The page is served to a merchant signed in to your CMS: both `fetch` calls
carry their session (a cookie here), and the backend takes the merchant
from it, never from anything the page sends. The page also asks for the
number's two-step verification PIN (section 5).

```html
<script async defer crossorigin="anonymous" src="https://connect.facebook.net/en_US/sdk.js"></script>
<script>
  window.fbAsyncInit = () => FB.init({ appId: '<APP_ID>', autoLogAppEvents: true, xfbml: true, version: 'v25.0' });

  let attempt = null, code = null, sessionEvent = null, sent = false;
  // Fetched before the click: FB.login must run inside the click handler or the popup is blocked.
  fetch('/whatsapp/connect', { method: 'POST' }).then((r) => r.json())
    .then((a) => { attempt = a; document.getElementById('connect').disabled = false; });

  window.addEventListener('message', (e) => {
    let url;
    try { url = new URL(e.origin); } catch { return; }
    const host = url.hostname; // exact host check: endsWith('facebook.com') also accepts evilfacebook.com
    if (url.protocol !== 'https:' || (host !== 'facebook.com' && !host.endsWith('.facebook.com'))) return;
    const raw = typeof e.data === 'string' ? e.data : JSON.stringify(e.data);
    let data; try { data = JSON.parse(raw); } catch { return; }
    if (data.type !== 'WA_EMBEDDED_SIGNUP') return;
    if (data.event === 'CANCEL' || data.event === 'ERROR') return show('Not connected: ' + raw);
    sessionEvent = raw; submit(); // keep ids as strings: never round-trip them through JS numbers
  });

  function connect() {
    FB.login((r) => { if (r.authResponse) { code = r.authResponse.code; submit(); } }, attempt.options);
  }

  function submit() { // the code lives 30 seconds: post as soon as both halves are here
    if (sent || !code || !sessionEvent) return;
    sent = true;
    const pin = document.getElementById('pin').value || null; // the merchant's own PIN: never logged
    fetch('/whatsapp/connect/callback', { method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ state: attempt.state, code, event: sessionEvent, pin }) }).then((r) => r.text()).then(show);
  }
  function show(text) { document.getElementById('result').textContent = text; }
</script>
<label>Two-step verification PIN of the number (6 digits: the current one, or the one to set)
  <input id="pin" type="password" inputmode="numeric" pattern="[0-9]{6}" maxlength="6" autocomplete="off"></label>
<button id="connect" onclick="connect()" disabled>Connect WhatsApp</button><pre id="result"></pre>
```

Serve `version` from `ApiVersion::DEFAULT` (the example fills it in), and
the page over HTTPS from an allowed domain.

## 5. Complete: redeem, then onboard

```rust
use wa_rs::client::embedded_signup::{
    EmbeddedSignup, EmbeddedSignupEvent, FinishKind, Onboarded, OnboardingRequest, SignupCode,
    SignupSessions, SignupState, TokenVault, steps,
};
use wa_rs::client::phone_numbers::TwoStepPin;
use wa_rs::prelude::*;

pub enum Completed {
    Connected(Onboarded),
    NotFinished(EmbeddedSignupEvent), // cancelled, errored or unknown: nothing to onboard
    StaleAttempt,                     // expired, already used, or another merchant's state
    Resumable { waba_id: WabaId, request: OnboardingRequest, error: Error }, // token stored, tail failed
}

pub async fn complete(
    es: &EmbeddedSignup, sessions: &SignupSessions, vault: &TokenVault,
    merchant_id: &str, // from YOUR session, never from the page
    state: &str, code: String, event: &str, pin: Option<TwoStepPin>,
) -> wa_rs::Result<Completed> {
    // Local checks first: a malformed post must not burn the attempt.
    let state = SignupState::parse(state)?;
    let code = SignupCode::new(code)?;
    let event = EmbeddedSignupEvent::from_json(event)?;
    let Some(kind) = event.finish_kind() else { return Ok(Completed::NotFinished(event)) };
    let mut request = OnboardingRequest::from_event(code, &event)?;
    if let (FinishKind::Finish, Some(pin)) = (kind, pin) {
        request = request.register_with_pin(pin); // only Cloud API numbers are registered
    }
    if !sessions.redeem(&state, merchant_id).await? {
        return Ok(Completed::StaleAttempt);
    }
    match es.onboard(&request, vault).await { // the code is spent: never call this twice
        Ok(onboarded) => {
            save_merchant_waba(merchant_id, &onboarded.waba_id).await; // your tenant ↔ WABA table
            Ok(Completed::Connected(onboarded))
        }
        Err(error @ Error::Step { step: steps::SUBSCRIBE_APP | steps::REGISTER_PHONE, .. }) => {
            let Some(waba_id) = request.session.primary_waba_id().cloned() else { return Err(error) };
            save_merchant_waba(merchant_id, &waba_id).await;
            Ok(Completed::Resumable { waba_id, request, error })
        }
        Err(error) => Err(error), // an earlier step: the merchant starts again
    }
}
```

The PIN is the merchant's: registering a Cloud API number sets it as the
number's two-step verification PIN, or must match the one it already has.
Parse it with the other inputs (`TwoStepPin::new`, exactly 6 digits) before
`redeem`, so a typo does not burn the attempt; never log or store it; never
register every merchant's number with one PIN of yours, which one leak would
expose. Without a PIN the number is left unregistered (a later `resume`
with one registers it). Who chooses and keeps PINs is an
[open decision](#open-decisions) (4).

`onboard` runs these steps; every failure is `Error::Step { step, source }`
with the names in `embedded_signup::steps`:

| Step | Does | On failure |
| --- | --- | --- |
| `exchange_code` | code → business token | start over (the code is spent) |
| `debug_token` | inspects the new token | start over |
| `verify_assets` | the WABA is among the token's grants, its owner is read from Meta, the number belongs to the WABA | start over; a mismatch means the browser's ids were wrong |
| `store_token` | encrypts the token into the vault, indexes every number of the WABA | fix the store, start over |
| `subscribe_app` | `POST /{waba}/subscribed_apps` | fix, then `resume` |
| `register_phone` | `POST /{number}/register` with the PIN | fix (often the PIN), then `resume` |

The token is stored **before** the last two steps on purpose: a wrong PIN
(`ErrorKind::TwoStepVerification`, 133005) must not cost the merchant the
whole popup again. Registration counts against 10 per 72 hours; 133016
locks the number for 72 hours and is never retried automatically.

## 6. Resume

```rust
// `resume` acts with whatever token is stored for the WABA you name:
// check that it belongs to the calling merchant first.
if !merchant_owns_waba(&merchant_id, &waba_id).await { return Err(forbidden()) }
let request = request.register_with_pin(TwoStepPin::new(corrected_pin)?);
let done = es.resume(&waba_id, &request, &vault).await?; // subscribe + register only
```

After a restart you no longer have the `OnboardingRequest`; rebuild one with
a placeholder code (ignored by `resume`) and an empty `SessionInfo`:
`OnboardingRequest::new(SignupCode::new("unused")?, SessionInfo::default()).register_with_pin(pin)`.
With no number named, `resume` registers the first number stored for the
WABA (the onboarded one); without `register_with_pin` it only subscribes.
A code-less constructor is an [open question](../../OPEN_QUESTIONS.md#embedded-signup-onboarding-merchants) (10).

## 7. The token vault

- **Key:** 32 random bytes (`openssl rand -base64 32`) in your secret
  manager, never in the database that holds the vault. Key ids are 1–64
  characters of `A-Z a-z 0-9 - _ . :` and are stored with each record.
  `VaultKey::generate` cannot export its bytes: tests only. Lose the key and
  every merchant must connect again.
- **Rotation:** start with `VaultKeys::new(new_key).with_previous(old_key)`.
  Reads re-encrypt old records under the active key (`rotate_on_read`, on by
  default; turn it off on read-only replicas). The vault cannot list its
  records, so walk *your* merchant table calling `vault.rotate(&waba_id)`,
  then drop the old key.
- Records are bound to their WABA id: a record copied into another WABA's
  entry fails to decrypt instead of handing out the wrong token.
- `TokenVault::store` trusts its input: every phone number id in the record
  is routed to that WABA. Only `onboard` (which checked the ids with Meta)
  should write it.

## 8. Route inbound webhooks to merchants

All merchants' webhooks arrive at your one callback URL. Messages and
statuses carry the business phone number id:

```rust
let Some(number) = event.phone_number_id() else { return Ok(()) }; // account/template events: use event.waba_id()
let Some(stored) = vault.get_by_phone_number(number).await? else { return Ok(()) }; // not connected (any more)
let merchant = merchant_of_waba(&stored.waba_id).await; // your table, the vault knows no tenants
```

To send one merchant's traffic elsewhere, pass
`CallbackOverride::new(url, verify_token)` to
`OnboardingRequest::subscribe_override`. Overrides only apply to some fields
(`messages`, calls, groups, coexistence sync); template and account webhooks
always go to the app's callback
([webhooks/override](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/override)).

## 9. Offboarding

- **The merchant disconnects in your CMS:** with their token, stop the
  webhooks, then forget the token:
  `client.with_token(stored.token).waba(stored.waba_id.clone()).unsubscribe_app().await?`,
  then `vault.delete(&stored.waba_id).await?` and your own mapping.
- **Meta tells you:** `WebhookEvent::AccountUpdated` whose `update.event`
  is `AccountUpdateEvent::PartnerAppUninstalled` (the business removed your
  app) or `AccountDeleted`: delete the vault entry. `AccountOffboarded` and
  `PartnerRemoved` with disconnection details concern coexistence numbers
  that changed device or number: ask the merchant to reconnect.
- **The token stops working:** `ErrorKind::Authentication` (190) on a
  merchant's calls. Nothing refreshes tokens; the merchant runs Embedded
  Signup again. `StoredBusinessToken::is_expired(now)` tells you ahead of
  time when Meta reported an expiry.

## Coexistence (merchants keeping the WhatsApp Business app)

Launch with `LaunchOptions::new(id).coexistence()`. The flow finishes with
`FinishKind::WhatsappBusinessAppOnboarding` and only a WABA id; do not add a
PIN (the number is already registered; validation refuses it). When
`onboarded.needs_coexistence_sync()`, call
`merchant.phone_number(pnid).sync_smb_app_data(SmbSyncType::SmbAppStateSync)`
and `…(SmbSyncType::History)` once each within 24 hours, and subscribe to
`history`, `smb_app_state_sync` and `smb_message_echoes`. The inbox does not
record echoes or history yet.

## Pitfalls

- Retrying `onboard`: the second call fails at `exchange_code` and hides
  the real error. Resume instead.
- Using `sessions.consume` and comparing tenants yourself: a wrong tenant
  burns the state. `redeem` checks inside and does not burn it.
- Trusting the event's ids: they are claims. `onboard` verifies them; never
  write the vault from them yourself.
- Logging the code, the posted event, a token or the PIN. Their types
  redact `Debug`; `expose_secret()` does not.
- Leaving the callback's body size unbounded (the example allows 16 KiB).

## Open decisions

Read [OPEN_QUESTIONS.md § Embedded Signup](../../OPEN_QUESTIONS.md#embedded-signup-onboarding-merchants)
before production. Each is a product call; today the code does this:

| # | Question | Today |
| --- | --- | --- |
| 3 | Tech Provider or Solution Partner? | Tech Provider only; no credit-line sharing |
| 4 | Two-step PIN policy | you pass a 6-digit PIN on every `onboard`/`resume`; nothing generates or stores it (the example asks the merchant each time) |
| 5 | Multi-WABA signups | only the claimed (or first, or newest granted) WABA is onboarded |
| 6 | One WABA shared by several tenants | the vault is keyed by WABA; the last onboarding wins |
| 7 | Coexistence sync | flagged by `needs_coexistence_sync()`, not triggered |
| 8 | Token expiry and refresh | expiry recorded, nothing refreshes |
| 9 | Vault key custody and rotation cadence | you supply keys; rotation supported, not scheduled |
| 10 | Resuming after a restart | needs a placeholder code (above) |
| 11 | App-only install, Hosted Embedded Signup | not integrated |
| 12 | Pre-fill shape | follows Meta's worked example; confirm in the Integration Helper |

## Not handled

Solution Partner APIs and credit lines, pre-verified number pools, token
refresh, PIN storage and recovery, your tenant model, and the merchant's
billing setup ([coverage.md](../coverage.md) rows 1 and 28).
