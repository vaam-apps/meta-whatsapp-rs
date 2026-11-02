---
name: meta-whatsapp-rs-server-registration
description: "Registering a WhatsApp business number through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust - requesting and submitting the verification code (POST /v1/numbers/{pn}/request-code, verify-code), registering with the merchant's two-step verification PIN typed for the attempt and stored nowhere (register), setting a new PIN (two-step-pin), deregistering with a confirmation (deregister), the per-number attempt budgets and Meta's 72-hour lock, Idempotency-Key that never covers the PIN. Load when a CMS or a Medusa store onboards or re-registers a merchant's number through meta-whatsapp-server."
---

# meta-whatsapp-rs-server-registration

> **Verified against meta-whatsapp-rs 983ed5ef1bf828e6e7989563c6db19e97f3a5d29 (2026-11-02).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/registration.ts](examples/registration.ts), type-checked against the service's OpenAPI document. Operators' guide: [docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md#registration-and-the-two-step-pin).

## When to use

A merchant's number is bound to their tenant (Embedded Signup, or the
operator's attach) and must be registered for the Cloud API before it
sends: a new number, one whose display name Meta just approved, or one
deregistered earlier. Deploying the service and its keys:
`meta-whatsapp-rs-server` first. The key needs the scope `numbers`.

## The flow

| Route | Body | Answers |
| --- | --- | --- |
| `POST /v1/numbers/{pn}/request-code` | `{"code_method": "SMS", "language": "en_US"}` (or `VOICE`) | `200`, `status` `code_requested` |
| `POST /v1/numbers/{pn}/verify-code` | `{"code": "123-830"}` | `code_verified` |
| `POST /v1/numbers/{pn}/register` | `{"pin": "212834"}`, optionally `data_localization_region` | `registered` |
| `POST /v1/numbers/{pn}/two-step-pin` | `{"pin": "212834"}` | `pin_set` |
| `POST /v1/numbers/{pn}/deregister` | `{"confirm": "<the same pn>"}` | `deregistered` |

Each answers `RegistrationAnswer`: `phone_number_id` and `status`. A
number verified in WhatsApp Manager skips the code (asking for one is
`409 registration`).

```ts
export async function requestCode(api: WhatsApp, pn: string, attempt: string): Promise<void> {
  const body: CodeRequestBody = { code_method: "SMS", language: "en_US" };
  const { error } = await api.POST("/v1/numbers/{pn}/request-code", {
    params: { path: { pn }, header: { "Idempotency-Key": `number:${pn}:code:${attempt}` } },
    body,
  });
```

A wrong or expired code is Meta's `invalid_parameter` (`422`): ask the
merchant again, or request a new code.

## The PIN: typed per attempt, stored nowhere

Registering sets the number's two-step verification PIN when it has
none, or must prove the one it has. The PIN is the merchant's: a field
on your page (`type="password"`, six digits), sent to your backend, then
to the service, never stored, logged or put in a URL on your side
either. The service sends it to Meta in that one request and keeps
nothing of it (design D6): not in an answer, a log, an error, an audit
event or an idempotency record.

```ts
  const body: RegistrationBody = { pin };
  const { error, response } = await api.POST("/v1/numbers/{pn}/register", {
    // The key covers the number (and the region), never the PIN: a wrong
    // PIN frees it, so the corrected PIN may reuse the same attempt id.
    params: { path: { pn }, header: { "Idempotency-Key": `number:${pn}:register:${attempt}` } },
    body,
  });
```

```ts
  switch (error.error.code as KnownErrorCode) {
    case "two_step_verification":
      return { kind: "wrong_pin" };
    case "registration":
      return { kind: "verify_first" };
    case "too_many_requests": {
```

- A PIN that is not six digits is `422 invalid_request` on `pin`, and a
  body that does not parse `422` on `body`: neither quotes what you
  sent, and Meta is not asked.
- Meta's error texts are dropped on these routes (`details` included):
  read `error.code` and `error.graph.code` (`133005` a wrong PIN,
  `133016` Meta's 72-hour lock).
- On `may_have_been_sent: true` (`502`, `504`) the number may be
  registered: repeat with the same `Idempotency-Key` (the kept answer
  comes back), or read `GET /v1/numbers/{pn}` before trying again.

## Attempts are counted per number

Meta allows 10 registrations and deregistrations per number in 72
hours, then locks the number for 72 hours, and locks it after too many
wrong PINs. The service counts for every replica together, once the
number is found to be yours: 5 registrations and deregistrations in 72
hours (`WA_SERVER_REGISTRATION_ATTEMPTS`, `WA_SERVER_REGISTRATION_WINDOW`),
10 codes requested and checked an hour
(`WA_SERVER_VERIFICATION_ATTEMPTS`, `WA_SERVER_VERIFICATION_WINDOW`).
Past it: `429 too_many_requests` with `Retry-After` the rest of the
window, and nothing reaches Meta. Never retry a registration in a loop:
show the merchant the wait.

## Deregistering

```ts
  const { data, error } = await api.POST("/v1/numbers/{pn}/deregister", {
    params: { path: { pn }, header: { "Idempotency-Key": `number:${pn}:deregister` } },
    body: { confirm: pn },
  });
```

The number stops sending and receiving through the Cloud API until it is
registered again, with its PIN. Meta keeps its history and its local
storage setting, and refuses a number also used in the WhatsApp Business
app. `confirm` must be the path's number (else `422` on `confirm`). The
service writes an audit event in its logs (action number_deregistered,
as for a PIN change, two_step_pin_set) with your key's public id. The number stays
bound to your tenant; its sends answer `409 registration` meanwhile.

## What meta-whatsapp-rs does not do

- It stores no PIN, generates none and cannot recover one: a forgotten
  PIN is reset in WhatsApp Manager (or set with `two-step-pin` while the
  number is registered).
- It does not pass on the wait Meta gives in `details` after too many
  wrong PINs (`133008`): wait for the attempt budget instead.
- It does not register a number on its own after Embedded Signup or a
  display name approval, nor unbind a deregistered number.
- It does not turn two-step verification off (Meta has no call for it),
  nor change data localization after registering (that takes a
  deregistration first).

## Related skills

`meta-whatsapp-rs-server` (deploying the service, keys, errors),
`meta-whatsapp-rs-phone-numbers` (the same calls in the Rust library the
service runs on), `meta-whatsapp-rs-server-send` (sending once the
number is registered).
