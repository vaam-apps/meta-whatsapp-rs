# WhatsApp OTP login

**Goal:** log users in (or verify a phone number) with a one-time code sent
over WhatsApp, safely: hashed at rest, bound to the exact number, with
attempt and issue limits.

Example: [`otp_login.rs`](../../crates/wa-rs/examples/otp_login.rs) (every
outcome handled; run it with the command in its header). Agent skill:
[`wa-rs-templates-otp`](../../skills/wa-rs-templates-otp/SKILL.md).

## 1. On Meta's side

- Codes go out from **your own** number, with your system user token
  ([getting-started.md](getting-started.md)), never a merchant's.
- If one number sends codes for **several merchants or tenants**, give
  each tenant's `OtpService` its own `OtpConfig::namespace` (the tenant
  id). Services on the same number with the default config share codes: a
  code sent for one tenant verifies at another (section 3).
- Codes must be sent with an **authentication template**. Its text is fixed
  by Meta ("*code* is your verification code."), with an optional security
  line and an optional expiry footer (1–90 minutes), and one of three
  buttons:
  - **copy code**: works everywhere;
  - **one-tap autofill**: Android; your app declares its package name and
    signing-key hash and implements Meta's handshake;
  - **zero-tap**: Android; the code reaches your app without a tap; you must
    accept Meta's zero-tap terms.

  On iOS 26 and later, Meta enables keyboard autofill of the code from the
  notification by default (from 15 June 2026).
- Authentication messages are delivered to the user's **primary** device
  only; linked devices see a prompt instead.
- Templates are business-initiated: they count against your portfolio's
  messaging limit (unique users per 24 hours, 250 for a new portfolio).
  Verify your business before opening sign-up to everyone.
- Meta's advice: confirm the number before sending, and tell users the code
  comes by WhatsApp.

Meta's pages:
[authentication-templates](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/authentication-templates/authentication-templates),
[copy-code](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/authentication-templates/copy-code-button-authentication-templates),
[one-tap](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/authentication-templates/autofill-button-authentication-templates),
[zero-tap](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/authentication-templates/zero-tap-authentication-templates),
[best practices](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/authentication-templates/authentication-best-practices).

## 2. Create the templates

One template per name and language. Create them from a deploy script, not
per request:

```rust
use wa_rs::client::authentication::{AuthenticationTemplate, AuthenticationUpsert, SupportedApp};

let auth = client.authentication(waba_id); // your WABA, your system user token

// Copy code: the simplest.
let copy = AuthenticationTemplate::copy_code("login_code", "en_US")
    .security_recommendation(true)
    .code_expiration_minutes(10); // keep equal to OtpConfig::ttl
auth.create(&copy).await?; // TemplateCreated { id, status, .. }; status is often PENDING

// One-tap for your Android app (package name, signing-key hash).
let one_tap = AuthenticationTemplate::one_tap(
    "login_code_android", "en_US", [SupportedApp::new("com.example.shop", "K8a/AINcGX7")],
)
.code_expiration_minutes(10);
auth.create(&one_tap).await?;

// Zero-tap: `true` states that you accept Meta's zero-tap terms.
let zero_tap = AuthenticationTemplate::zero_tap(
    "login_code_zero", "en_US", [SupportedApp::new("com.example.shop", "K8a/AINcGX7")], true,
);

// The same template in several languages at once.
auth.upsert(&AuthenticationUpsert::from_template(&copy, ["en_US", "fr_FR", "de_DE"])).await?;
```

Only **approved** templates can be sent. Watch
`WebhookEvent::TemplateStatusUpdated` (`update.event` is
`TemplateStatusEvent::Approved` or `Rejected` with `update.reason`) rather
than polling. `auth.previews(&PreviewQuery { .. })` shows Meta's preset text
per language.

## 3. Wire the service

```rust
use std::sync::Arc;
use wa_rs::client::authentication::{OtpConfig, OtpPepper, OtpService, OtpTemplate};
use wa_rs::core::clock::SystemClock;

let otp = OtpService::new(
    wa_rs::client(system_user_token)?, // your number's token
    phone_number_id,                   // the number that sends the codes
    OtpTemplate::new("login_code", "en_US"), // approved, in that language
    kv.clone(),                        // shared KvStore: Postgres or Redis
    Arc::new(SystemClock),
    OtpPepper::new(pepper)?,           // >= 32 random bytes, from your secret manager
    OtpConfig::default(),              // 6 digits, 10 min, 5 attempts, 30 s cooldown, 5 codes/hour
)?;
```

`OtpService` is cheap to clone: build it once. With more than one instance,
the `KvStore` must be shared, or each instance counts attempts and limits on
its own. `new` refuses a bad config with `Error::Config`
(`OtpConfig::validate()` names the field). Codes go out through
`client.messages(…).send(…)`, so the usual send checks apply, and an
unreadable send response is reported without quoting the user's number.

**Codes are scoped to the service.** Every code, cooldown and issue limit is
bound to the sending `phone_number_id` and to `OtpConfig::namespace`, on
top of the user's number and the `purpose`. Several services can share one
store and one pepper (one per brand, each with its own number): a code
sent by one never verifies at another, and their limits never mix. But two services on the **same** number with the default config
(`namespace: None`) share a scope, codes and limits included. So if one
number sends codes for several merchants, tenants or apps, set the
namespace to the tenant id — always
([open question](../../OPEN_QUESTIONS.md#authentication-otp) 34 asks
whether to make it required):

```rust
let config = OtpConfig { namespace: Some("brand-b".into()), ..OtpConfig::default() }; // not blank
```

Upgrading wa-rs from a revision before e40b86f changes every store key once:
codes in flight at the deploy answer `NotFound` (the user asks for a new
one) and the hourly issue limits start again. Deploy outside peak login
hours.

## 4. Numbers must be E.164 with `+`

Meta reads a number without `+` as local to the **sending** number's
country. If codes were keyed by digits alone, a code sent to `12015553931`
from an Indian number (so delivered to `+91 12015553931`) would also verify
`+1 201 555 3931`: the owner of the first number logs in as the second.
So `issue` and `verify` accept only `+` followed by up to 15 digits (spaces,
hyphens and parentheses are ignored), bind the code to exactly those
digits, and send to exactly `+<digits>`.

- Normalize input to E.164 with the country code yourself (a country
  picker, a phone-number library). Never "fix" a refusal by stripping the
  `+`.
- A `wa_id` from a webhook is digits only: prepend `+`.
- A BSUID alone (or a group) is refused locally: authentication templates
  need a phone number (Meta's 131062, `ErrorKind::RecipientNotSupported`).

## 5. Issue and verify: what the user sees

```rust
use std::time::Duration;
use time::OffsetDateTime;
use wa_rs::client::authentication::{IssueOutcome, OtpService, VerifyOutcome};
use wa_rs::prelude::*;

pub enum Screen { EnterCode { expires_at: OffsetDateTime }, Wait(Duration), AskForNumber, TryLater }

pub async fn send_code(otp: &OtpService, phone: &str) -> wa_rs::Result<Screen> {
    let user = Recipient::phone(phone); // "+16505551234"
    Ok(match otp.issue(&user, "login").await {
        Ok(IssueOutcome::Sent(challenge)) => Screen::EnterCode { expires_at: challenge.expires_at },
        Ok(IssueOutcome::CoolingDown { retry_after } | IssueOutcome::RateLimited { retry_after }) => {
            Screen::Wait(retry_after)
        }
        Err(Error::Validation(v)) if v.field == "recipient" => Screen::AskForNumber,
        Err(e) if e.kind() == ErrorKind::RecipientNotSupported => Screen::AskForNumber,
        // A timeout or 5xx: the code may still arrive and stays verifiable. Do not issue again at once.
        Err(_) => Screen::TryLater,
    })
}

pub async fn check_code(otp: &OtpService, phone: &str, typed: &str) -> wa_rs::Result<bool> {
    match otp.verify(&Recipient::phone(phone), "login", typed.trim()).await? {
        VerifyOutcome::Verified => Ok(true), // consumed: create the session now
        VerifyOutcome::Invalid { attempts_left } => { show_wrong_code(attempts_left); Ok(false) }
        VerifyOutcome::Expired | VerifyOutcome::NotFound => { offer_new_code(); Ok(false) }
        VerifyOutcome::TooManyAttempts => { offer_new_code(); Ok(false) } // this code is dead even if right
    }
}
```

| Outcome | Screen |
| --- | --- |
| `IssueOutcome::Sent(challenge)` | code entry, with `challenge.expires_at` |
| `CoolingDown { retry_after }` | "a code is on its way; resend in …" |
| `RateLimited { retry_after }` | "too many codes for this number; try again in …" |
| `Err` validation on `recipient`, or `RecipientNotSupported` | ask for a phone number with country code |
| other `Err` | "the code may still arrive"; allow resend after the cooldown |
| `VerifyOutcome::Verified` | signed in (the code is consumed) |
| `Invalid { attempts_left }` | "wrong code, N tries left" |
| `Expired`, `NotFound`, `TooManyAttempts` | "request a new code" |

Every `verify` call with a live code counts as an attempt, right or wrong,
and is counted atomically before the comparison, so concurrent guesses
cannot exceed the limit. Trim what the user typed.

## 6. Limits, and what they buy

| Setting | Default | Why |
| --- | --- | --- |
| `code_length` | 6 (4–8) | |
| `ttl` | 10 min (≤ 90) | match the template's `code_expiration_minutes` |
| `max_attempts` | 5 per code | |
| `resend_cooldown` | 30 s | stops double taps; does **not** bound guessing |
| `issue_limit` | 5 codes per number and purpose per rolling hour | bounds brute force to about 0.06 % a day at 6 digits |
| `namespace` | `None` | separates tenants or apps that share a sending number; changing it invalidates outstanding codes |

The attacker chooses the victim's number, so the issue limit is per number,
not per IP. `issue_limit: None` is an explicit opt-out for when an
equivalent per-number limit sits in front. The default is a
[pending decision](../../OPEN_QUESTIONS.md#authentication-otp) (13). The
`purpose` argument (`"login"`, `"reset_password"`) keeps flows apart.

## 7. Pepper custody

- The pepper keys every HMAC: store keys (over the sending number, the
  namespace, the user's number and the purpose) and code hashes. No phone
  number and no code is stored (namespaces `wa.otp` and `wa.otp.rate`).
- At least 32 random bytes (`openssl rand -base64 32`), kept **outside** the
  database that holds the challenges: with both, a 6-digit code falls to
  10⁶ guesses offline.
- Rotating it invalidates outstanding codes and resets issue limits. Who
  holds it and when it rotates is [open question](../../OPEN_QUESTIONS.md#authentication-otp) 14.

## Pitfalls

- A template's language code must be the one it was approved in.
- Retrying `issue` in a loop after an error: the first code may already be
  on the phone, and every call takes a slot of the hourly limit.
- Sending a code in a text message, a non-authentication template, an image
  or a PDF. Only `OtpService` builds the authentication payload; never
  render a code into media.
- `MemoryKvStore` with several instances.

## Not handled

Creating the session or JWT after `Verified`, account linking, SMS fallback
when WhatsApp cannot deliver (`ErrorKind::Undeliverable`, 131026), per-IP
or per-device throttling in front of `issue`, and the Android side of
one-tap and zero-tap.
