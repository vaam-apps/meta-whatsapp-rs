---
name: meta-whatsapp-rs-otp-login
description: "WhatsApp OTP login and phone verification with meta-whatsapp-rs - creating the authentication template (copy code, one-tap, zero-tap), OtpService issuing and verifying one-time passcodes (IssueOutcome, VerifyOutcome), why recipients must be E.164 with a plus sign, the per-number issue limit, resend cooldown and attempt limits, the required OtpConfig::namespace (the tenant), the pepper and store custody, and what to do after a timeout. Load when building sign-in, sign-up, password reset or phone-number verification with WhatsApp codes."
---

# meta-whatsapp-rs-otp-login

> **Verified against meta-whatsapp-rs 6d04f3da9c504cffac32f7dbe05869adcaf1957e (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/otp.rs](examples/otp.rs), compiled and tested by
meta-whatsapp-rs's own gate (issue → verify once, expiry with a `ManualClock`,
refused numbers). Runnable program:
[`otp_login.rs`](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/crates/meta-whatsapp-rs/examples/otp_login.rs).

## When to use

Proving a user controls a WhatsApp number: login, sign-up, reset,
changing a number. Module `meta_whatsapp_rs::client::authentication`.

## 1. The authentication template, once per language

```rust
let template = AuthenticationTemplate::copy_code("login_code", "en_US")
    .security_recommendation(true)
    .code_expiration_minutes(10); // keep equal to OtpConfig::ttl
client.authentication(waba_id).create(&template).await
```

Meta fixes the text ("*{{1}}* is your verification code.").
`AuthenticationTemplate::one_tap(name, language, apps)` and
`AuthenticationTemplate::zero_tap(name, language, apps, terms_accepted)`
add Android autofill (`SupportedApp::new(package_name, signature_hash)`).

## 2. One service per sending number and tenant

```rust
OtpService::new(
    client,
    phone_number_id,
    OtpTemplate::new("login_code", "en_US"), // must be APPROVED
    kv,
    Arc::new(SystemClock),
    OtpPepper::new(pepper)?,
    OtpConfig::new(tenant), // 6 digits, 10 min, 5 attempts, 30 s, 5/hour
)
```

The namespace (your tenant id) is required: `OtpConfig` has no `Default`.
Other settings take struct update syntax:

```rust
OtpConfig {
    code_length: 8,
    ..OtpConfig::new(tenant)
}
```

`OtpService::new` checks the config (`OtpConfig::validate()` names the
field: `code_length` 4–8, `ttl` up to 90 minutes, `max_attempts`,
`issue_limit`, a blank `namespace` or one with edge whitespace, control
or format characters: U+200B, U+FEFF, bidi controls) as `Error::Config`.

## 3. Issue, then verify

```rust
let user = Recipient::phone(user_input); // "+16505551234": never strip the `+`
match otp.issue(&user, "login").await {
    Ok(IssueOutcome::Sent(_challenge)) => Ok(Login::CodeSent),
    Ok(
        IssueOutcome::CoolingDown { retry_after } | IssueOutcome::RateLimited { retry_after },
    ) => Ok(Login::Wait(retry_after)),
    // Not `+<digits>`, or a BSUID: refused before anything is stored or sent.
    Err(Error::Validation(v)) if v.field == "recipient" => {
        Ok(Login::NotAWhatsAppNumber(v.reason))
    }
    Err(e) => Err(e), // after a timeout or 5xx the code may have arrived: do not re-issue at once
}
```

```rust
Ok(match otp.verify(&user, "login", typed.trim()).await? {
    VerifyOutcome::Verified => Login::SignedIn, // consumed: single use
    VerifyOutcome::Invalid { attempts_left } => Login::WrongCode { attempts_left },
    VerifyOutcome::Expired | VerifyOutcome::NotFound | VerifyOutcome::TooManyAttempts => {
        Login::RequestANewCode
    }
})
```

`purpose` (`"login"`, `"reset_password"`, …) separates flows for one
number. It and the namespace are constants of your code or tenant table,
never request input. `verify` compares byte for byte: trim input. Every
call with a live code counts as an attempt, counted atomically first.

## Why E.164 with `+` is mandatory

Meta prepends **the sending number's country code** to a number without
`+`. Keyed by digits alone, a code sent to `12015553931` (delivered to
`+91 12015553931` from an Indian number) would verify `+12015553931`: the
owner of the first number logs in as the second. So `issue`/`verify`
accept only `+` and up to 15 digits (spaces, `-`, `(`, `)` ignored), bind
the code to those digits and send to exactly `+<digits>`. Normalize input
with the country code yourself (a country picker, a phone library); from
a webhook `wa_id`, prepend `+`. A BSUID or group is refused locally:
authentication templates cannot go to a BSUID (Meta's 131062).

## Several services on one store: namespaces

Codes, cooldowns and issue limits are bound to the sending
`phone_number_id` and `OtpConfig::namespace`, on top of the digits and
the purpose: one store and one pepper serve any number of services, and
tenants sharing a number never see each other's codes. Changing a
namespace invalidates outstanding codes.

Upgrades (codes in flight answer as said, once): ~~`OtpConfig::namespace`
is an `Option`~~: until d67b3ac; `Some(ns)` keeps its keys with
`OtpConfig::new(ns)`, a `None` service must pick one (`NotFound`). ~~Codes
were keyed by the pepper, the digits and the purpose only~~: until e40b86f
(`NotFound`, limits restart). ~~The code hash left out the store key~~ (a
store writer could copy their record over another key): until 8238853
(2026-09-24; `Invalid`). ~~A namespace with edge whitespace, control or
format characters works~~: until 8238853 (format characters: 7e4801f,
2026-09-25). Breaking: such a service fails `OtpService::new`, and fixing
the namespace changes its keys (`NotFound`, limits restart).

## Pitfalls

- **Limits**: 5 attempts per code; 30 s between codes
  (`CoolingDown`); 5 codes per number, purpose and rolling hour
  (`IssueLimit::DEFAULT`, `RateLimited`) — what bounds brute force
  (≈ 0.06 %/day at 6 digits). `issue_limit: None` is an explicit opt-out
  only for when an equivalent per-number limit sits in front. The
  attacker picks the victim's number: limit per number, not per IP.
- **`issue` returning `Err` after a timeout or 5xx**: the code may have
  been delivered and stays verifiable; the cooldown applies. Tell the user
  to wait; never call `issue` in a loop.
- **Pepper**: at least 32 random bytes (e.g. `openssl rand -base64 32`),
  kept **outside** the database that holds the challenges (a dump plus the
  pepper brute-forces 10⁶ codes). Rotating it invalidates outstanding codes.
- **Store**: shared (Postgres or Redis with `noeviction`) as soon as two
  instances serve logins; namespaces `wa.otp` and `wa.otp.rate`.
- Keep `code_expiration_minutes` equal to `OtpConfig::ttl`.
- Never log, render or email the code; never send it in a plain text
  message or another template (`otp_template_message` is the exact payload
  `OtpService` sends).

## What meta-whatsapp-rs does not do

- No session or JWT after `Verified`, no account linking, no SMS fallback
  when WhatsApp is undeliverable (`ErrorKind::Undeliverable`, 131026), no
  per-IP or per-device throttle in front of `issue`.
- It does not choose the namespace: your tenant model does. The default
  issue limit and pepper custody are open
  ([open questions 13, 14](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/OPEN_QUESTIONS.md#authentication-otp)).

## Related skills

`meta-whatsapp-rs-templates`, `meta-whatsapp-rs-storage` (the shared store), `meta-whatsapp-rs-testing`
(`ManualClock`, capturing the sent code), `meta-whatsapp-rs-errors`, `meta-whatsapp-rs-production`
(pepper custody).
