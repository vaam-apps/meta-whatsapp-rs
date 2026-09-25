---
name: meta-whatsapp-rs-server
description: "Using WhatsApp through meta-whatsapp-server, the meta-whatsapp-rs HTTP service, from an app not written in Rust (a Medusa store, a CMS backend in any stack) - deploying it and its fail-closed environment, tenant keys, platform keys with the WA-Tenant header and admin keys, the first admin key from the CLI, the /v1 routes that exist today, the error body (code, may_have_been_sent, retryable), and a TypeScript client typed from the service's OpenAPI document. Load when a non-Rust project deploys or calls meta-whatsapp-server, needs a key for it, or handles its errors."
---

# meta-whatsapp-rs-server

> **Verified against meta-whatsapp-rs 67a6684f5fbcc542da3c3a7b69c42a5a3ef53bf0 (2026-09-25).** On another revision, trust the service's `/v1/openapi.json` over this page.

Reference code: [examples/client.ts](examples/client.ts), type-checked by
meta-whatsapp-rs's own gate against the service's committed OpenAPI
document. Operators' guide:
[docs/guides/server.md](https://github.com/vaam-apps/meta-whatsapp-rs/blob/main/docs/guides/server.md).

## When to use

Your app is not Rust and reaches WhatsApp through meta-whatsapp-server:
you deploy it, hold its keys, call its /v1 API and handle its errors.
Rust code uses the library directly (`meta-whatsapp-rs`).

## What the service does today (milestone M1a)

| Route | Needs | Does |
| --- | --- | --- |
| `GET /v1/wabas`, `GET /v1/numbers` | scope `numbers` | the tenant's WABAs; its numbers with their `status` |
| `GET /v1/numbers/{pn}` | scope `numbers` | live details from Meta: `display_phone_number`, `verified_name`, `quality_rating`, `name_status`, `throughput` |
| `GET /v1/numbers/{pn}/profile`, `PATCH /v1/numbers/{pn}/profile` | scope `numbers` | the business profile (`about`, `address`, `description`, `email`, `websites`, `vertical`) |
| `DELETE /v1/wabas/{waba_id}` | scope `numbers` | disconnect: the token and bindings go only once Meta unsubscribed the app |
| `POST /v1/admin/tenants`, `POST /v1/admin/tenants/{id}/keys`, `POST /v1/admin/platform-keys` | admin key | tenants and keys |
| `POST /v1/admin/tenants/{id}/wabas`, `DELETE /v1/admin/wabas/{waba_id}/binding` | admin key | attach the platform's own WABA; free one for another tenant |
| `/livez`, `/readyz`, `/metrics`, `/v1/version`, `/v1/openapi.json` | nothing | operations, and the document every client is generated from |

Sending messages, media and templates, Meta's webhooks, the inbox, events,
Embedded Signup and OTP are not there yet (docs/design/server.md, section
9): do not write code against them.

## Deploy

One deployment per Meta app. The public listener (default
`127.0.0.1:8080`) serves only `GET /webhooks/meta` (Meta's subscription
check) and `GET /livez`; the internal one (`127.0.0.1:8081`) serves
everything else and must stay on your private network. There is no image
yet: build it.

```bash
cargo build --release -p meta-whatsapp-server
export DATABASE_URL=postgres://… WA_APP_SECRET_FILE=/run/secrets/app WA_VERIFY_TOKEN_FILE=/run/secrets/verify
export WA_VAULT_KEY_FILE=/run/secrets/vault WA_OTP_PEPPER_FILE=/run/secrets/pepper
export WA_SERVER_PUBLIC_BIND=0.0.0.0:8080 WA_SERVER_INTERNAL_BIND=0.0.0.0:8081
meta-whatsapp-server admin create-admin-key --name ops   # the first admin key, printed once
meta-whatsapp-server serve
```

It **refuses to start** on a missing or blank `WA_APP_SECRET` or
`WA_VERIFY_TOKEN`, no `WA_VAULT_KEY` (base64 of 32 random bytes) or a
`WA_OTP_PEPPER` under 32 bytes with Postgres, no `DATABASE_URL` without
`WA_SERVER_ENV=development`, identical binds, or a Solution Partner
`WA_ONBOARDING_MODE` without its settings. Every secret may come from a
file (`WA_APP_SECRET_FILE`). Keep `WA_VAULT_KEY` backed up apart from the
database: without it no stored token decrypts.

## Credentials

| Key | Send | Acts as |
| --- | --- | --- |
| tenant key | Authorization: Bearer wak_… | its tenant (a store that is one tenant) |
| platform key | the same, plus `WA-Tenant` | the named tenant, if in its allowed set (a CMS acting per merchant) |
| admin key | the same | nobody: the admin routes only |

Keys are shown once (`key` in the answer); the service keeps a digest.
Rotate by minting, deploying, then revoking the old key
(`DELETE /v1/admin/tenants/{id}/keys/{key_id}`); revocation holds on the
next request. Tenant ids are yours (`[A-Za-z0-9._:-]`, 1 to 64) and immutable.

```bash
curl -sS -X POST "$WA_SERVER/v1/admin/tenants" -H "Authorization: Bearer $ADMIN_KEY" \
  -H "Content-Type: application/json" -d '{"id": "merchant-42", "name": "Lucky Shrub"}'
curl -sS -X POST "$WA_SERVER/v1/admin/tenants/merchant-42/keys" -H "Authorization: Bearer $ADMIN_KEY" \
  -H "Content-Type: application/json" -d '{"scopes": ["numbers"], "name": "medusa"}'
curl -sS "$WA_SERVER/v1/numbers" -H "Authorization: Bearer $KEY"
```

## Calling it from TypeScript

Generate the types from the service you deploy
(`npx openapi-typescript "$WA_SERVER/v1/openapi.json" -o meta-whatsapp-server.d.ts`),
then use `openapi-fetch`, one client per key:

```ts
export function whatsapp(baseUrl: string, key: string, tenant?: string) {
  const headers: Record<string, string> = { Authorization: `Bearer ${key}` };
  if (tenant !== undefined) {
    headers["WA-Tenant"] = tenant;
  }
  return createClient<paths>({ baseUrl, headers });
}
```

Lists take `limit` (1 to 100) and `cursor`, and answer `data` and
`next_cursor`; annotate request literals with the generated types (the
numbers query, `ProfilePatch` in the example), or a misspelled field
goes unnoticed:

```ts
const query: NumbersQuery = cursor === undefined ? { limit: 100 } : { limit: 100, cursor };
const { data, error } = await api.GET("/v1/numbers", { params: { query } });
if (error) {
  throw new WhatsAppError(error.error);
}
```

## Errors

Every failure answers one body (`ErrorBody`):

```json
{"error": {"code": "reconnect_required", "message": "The number's token is no longer valid: connect it again.",
  "retryable": false, "may_have_been_sent": false, "field": null, "step": null, "resumable": null,
  "graph": {"code": 190, "subcode": 463, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn", "details": null},
  "request_id": "req_4f1c2a9b0d3e5f67"}}
```

```ts
export function nextStep(error: ErrorObject): Next {
  if (error.may_have_been_sent) {
    return "reconcile"; // it may have taken effect: check before repeating
  }
  switch (error.code) {
```

- Branch on `code` (the `ErrorCode` union), never on `message`. Codes only
  grow: treat an unknown one by its HTTP status class.
- **Repeat a request only when `may_have_been_sent` is false**; a
  `timeout` (504) or a Meta failure (502) may have taken effect.
- `invalid_request` names the culprit in `field`. `401` is always
  `unauthenticated`.
- Another tenant's number is 404 `not_found`, exactly like a missing one.
- `reconnect_required`: Meta rejected the WABA's token (`190`); every call
  on its numbers answers it until an operator attaches the WABA again.
- A code Meta caused is Meta's error kind (`template_not_found`,
  `marketing_opted_out`, …), with Meta's code under `graph`; Meta's own
  error message never reaches you.

## What meta-whatsapp-rs does not do

- No sends, media, templates, webhooks, inbox, events, signup or OTP in
  the service yet (M1b to M3); no Docker image or published TypeScript
  client (M4, the package is an open decision).
- Browsers never call it: no CORS, no browser tokens. Your backend
  relays.
- It is no Graph proxy: only the documented routes exist.
- The memory mode (`WA_SERVER_ENV=development`, no `DATABASE_URL`) keeps
  nothing across restarts, and the admin CLI cannot reach it.

## Related skills

`meta-whatsapp-rs` (the Rust library, when you write Rust),
`meta-whatsapp-rs-errors` (the error kinds behind the Meta codes),
`meta-whatsapp-rs-production` (what the service does for you: secrets,
logs, several instances).
