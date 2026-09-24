---
name: wa-rs-templates-otp
description: "WhatsApp message templates and OTP login with wa-rs - template lifecycle (create, list, get, edit, delete; categories; positional vs named parameters; media header handles; status and quality webhooks) and one-time passcodes over authentication templates with OtpService (why recipients must be E.164 with a plus sign, IssueOutcome and VerifyOutcome, attempt limits, resend cooldown, per-number issue limit, codes scoped to the sending number and an optional namespace, pepper custody). Load when defining or managing templates, or building phone-number login or verification."
---

# wa-rs-templates-otp

> **Verified against wa-rs 7940d15 (2026-09-24).** On another revision, trust
> the code over this page (see `skills/README.md`).

Modules: `wa_rs::client::templates` (definitions and management) and
`wa_rs::client::authentication` (authentication templates, `OtpService`).
Sending a template with parameters is in `wa-rs-messaging`.

**Two builder families — do not mix them up:** a *definition*
(`TemplateDefinition`, `TemplateComponent`, `Button`) is what a template **is**,
sent to create/edit it; an *invocation* (`TemplateMessage`, `Parameter`) fills
its placeholders when **sending**.

## Template lifecycle

```rust
use wa_rs::client::templates::{
    Button, ParameterFormat, TemplateCategory, TemplateComponent, TemplateDefinition,
    TemplateListQuery, TemplateStatus,
};

let templates = client.templates(waba_id); // merchant's WABA: client.with_token(..)
let created = templates.create(
    &TemplateDefinition::new("order_update", "en_US", TemplateCategory::Utility)
        .component(TemplateComponent::body_positional(
            "Hi {{1}}, order {{2}} has shipped.", ["Pablo", "860198"])) // one example per placeholder
        .component(TemplateComponent::buttons([Button::url_with_example(
            "Track", "https://shop.example/track/{{1}}", "860198")])),
).await?;                                  // created.id, created.status (often PENDING)

let named = TemplateDefinition::new("welcome", "en_US", TemplateCategory::Marketing)
    .parameter_format(ParameterFormat::Named)          // REQUIRED for {{first_name}}
    .component(TemplateComponent::body_named("Hi {{first_name}}!", [("first_name", "Pablo")]));

let approved = templates.list(&TemplateListQuery::new().status(TemplateStatus::Approved)).await?;
```

- `create` validates locally first (`TemplateDefinition::validate`, which you
  can also call yourself; like every public `validate()` it returns
  `Result<(), ValidationError>`): names,
  lengths, button counts, placeholders **against the declared format**.
  Omitted format = positional; `{{first_name}}` without
  `.parameter_format(ParameterFormat::Named)` is a `Validation` error.
  Positional placeholders must read `{{1}}, {{2}}, …` in order, and every
  placeholder needs an example.
- Categories: `Utility` (follow-ups to a user action: order updates),
  `Marketing` (anything promotional or mixed), `Authentication` (OTP only; use
  `AuthenticationTemplate`, below). Meta may re-categorize.
- Only `APPROVED` templates can be sent. Approval is asynchronous: watch
  `WebhookEvent::TemplateStatusUpdated` (`update.event`:
  `TemplateStatusEvent::Approved`, `Rejected` with `update.reason`, `Paused`,
  `Disabled`, …; `update.message_template_id`) rather than polling. Quality
  and category changes: `TemplateQualityUpdated`, `TemplateCategoryUpdated`.
- `list` returns one `Page<TemplateInfo>`; `list_stream` follows cursors.
  `get(&id)`, `get_fields(&id, &["status"])`.
- `edit(&id, &TemplateEdit::components(vec![..]))` **replaces all
  components**; only `APPROVED`, `REJECTED` or `PAUSED` templates can be
  edited, an approved template's category cannot change, and edits of
  approved templates are limited (never replayed on timeout).
- `delete_by_name(name)` deletes every language; `delete_by_id(name, &id)` one;
  `delete_by_ids(&ids)` up to 100 (all or nothing). An approved template's name
  cannot be reused for 30 days.
- At most 100 creations per WABA per hour; `TemplateLimitReached` (2388019)
  for the WABA total.
- **Media headers need a Resumable Upload handle**, not a media id:

```rust
let handle = client.media(pnid)
    .resumable_upload(&app_id, "header.png", "image/png", png_bytes).await?; // UploadHandle
let header = TemplateComponent::header_image(handle.as_str());
```

  (`start_upload_session`/`upload_chunk`/`upload_session_status` for large
  files; the client needs a token.) At *send* time the header takes a normal
  media id: `Parameter::image_id(media_id)`.
- Also available: Template Library (`library`, `create_from_library`),
  `migrate_from`, `compare`, `unpause`. Archive/unarchive has no documented
  endpoint.

## OTP login

Authentication templates carry Meta's preset text ("*{{1}}* is your
verification code.") and one OTP button; create one per language:

```rust
use wa_rs::client::authentication::AuthenticationTemplate;

client.authentication(waba_id).create(
    &AuthenticationTemplate::copy_code("login_code", "en_US")
        .security_recommendation(true)
        .code_expiration_minutes(10), // keep equal to OtpConfig::ttl
).await?;
```

`one_tap(name, lang, [SupportedApp::new(package, signature_hash)])` and
`zero_tap(name, lang, apps, terms_accepted)` exist for Android autofill.

Then `OtpService` issues, stores (hashed) and verifies codes:

```rust
use std::sync::Arc;
use wa_rs::client::authentication::{
    IssueOutcome, OtpConfig, OtpPepper, OtpService, OtpTemplate, VerifyOutcome,
};
use wa_rs::core::clock::SystemClock;
use wa_rs::core::recipient::Recipient;

let otp = OtpService::new(
    client.clone(),                               // the store's own number and token
    phone_number_id,
    OtpTemplate::new("login_code", "en_US"),      // must be APPROVED
    kv.clone(),                                   // shared KvStore (Postgres/Redis)
    Arc::new(SystemClock),
    OtpPepper::new(pepper_bytes)?,                // ≥ 32 random bytes, from your secret manager
    OtpConfig::default(),                         // 6 digits, 10 min, 5 attempts, 30 s, 5/hour, no namespace
)?;

let user = Recipient::phone("+16505551234");      // E.164 WITH the plus sign
match otp.issue(&user, "login").await? {
    IssueOutcome::Sent(challenge) => show_code_screen(challenge.expires_at),
    IssueOutcome::CoolingDown { retry_after } => ask_to_wait(retry_after),
    IssueOutcome::RateLimited { retry_after } => ask_to_wait(retry_after),
}
match otp.verify(&user, "login", typed_code.trim()).await? {
    VerifyOutcome::Verified => sign_in(),                        // consumed: single use
    VerifyOutcome::Invalid { attempts_left } => wrong_code(attempts_left),
    VerifyOutcome::Expired | VerifyOutcome::NotFound => ask_for_new_code(),
    VerifyOutcome::TooManyAttempts => ask_for_new_code_later(),
}
```

`OtpService::new` checks the config (`OtpConfig::validate()` returns
`Result<(), ValidationError>` naming the field: `code_length`, `ttl`,
`max_attempts`, `issue_limit`, `namespace`) and reports a bad one as
`Error::Config`. `issue` sends through `client.messages(pnid).send(..)`, so
the message passes `OutboundMessage::validate`, and an unreadable send
response is reported without quoting the user's number.

### Several services on one store: codes are scoped

Every code, cooldown and issue limit is bound to the service's **sending
`phone_number_id`** and its optional **`OtpConfig::namespace`**, on top of
the recipient's digits and the `purpose`. So one `KvStore` and one pepper
can serve any number of services — e.g. one per merchant, each sending from
the merchant's own number: a code merchant A sent never verifies at
merchant B, and A's cooldowns and limits never block B.

- Different sending numbers need nothing more.
- **One number sending codes for several merchants, tenants or apps: set
  the namespace to the tenant id, always.** Services on the same number
  with the default config (`namespace: None`) share one scope: a code sent
  for one tenant verifies at another, and cooldowns and limits are pooled.
  `OtpConfig { namespace: Some("tenant-42".into()), ..OtpConfig::default() }`
  (not blank). Changing a service's namespace invalidates its outstanding
  codes. (Whether to make it required: `OPEN_QUESTIONS.md` #34.)
- **Moving your `rev` from before e40b86f to e40b86f or later changes every
  store key once**: codes in flight at the deploy come back `NotFound` (users
  request a new one) and issue limits restart. Deploy outside peak login
  time.

~~Codes were keyed by the pepper, the recipient's digits and the purpose
only~~: true until e40b86f (2026-09-24). Two services sharing a store and a
pepper (several merchants of one integrator) shared records: a code
merchant A sent verified at merchant B for the same number and purpose, an
issue at A replaced B's code, and cooldowns and limits were pooled. On an
older pin, give every sending number its own pepper or its own store.

### Why E.164 with `+` is mandatory (the takeover it prevents)

Meta prepends **your sending number's country code** to a number without `+`.
If codes were keyed by digits alone, a code sent to `"12015553931"` (delivered
to `+91 12015553931` from an Indian number) would verify `"+12015553931"` —
the owner of the first number logs in as the second. So `issue`/`verify`
refuse anything but `+` followed by up to 15 digits (spaces, `-`, `(`, `)`
ignored; no leading 0), bind the code to exactly those digits (and to the
sending number, above), and send to exactly `+<digits>`. Normalize user input to E.164 **with the country code**
yourself (a phone-number library, a country picker); from a webhook `wa_id`,
prepend `+`. Never "fix" a validation error by stripping the `+`.

A BSUID-only or group recipient is refused locally (`Error::Validation` on
`recipient`): authentication templates cannot go to a BSUID (Meta's 131062,
`RecipientNotSupported`). Handle both the same way — ask for a phone number.

### Limits and outcomes

- `max_attempts` (5) wrong guesses per code; the attempt is counted
  atomically (compare-and-swap) **before** comparing, so concurrent guesses
  cannot exceed it. A match consumes the code.
- `resend_cooldown` (30 s) stops accidental resends; the **issue limit** (5
  codes per recipient number and purpose per rolling hour, per service,
  `IssueLimit::DEFAULT`) is what
  bounds brute force (≈0.06 %/day at 6 digits). It is on by default;
  `issue_limit: None` is an explicit opt-out for when an equivalent
  per-number limit sits in front. The attacker chooses the victim's number,
  so limit per number, not per IP.
- `purpose` separates flows (`"login"`, `"reset_password"`, `"change_phone"`).
- `issue` returning **`Err`** after a timeout or 5xx means the code may have
  been delivered: the challenge stays verifiable and the cooldown applies.
  Tell the user to wait and retry; do not call `issue` in a loop.
- `verify` compares byte for byte: trim input. Every call with a live code
  counts as an attempt.

### Pepper and storage custody

- `OtpPepper` keys every HMAC (store keys and code hashes); a store key is
  HMAC(pepper, scope | digits | purpose), the scope being the sending
  `phone_number_id` and the namespace. No phone number or code is stored. At least 32 bytes from a CSPRNG (e.g. the text of a
  `openssl rand -base64 32` string is fine); keep it **outside** the database
  that holds the challenges, or a dump can be brute-forced (6 digits = 10⁶).
- Rotating the pepper invalidates outstanding codes and resets issue logs.
- Use a **shared** `KvStore` (Postgres/Redis) when more than one instance
  serves logins; with `MemoryKvStore` each instance has its own limits.
  Postgres/Redis expire records by *their* clock: keep hosts on NTP.
- Namespaces used: `wa.otp`, `wa.otp.rate`.

### Never

- Never log or render the code (no PDFs, images, emails of it — `wa-rs-documents`).
- Never send an OTP with a plain text message or a non-authentication
  template; `OtpService` builds the exact authentication payload
  (`otp_template_message`).
- Never let `OtpConfig::ttl` and the template's `code_expiration_minutes`
  disagree: the message would promise a lifetime the service does not honour.

## Not handled by the library

Session/JWT issuance after `Verified`, account linking, fallback channels
(SMS) when WhatsApp is undeliverable (`Undeliverable`, 131026), and per-IP or
per-device throttling in front of `issue`.
