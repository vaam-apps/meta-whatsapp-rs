# Embedded Signup: merchants connect their own number

**Goal:** a merchant of your CMS clicks "Connect WhatsApp", goes through
Meta's popup, and your backend ends up holding their business token,
encrypted, routable by phone number id, with webhooks flowing and the
number registered.

wa-rs implements Embedded Signup v4 for a **Tech Provider** (each merchant
adds a payment method and pays Meta) or a **Solution Partner** (your
credit line pays for every merchant you onboard), chosen once per
deployment: see [Solution Partner mode](#solution-partner-mode).
Example: [`embedded_signup.rs`](../../crates/wa-rs/examples/embedded_signup.rs),
a **Tech Provider** server (it calls plain `onboard`, which Solution Partner
mode refuses); the Solution Partner flow is in the skill's
[`solution_partner.rs`](../../skills/wa-rs-embedded-signup/examples/solution_partner.rs).
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
                                                                                      subscribe app,
                                                                                      [share credit line],
                                                                                      register
```

## 1. On Meta's side

| # | Step | Meta's page |
| --- | --- | --- |
| 1 | Become a Tech Provider: verify your business, then pass App Review for **Advanced access** to `whatsapp_business_messaging` and `whatsapp_business_management`. Without it, merchants cannot grant your app those permissions in the popup, and calls on their WABAs fail with Graph error `200`. | [get-started-for-tech-providers](https://developers.facebook.com/documentation/business-messaging/whatsapp/solution-providers/get-started-for-tech-providers), [permissions](https://developers.facebook.com/documentation/business-messaging/whatsapp/permissions) |
| 2 | Facebook Login for Business → Settings → Client OAuth settings: switch on client OAuth login, web OAuth login, enforce HTTPS, embedded browser OAuth login, strict mode for redirect URIs and login with the JavaScript SDK. List every domain that serves the connect page (development ones too, HTTPS only) under **Allowed domains** and **Valid OAuth redirect URIs**; otherwise the popup never hands the code back. | [embedded-signup/implementation](https://developers.facebook.com/documentation/business-messaging/whatsapp/embedded-signup/implementation) |
| 3 | Facebook Login for Business → Configurations: create a configuration from Meta's WhatsApp Embedded Signup template (or a custom one using the WhatsApp Embedded Signup login variation). Ask only for the assets you use: every extra screen loses merchants. Keep the **configuration id**. | same |
| 4 | Configure your app's webhook callback ([webhooks.md](webhooks.md)) and subscribe to `messages` and `account_update` (Meta requires the latter for Embedded Signup). | [webhooks/overview](https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/overview) |
| 5 | Tech Provider: tell each merchant to add a payment method in WhatsApp Manager after connecting; until then their number cannot send. A Solution Partner shares its credit line instead ([below](#solution-partner-mode)). | [onboarding-customers-as-a-tech-provider](https://developers.facebook.com/documentation/business-messaging/whatsapp/embedded-signup/onboarding-customers-as-a-tech-provider) |

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
    // Tech Provider. A Solution Partner deployment refuses plain `onboard`
    // (before spending the code): call `es.onboard_with_approval(&request,
    // vault, |verified| …)` and reserve the WABA for `merchant_id` in it.
    match es.onboard(&request, vault).await { // the code is spent: never call this twice
        Ok(onboarded) => {
            save_merchant_waba(merchant_id, &onboarded.waba_id).await; // your tenant ↔ WABA table
            Ok(Completed::Connected(onboarded))
        }
        // Every step after store_token: the token is kept, `resume` redoes them.
        Err(error @ Error::Step {
            step: steps::SUBSCRIBE_APP | steps::ASSIGN_SYSTEM_USER | steps::SHARE_CREDIT_LINE
                | steps::REGISTER_PHONE,
            ..
        }) => {
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
| `approve` | `onboard_with_approval` (required for a Solution Partner) and `resume_with_approval`: your check of what Meta verified, recorded in the credit ledger in Solution Partner mode | nothing was stored, subscribed or shared; the merchant starts again if you allow it. A Solution Partner's `resume` fails here when no approval is recorded: `resume_with_approval` once |
| `store_token` | encrypts the token into the vault, indexes every number of the WABA | fix the store, start over |
| `subscribe_app` | `POST /{waba}/subscribed_apps` | fix, then `resume` |
| `assign_system_user` | Solution Partner, share-and-attach only: your system user on the merchant's WABA | fix, then `resume` |
| `share_credit_line` | Solution Partner only: checks, then shares your credit line | fix, then `resume` (it checks before it posts); a business whose line was revoked is refused unless the request opts in |
| `register_phone` | `POST /{number}/register` with the PIN | fix (often the PIN), then `resume` |

The token is stored **before** the steps after it on purpose: a wrong PIN
(`ErrorKind::TwoStepVerification`, 133005) must not cost the merchant the
whole popup again. Registration counts against 10 per 72 hours; 133016
locks the number for 72 hours and is never retried automatically.

**Checks on the verified ids belong before the store.** To refuse a WABA
that another of your merchants already connected (or a business you do
not serve), use `es.onboard_with_approval(&request, &vault, |verified| async move { … })`
instead of `onboard`: the closure receives the `VerifiedOnboarding` (WABA,
owner business, numbers, all checked with Meta) right after
`verify_assets`, and an error from it stops the flow at step `approve`
with nothing stored, subscribed or shared. Reserve the WABA for the
merchant in **one atomic write** there (`put_if_absent`, an insert on a
unique key): with a lookup then a write, two merchants onboarding the same
WABA at once both pass. Checking after `onboard` returns is too late for a
Solution Partner: the credit line is attached by then, and an attached
line cannot be taken back from the WABA, which is why a Solution Partner
deployment refuses plain `onboard`. Which merchant may have a WABA is your
policy: wa-rs decides none ([open decision](#open-decisions) 6).

## 6. Resume

```rust
// `resume` acts with whatever token is stored for the WABA you name:
// check that it belongs to the calling merchant first.
if !merchant_owns_waba(&merchant_id, &waba_id).await { return Err(forbidden()) }
let request = request.register_with_pin(TwoStepPin::new(corrected_pin)?);
let done = es.resume(&waba_id, &request, &vault).await?; // the steps after store_token only
```

A Solution Partner's `resume` shares the credit line only for a WABA whose
approval the ledger records for the token record now stored
(`onboard_with_approval` records it for the token it stores); for a token
stored without one, or stored again since, it fails at step `approve`
before any request: call
`es.resume_with_approval(&waba_id, &request, &vault, |verified| …)` once,
with the same tenant check as at onboarding.

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
  default; turn it off on read-only replicas), tokens and the Solution
  Partner credit ledger alike. The vault cannot list its records, so walk
  *your* merchant table calling `vault.rotate(&waba_id)` for **every WABA
  you ever onboarded, offboarded ones included** (a Solution Partner's
  credit ledger outlives the token, and `rotate` re-seals it and the
  revocation marker of the business it names), plus
  `vault.rotate_business(&business_id)` for each business you revoked by
  business id alone (`revoke_business_credit_line`). Only then drop the old
  key: a record still under it fails with `CryptoError::InvalidKey`, and a
  Solution Partner's onboarding and `resume` of that merchant with it.
  Revocation goes on with what it can read: an unreadable token or credit
  record is skipped (the business then comes from the other one, or from
  the webhook's `owner_business_id`) and an unreadable revocation marker
  is replaced, but the allocation and any pending share recorded in an
  unreadable credit record are not seen (only what Meta's lookup lists is
  revoked), and with nothing readable naming the business the
  `InvalidKey` error is what comes back.
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

- **The merchant disconnects in your CMS:** while their token still
  works, stop the webhooks with it
  (`client.with_token(stored.token).waba(stored.waba_id.clone()).unsubscribe_app().await?`),
  then `es.offboard(&waba_id, None, &vault).await?`, then your own
  mapping. `offboard` deletes the token and its phone index; a Solution
  Partner's `offboard` revokes the credit line **first** and deletes
  nothing if that fails.
- **Meta tells you:** `WebhookEvent::AccountUpdated`, with the merchant's
  WABA in `event.waba_id()` (for the `Partner*` events wa-rs takes it from
  `waba_info.waba_id`: Meta's entry id there is a business portfolio, kept
  as `entry_id`). On `AccountUpdateEvent::PartnerAppUninstalled` whose
  `waba_info.partner_app_id` is **your** app id (`es.app().app_id`; under a
  Multi-Partner Solution the other partners' uninstalls reach you too), the
  business removed your app: call `es.offboard(waba_id, owner, &vault)`,
  with `owner` the event's `waba_info.owner_business_id`; on
  `AccountDeleted`, `vault.delete`. `AccountOffboarded`, and
  `PartnerRemoved` with disconnection details, concern coexistence numbers
  that changed device or number: ask the merchant to reconnect.
  `PartnerRemoved` without them means the merchant unshared the WABA.
  Either way a Solution Partner revokes its credit line **at once**
  ([below](#solution-partner-mode); by business with
  `revoke_business_credit_line` when the event names no WABA), unless the
  event's `waba_info.solution_partner_business_ids` (sent under a
  Multi-Partner Solution only) does not list its own business: that
  removal is from a solution it is not in. A merchant who reconnects
  onboards again, and funding them again takes
  `.reshare_after_revocation()`. Pass `owner_business_id` only from a
  delivery whose signature was checked.
- **The token stops working:** `ErrorKind::Authentication` (190) on a
  merchant's calls. Nothing refreshes tokens; the merchant runs Embedded
  Signup again. `StoredBusinessToken::is_expired(now)` tells you ahead of
  time when Meta reported an expiry.

## Solution Partner mode

A Solution Partner pays Meta for its merchants through its own credit
line (and invoices them); wa-rs shares that line with every merchant it
onboards. You are liable to Meta for every message sent on a shared line,
and a line cannot be changed or taken back from a WABA once attached. It
is one choice per deployment (the owner's decision on 2026-09-24):
configure it at startup, or leave it out for the Tech Provider flow, whose
requests are unchanged.

On Meta's side: Solution Partner status and a credit line (its id:
`client.with_token(system_token).credit_lines().list(&your_business_id, &[])`);
a system user with the business_management permission and an Admin or
Financial Editor role on your portfolio, its token and its id.

```rust
use wa_rs::client::credit_lines::WabaCurrency;
use wa_rs::client::embedded_signup::{CreditSharing, SolutionPartner};

let es = client
    .embedded_signup(AppCredentials::new(app_id, app_secret))
    .solution_partner(
        SolutionPartner::new(system_token, system_user_id, credit_line_id)
            .method(CreditSharing::ShareAndAttach) // Meta's current method, the default
            .default_currency(WabaCurrency::Usd), // AUD, EUR, GBP, IDR, INR or USD
    );
// Per merchant, from your billing records (never the browser): checked
// before the code is exchanged.
let request = request.currency("EUR".parse::<WabaCurrency>()?);
// Required: plain `onboard` is refused. Reserve the WABA for the merchant
// atomically in the approval, before anything is stored or shared (§5).
let done = es.onboard_with_approval(&request, &vault, |verified| async move {
    reserve_for_merchant(&verified.waba_id, merchant_id).await // put_if_absent, not a lookup
}).await?;
```

**The approval is required.** A Solution Partner deployment refuses plain
`onboard` before the code is exchanged (`CreditError::ApprovalRequired`),
because the approval is the last point where a line can still be kept from
a WABA. `onboard_with_approval` records the approval in the vault's credit
ledger (`StoredCredit::approved_at`), bound to the token record it stores
(`StoredCredit::approved_token_created_at`), and `resume` shares only for a
WABA approved so: a token stored without one (onboarded in Tech Provider
mode before the deployment switched, by an older wa-rs, or stored again
since, e.g. by a Tech Provider onboarding after an offboard) fails
`resume` at step `approve` until `resume_with_approval` approves it once.
The approval stays required (the owner confirmed it on 2026-09-25).
Which merchant may have a WABA stays your policy (open decision 6).

`onboard_with_approval` then runs, following Meta's Solution Partner order
(subscribe, share the credit line, register):

| After `subscribe_app` | Request | Token |
| --- | --- | --- |
| `assign_system_user` (share-and-attach only) | `POST /{waba}/assigned_users?user=<system user>&tasks=["MANAGE"]` | your system user's |
| `share_credit_line`: the check | `GET /{credit line}/owning_credit_allocation_configs?receiving_business_id=<owner>`; per record found (and the one recorded in the vault) `GET /{allocation}?fields=receiving_business,request_status`, `GET /{allocation}?fields=receiving_credential`; `GET /{waba}?fields=primary_funding_id` | yours; the merchant's for `primary_funding_id` |
| `share_credit_line`, share-and-attach | `POST /{credit line}/whatsapp_credit_sharing_and_attach?waba_currency=…&waba_id=…` | your system user's |
| `share_credit_line`, share-then-attach | `POST /{credit line}/whatsapp_credit_sharing?receiving_business_id=<owner>` (only when no active record exists), then `POST /{credit line}/whatsapp_credit_attach?waba_currency=…&waba_id=…` | yours, then the **merchant's** for the attach |

- The owner is the business Meta reports for the WABA
  (`owner_business_info`, read in `verify_assets`), never the
  `business_id` the browser sent.
- **The currency** must be the merchant's billing currency, from your
  records: it sets what Meta charges you, and a credit line cannot be
  changed once attached (only a new WABA gets another one). Without one on
  the request or as the default, `onboard` refuses before spending the
  code; a Tech Provider request naming one is refused too. The first
  currency is sealed in the vault before the first share is posted; a
  `resume` or a later onboarding of the WABA naming another is refused.
- **It checks before it posts.** When an active record (no
  `request_status`: Meta documents only `DELETED`) has the WABA's
  `primary_funding_id` as its receiving credential, nothing is posted, so
  onboarding a WABA that is already funded posts nothing. A share whose
  answer is lost (a timeout, a 5xx: it may have gone through) is
  `CreditError::Reconcile`, not retryable, so no automatic retry posts it
  again; a later `resume` checks first, and posts again only when no
  record funds the WABA and its `primary_funding_id` is empty. That
  assumes Meta shows a share in both as soon as it applied it, which it
  does not document (and the two-call method's share alone funds
  nothing, so for it only the lookup tells). The ledger flags each post
  until its allocation is recorded (`StoredCredit::pending_share`); a
  flagged share that no record explains, on a WABA something funds, is
  `CreditError::Reconcile`: check in Meta Business Suite before anything
  else (one that is not there, an operator clears: `clear_pending_share`,
  below). A two-call share whose attach Meta refused is
  `CreditError::AttachFailed`: the share went out and is recorded, and
  `resume` attaches it without sharing again. Without the owner business
  (`owner_business_info`, which Meta's docs show on every WABA) nothing
  can be checked or revoked, so nothing is shared
  (`CreditError::OwnerUnknown`).
- **One onboarding at a time.** The credit step holds a per-WABA lease,
  renewed right before each post; a second onboarding of the WABA, or one
  whose lease a slow step outlived, posts nothing more and is
  `CreditError::Busy` (`EmbeddedSignup::is_credit_step_busy`, retryable:
  resume later). Its `posted` is `true` when the two-call method had
  already shared (and recorded) the line before it lost the lease: the
  attach is what a later `resume` does.
- **A revoked business stays revoked.** When `revoke_credit_line` marked
  the business revoked, or Meta reports only `DELETED` records for it,
  `onboard_with_approval` and `resume` refuse (`CreditError::Revoked`,
  `EmbeddedSignup::is_credit_line_revoked`) unless the request says
  `.reshare_after_revocation()`; a record whose `request_status` Meta does
  not document is refused the same way (`CreditError::StatusUnknown`).
  Funding a merchant again is your decision, taken per onboarding. A
  revocation that runs while a share is posted ends with the line revoked
  or the share reported: the revocation writes its marker before it looks
  anything up, and the share reads the marker after its post, also when
  the post's answer was lost. A share that finds a new or changed marker
  revokes by business what it may have made: `CreditError::Revoked` with
  `posted` once that is revoked, else `CreditError::Reconcile`, with the
  share kept pending in the ledger. The revocation, for its part, reports
  a WABA whose share is pending and of which it revoked nothing as
  `RevocationIncomplete` with `share_pending` (retryable), never as done,
  and while the share is pending a later `onboard_with_approval` or
  `resume` of that business answers `Reconcile`, not `Revoked`.
- Every refusal is a `CreditError` (`err.credit()`), with its own
  `is_retryable()` and `may_have_been_sent()`; branch on those, never on
  the text.
- The allocation comes back as `Onboarded::allocation_config_id` and is
  recorded, with the owner and the currency, in the vault's credit ledger
  (`vault.credit(&waba_id)`), which `vault.delete` leaves in place.
- Under a Multi-Partner Solution without messaging permission, `MANAGE`
  is refused on the merchant's WABA: set
  `SolutionPartner::system_user_tasks` to granular tasks including
  `WabaTask::ManageBilling`.

**When a merchant removes you** (`AccountUpdateEvent::PartnerRemoved`):
messaging on the WABA is blocked, its owner can no longer be read, and
Meta recommends revoking the credit line at once:

```rust
let waba_id = event.waba_id().ok_or(…)?; // waba_info.waba_id for Partner* events
let owner = update.waba_info.as_ref().and_then(|i| i.owner_business_id.as_ref());
let report = es.revoke_credit_line(waba_id, owner, &vault).await?; // report.revoked, report.already_revoked
```

- It marks the business revoked in the vault first (replacing a marker it
  cannot read: a marker only makes onboarding stricter), before looking
  anything up, then revokes every active record of your line naming the
  business, plus the allocation recorded at onboarding, confirming each
  with `request_status`; records already `DELETED` are skipped, one naming
  another business is never touched, and one naming no business is
  reported rather than revoked blindly. Every record is tried, and a
  marker that cannot be written does not stop the `DELETE`s. What is left
  undone is `CreditError::RevocationIncomplete`, carrying the report and
  the failed, unconfirmed and unattributed records, `share_pending` (the
  WABA's ledger shows a share whose outcome is unknown and this call
  revoked no record: call again; if it keeps revoking nothing, check the
  WABA's funding) and `ledger` (a ledger write that failed; a repeat
  writes it again): repeat the call when it `is_retryable()`, otherwise
  check those records in Meta Business Suite. Safe to repeat.
- The owner comes from what onboarding recorded (the token record, else
  the credit ledger, else the business Meta's own record of the recorded
  allocation names, which is then written into the ledger); the
  webhook's `owner_business_id` is used only when none of these exists,
  and only if your line has records naming it (so an id that is not your
  customer's marks nothing; when that check fails, it is marked once the
  revocation's own lookup finds records naming it). If it contradicts the
  record nothing is revoked.
- **Revocation is per business**: every WABA of that business shared with
  you loses the line. `es.revoke_business_credit_line(&business_id,
  &vault)` revokes from a business id alone, for a `PartnerRemoved` that
  names no WABA.
- **Revoke at once on every `PartnerRemoved` of your solution**,
  including a coexistence one (with `disconnection_info`: a number that
  changed device, was re-registered or went inactive, and may reconnect).
  That is the owner's decision for wa-rs (2026-09-25), and what Meta
  recommends for any removal; no grace period. wa-rs stays passive:
  nothing revokes unless your handler calls `revoke_credit_line`. A
  merchant who reconnects runs Embedded Signup again, and the revoked
  business is refused (`CreditError::Revoked`) until that onboarding says
  `.reshare_after_revocation()`: funding them again is your decision, per
  onboarding. The skill's example returns `PartnerAction::Disconnected`
  for a coexistence removal so you can ask the merchant to reconnect, and
  its `reconnect` onboards with the opt-in.
- **A share whose answer was lost and that Meta never lists** keeps every
  revocation of the WABA at `share_pending`, and `offboard` from deleting
  the token. When someone has checked the WABA's funding in Meta Business
  Suite and the share is not there, clear it:
  `es.clear_pending_share(&waba_id, cleared_by, None, &vault)`. It posts
  nothing and holds the WABA's credit lease (`CreditError::Busy` while a
  share runs). It checks Meta first: your line's records for the owner
  business and the recorded allocation, each with its `request_status`,
  and the WABA's `primary_funding_id` (with the merchant's stored token).
  An active record, one whose status Meta does not document, or one the
  lookup returns naming no business clears nothing
  (`PendingShareClearance::NotCleared`, with what was found; a record
  funding the WABA is recorded as its allocation, which the next
  revocation revokes). Any active record of the business stops it, also
  one funding another of its WABAs: Meta does not say which WABA a record
  funds. A `primary_funding_id` that no record explains stops it too
  (`SharesFound::unexplained_funding`): it may be the lost share itself,
  applied before Meta's lookup lists it, and wa-rs cannot tell it from
  the merchant's own card. Look at what pays for the WABA in Meta
  Business Suite; only when it is not your credit line, call again with
  that id as the third argument. Otherwise the flag is cleared and who
  cleared it, when, and the acknowledged funding are sealed in the ledger
  (`StoredCredit::cleared_shares`); revocation and `offboard` then behave
  as if nothing had been posted. It refuses when nothing is pending.
  Meta documents no delay after which a share that went through is
  listed: let time pass since the pending share before clearing it.
- `es.offboard(&waba_id, owner, &vault)` revokes the same way and then
  deletes the token (§9). The credit ledger outlives the token, so
  `PartnerAppUninstalled` and `PartnerRemoved` end with the line revoked
  in either order. When nothing names the business and the ledger shows
  nothing was ever shared, there is nothing to revoke and the token is
  deleted; when the ledger shows a share that revocation cannot find (or
  cannot be read), the token is kept (`CreditError::Reconcile`): check in
  Meta Business Suite, then `vault.delete` it yourself. A pending share
  the revocation revoked nothing for keeps the token too
  (`RevocationIncomplete` with `share_pending`: call `offboard` again, or
  clear the share as above). A business recorded for the WABA is marked
  revoked even when nothing was ever shared with it (a WABA onboarded in
  Tech Provider mode, say): the marker comes before any lookup, which is
  what stops a racing share from surviving. The owner decided to keep
  that on 2026-09-25; the cost is that onboarding that business in
  Solution Partner mode later takes `.reshare_after_revocation()`.

The lower-level calls (`CreditLines::share`, `attach`,
`receiving_credential`, `primary_funding`, `allocations_for`,
`revoke_for_business`, `allocation_status`) are in
`wa_rs::client::credit_lines`, with the token each needs in their rustdoc.

Not settled by Meta's pages (the code's choice in brackets): whether the
share-then-attach method also needs the system user on the WABA [not
added]; whether adding the system user again is harmless [repeated on
`resume`]; whether the lookup lists revoked records [each record's
`request_status` is read]; whether a business can attach a line shared
with it to other WABAs itself [not controlled: reconcile your credit line
invoice against the WABAs you onboarded]; whether the lookup lists a
record, and the WABA's `primary_funding_id` shows it, as soon as its share
returns [assumed: `resume` after a lost answer posts again when neither
shows anything, and a share that raced a revocation and lost its answer
is found by the lookup; were either to lag, a share could be posted
twice, or survive, which is why a lost answer is `Reconcile` and a
pending share the revocation revoked nothing for keeps it incomplete];
whether a `PARTNER_REMOVED` about another partner can reach your app [the
skill's example ignores one whose `solution_partner_business_ids` does not
list your business; Meta sends that list only under a Multi-Partner
Solution, and does not say whose business the entry id is].

## Coexistence (merchants keeping the WhatsApp Business app)

Launch with `LaunchOptions::new(id).coexistence()`. The flow finishes with
`FinishKind::WhatsappBusinessAppOnboarding` and only a WABA id; do not add a
PIN (the number is already registered; validation refuses it). When
`onboarded.needs_coexistence_sync()`, call
`merchant.phone_number(pnid).sync_smb_app_data(SmbSyncType::SmbAppStateSync)`
and `…(SmbSyncType::History)` once each within 24 hours, and subscribe to
`history`, `smb_app_state_sync` and `smb_message_echoes`. `InboxSink`
records the echoes and the synced history in the merchant's conversations
([CMS inbox guide](cms-inbox.md#coexistence-the-merchant-also-uses-the-whatsapp-business-app)).

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
before production. Each is a product call; today the code does this
(#3, Tech Provider or Solution Partner, was decided on 2026-09-24: both,
one per deployment; on 2026-09-25 the owner decided to revoke at once on
every removal, to keep the approval required, to let an operator clear a
lost share, and to keep marking a business on offboarding, all above):

| # | Question | Today |
| --- | --- | --- |
| 4 | Two-step PIN policy | you pass a 6-digit PIN on every `onboard`/`resume`; nothing generates or stores it (the example asks the merchant each time) |
| 5 | Multi-WABA signups | only the claimed (or first, or newest granted) WABA is onboarded |
| 6 | One WABA shared by several tenants | the vault is keyed by WABA; the last onboarding wins unless your `onboard_with_approval` refuses it (required for a Solution Partner, whose approval is recorded; the policy is still yours) |
| 7 | Coexistence sync | flagged by `needs_coexistence_sync()`, not triggered |
| 8 | Token expiry and refresh | expiry recorded, nothing refreshes |
| 9 | Vault key custody and rotation cadence | you supply keys; rotation supported, not scheduled |
| 10 | Resuming after a restart | needs a placeholder code (above) |
| 11 | App-only install, Hosted Embedded Signup | not integrated |
| 12 | Pre-fill shape | follows Meta's worked example; confirm in the Integration Helper |

## Not handled

Solution Partner APIs other than credit lines (partner-led business
verification, Multi-Partner Solutions, migration), pre-verified number
pools, token refresh, PIN storage and recovery, your tenant model, and a
Tech Provider merchant's billing setup ([coverage.md](../coverage.md) rows
1 and 28).
