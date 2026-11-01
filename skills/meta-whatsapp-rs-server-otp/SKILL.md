---
name: meta-whatsapp-rs-server-otp
description: "WhatsApp one-time passcodes (login codes, password resets) through meta-whatsapp-server over HTTP from an app not written in Rust (a Medusa store, a CMS backend) - POST /v1/otp/issue and /v1/otp/verify, their 200 outcomes (sent, cooling_down, rate_limited; verified, invalid, expired, too_many_attempts, not_found), E.164 phone numbers, purposes, the Idempotency-Key on issue, may_have_been_sent, the operator's per-tenant OTP settings (the sending number and template, the platform's number by default) and creating authentication templates. Load when a non-Rust backend logs users in with WhatsApp codes through the service."
---

# meta-whatsapp-rs-server-otp

> **Verified against meta-whatsapp-rs ef70c19d7ab4905b78eb9b108fa98dad2a43b4c3 (2026-11-01).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/otp.ts](examples/otp.ts) (type-checked against the service's OpenAPI document). Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md).

## When to use

Your backend is not Rust, reaches WhatsApp through meta-whatsapp-server
(`meta-whatsapp-rs-server`: deploying it, keys, errors) and logs users in,
or resets their password, with a code sent over WhatsApp (scope `otp`).

| Route | Does |
| --- | --- |
| `POST /v1/otp/issue` | send a code to `phone`: `200` with `status` (`sent`, `cooling_down`, `rate_limited`) |
| `POST /v1/otp/verify` | check the code the user typed: `200` with `result` |
| `GET\|PUT\|DELETE /v1/admin/tenants/{id}/otp` | the operator's: which number and template send a tenant's codes (admin key) |
| `POST /v1/wabas/{waba_id}/templates/authentication` | the authentication template, copy code or one-tap, in several languages (scope `templates`) |

## Send a code

The service makes the code, keeps only a keyed hash of it, and sends it
with your approved authentication template; you never see it. `phone` is
E.164 **with** its `+` (`+16505551234`), `purpose` a constant of your
code (login when absent). Name each login attempt with an
`Idempotency-Key`: the same key never sends a second code.

```ts
export async function requestCode(
  api: WhatsApp,
  phone: string,
  attempt: string,
): Promise<CodeRequest> {
  const { data, error, response } = await api.POST("/v1/otp/issue", {
    params: { header: { "Idempotency-Key": attempt } },
    body: { phone, purpose: "login" },
  });
  if (error) {
    return failedRequest(error.error, response.headers.get("Retry-After"));
  }
  return issued(data);
}
```

- **Outcomes are `200`** (`OtpIssued`): `sent` with `challenge_id` and
  `expires_at`; `cooling_down` (a code went to that number for that
  purpose less than 30 seconds ago) or `rate_limited` (5 an hour used),
  each with `retry_after_seconds`, and nothing sent. Show the wait.
- **Errors** (`ErrorBody`) are for what you fix: `invalid_request` on
  `phone` (no `+`, separators, a BSUID: Meta cannot send an
  authentication template to one), on `purpose` or `body`;
  `otp_not_configured` (409: the operator has not set a sender for your
  tenant); `too_many_requests` (429, `Retry-After`: your tenant's budget,
  5 issues a second); Meta's errors, `may_have_been_sent` set.
- `may_have_been_sent` true (a `timeout`, a Meta failure): the code may
  be on its way and still verifies. Do not issue another at once: let the
  user type what arrives, or repeat with the **same** key.

```ts
export function failedRequest(error: ErrorObject, retryAfter: string | null): CodeRequest {
  if (error.may_have_been_sent) {
    return { kind: "maybe_sent" };
  }
```

## Check the code

```ts
export async function checkCode(api: WhatsApp, phone: string, typed: string): Promise<CodeCheck> {
  const { data, error } = await api.POST("/v1/otp/verify", {
    body: { phone, purpose: "login", code: typed.trim() },
  });
```

`OtpVerified`'s `result`: `verified` (the code is used up); `invalid`
with `attempts_left` (the attempt counted); `expired`,
`too_many_attempts` (5 wrong guesses kill the code, even the right one
after them) and `not_found` (never sent, used, gone): ask for a new code.
Every check of an outstanding code counts, so never retry one on your
own. Codes are your tenant's: another tenant checking your user's code,
on the same sending number, gets `not_found` and costs you no attempt.

## The sender (operators)

Each tenant's codes go out from the number its OTP settings name, one of
the tenant's own, with its approved template; a tenant without settings
uses the default OTP tenant's (`WA_SERVER_OTP_DEFAULT_TENANT`, the
platform's number and name). None of either: `otp_not_configured`.

```ts
  const { error } = await admin.PUT("/v1/admin/tenants/{id}/otp", {
    params: { path: { id: tenant } },
    body: settings,
  });
```

`OtpSettingsRequest`: `phone_number_id`, `template` (`name`, `language`),
and the limits (`code_length`, `ttl_seconds`, `max_attempts`,
`resend_cooldown_seconds`, `issue_limit`; the issue limit cannot be
turned off). Keep `ttl_seconds` equal to the template's
`code_expiration_minutes`. Changing the sending number kills the codes in
flight; so does rotating `WA_OTP_PEPPER`.

The template, once per WABA (`AuthenticationTemplateRequest`: `name`,
`languages`, `otp_type` `copy_code` or `one_tap` with `supported_apps`):

```ts
  const { data, error } = await api.POST("/v1/wabas/{waba_id}/templates/authentication", {
    params: { path: { waba_id: wabaId }, header: { "Idempotency-Key": "template:login_code:v1" } },
    body: template,
  });
```

## What meta-whatsapp-rs does not do

- It never shows you the code, and never logs it or the phone number.
- No throttling per IP or device: the service sees your backend only.
  Limit requests per visitor yourself.
- No zero-tap templates (accepting Meta's zero-tap terms is not the
  service's to do for you) and no template previews yet.
- No resend on its own: a code whose send may have gone out is kept, not
  sent again.

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, the error body);
`meta-whatsapp-rs-server-send` (messages and templates); from Rust, and
what the limits mean: `meta-whatsapp-rs-otp-login`.
