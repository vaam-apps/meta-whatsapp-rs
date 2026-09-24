# Design: a deployable wa-rs service (`wa-server`)

> **Draft for the owner's review. Design only: no service code exists.**
> Against `main` = bbf24a3 (2026-09-24), Graph API v25.0. Assumes the
> changes in flight on another branch (`OtpConfig::namespace` required; the
> partner Intent API's result type, `marketing::OnboardingRequest` today,
> renamed) and Solution Partner onboarding landing next. Owner choices are
> marked **Decision for owner (Dn)** and listed in [§10](#10-decisions-for-the-owner).

The product decision: apps not written in Rust (Medusa, in TypeScript; the
CMS, any stack) use wa-rs through a **service deployed as a Docker image
and called over HTTP**. In short: a binary crate, `crates/wa-server`
(axum), built only on the `wa-rs` facade, productizes the runnable examples
and keeps their security rules; one multi-tenant deployment per Meta app;
a public listener serving only Meta's webhook and an internal one for the
API; Postgres for all state, with an event outbox feeding SSE, polling and
signed webhooks; `/v1` REST/JSON, a committed OpenAPI 3.1 spec, a generated
TypeScript client; four milestones.

## 1. Goals and non-goals

| Caller | Reaches | Credential ([§3.2](#32-credentials)) | Typical calls |
| --- | --- | --- | --- |
| Medusa backend | internal API | tenant key (the store is a tenant) | order templates, invoices, OTP login, events by webhook |
| CMS backend | internal API | platform key + `WA-Tenant` header | Embedded Signup, inbox, replies, merchants' templates, events |
| Merchant's browser | the CMS only (D3) | the CMS session | — |
| Meta | public listener, `/webhooks/meta` | verify token, `X-Hub-Signature-256` | subscription check, deliveries |
| Operator | `/v1/admin`, the CLI | admin key | tenants, keys, own numbers, vault rotation |

**Goals.** (1) What the three products need (messages, media, templates,
OTP, Embedded Signup, inbox, live events) over HTTP, with the library's
rules intact: E.164 with `+`, BSUID keys, no blind send retries, secrets
never logged, ownership checked before the vault is read, 401 before the
body on a bad signature. (2) Safe by default: loopback binds, refuses to
start on a blank secret, no CORS, no raw Graph passthrough. (3) Horizontal
scaling with Postgres as the only dependency. (4) A versioned contract
generated clients can rely on.

**Non-goals.** A public end-user API. A UI, campaign scheduler, consent
registry or job queue (callers own these, as with the library). All of
`wa-client` in v1: analytics, commerce, groups, calling, Flows, QR codes,
In-App Signup, Marketing Messages API sends and block users come on demand,
each a thin route over an existing module. Behaviour the library lacks
(token refresh, dead-lettering, a call-aware window). A Graph proxy:
`MessageContent::Raw` and `Client::request_url` are not exposed.

**Decision for owner (D1): tenancy per deployment.** (a) One deployment per
tenant; (b) one multi-tenant deployment per Meta app; (c) one per product.
Every merchant onboarded by Embedded Signup delivers to the **app's single
callback URL**, and template and account webhooks ignore per-WABA overrides,
so (a) needs a router in front and cannot share one vault.
*Recommendation: (b)*, the store being one tenant when it shares the app;
separate deployments only for separate apps or environments.

## 2. Architecture

### 2.1 The crate

`crates/wa-server`, binary `wa-server`, `publish = false`, a workspace
member (so `just ci` covers it). It depends only on the `wa-rs` facade
(`postgres`, `axum`, `typst`; axum and sqlx through its re-exports, OQ
#29): the API an outside integrator has, which proves the facade suffices.
[architecture.md](../architecture.md)'s "nothing depends on `wa-rs`"
becomes "no library crate depends on `wa-rs`; binaries may" (L6). New
dependencies (OpenAPI generation, `tower-http`, a Prometheus exporter, a
rate limiter, `clap`) must pass `cargo deny`.

```text
               public listener (:8080)                      internal listener (:8081)
Meta ─HTTPS─► ingress ─► GET|POST /webhooks/meta        Medusa / CMS ─► /v1/…            (API key)
                             │                           operator     ─► /v1/admin, /metrics, /readyz
                             ▼
     WebhookHandler: signature (401 before the body), 3 MiB, DedupGuard (Postgres KvStore)
                             │ each event, in order
                             ├─► InboxSink  ─► ConversationStore
                             └─► OutboxSink ─► wa_server_events ── NOTIFY ──┐
            ┌──────────────────────────────┬────────────────────────────────┘
            ▼                              ▼                                ▼
  SSE hub (each replica LISTENs)   GET /v1/events (poll)    webhooks-out workers (SKIP LOCKED)

API call ─► key → tenant ─► tenant owns number/WABA? ─► TokenVault ─► client.with_token ─► wa-client
```

Reused unchanged: `wa_rs::webhooks::router`, `DedupGuard`, `InboxSink`,
`Inbox`, `TokenVault`, `EmbeddedSignup`, `SignupSessions`, `OtpService`, the
endpoint modules, `wa_rs::typst::Renderer`, the Postgres stores. Not used:
`wa_rs::webhooks::sse` and `BroadcastSink` (the service needs resume and
cross-replica fan-out, [§4.5](#45-live-updates-sse)).

### 2.2 Configuration and storage

Environment variables, with a `<NAME>_FILE` variant for every secret; an
optional TOML file (`WA_SERVER_CONFIG`) for non-secrets. The list is in
[§7.2](#72-environment). **The service refuses to start** on a blank or
missing app secret or verify token (closing OQ #16 for itself), a missing
vault key or a pepper under 32 bytes with Postgres, memory storage outside
`WA_SERVER_ENV=development`, identical public and internal binds, or
Solution Partner mode without its credentials.

Postgres is required outside development (memory stores and a throwaway
vault key only there). Library tables keep their `wa_` prefix; the
service's use `wa_server_` and their own migration history:

| Table | Holds |
| --- | --- |
| `wa_server_tenants` | id, name, status, settings (OTP sender and template, limits) |
| `wa_server_api_keys` | key id, SHA-256 of the secret, kind, tenant(s), scopes, expiry, revocation, last use |
| `wa_server_wabas` | `waba_id` (unique) → tenant, credit allocation id (Solution Partner) |
| `wa_server_numbers` | `phone_number_id` (unique) → tenant, WABA, status (`connected`, `reconnect_required`, `disconnected`) |
| `wa_server_signup_attempts` | resumable attempts: tenant, WABA, failed step, session info (never the code or PIN) |
| `wa_server_idempotency` | [§5.4](#54-idempotency-keys) |
| `wa_server_events` | the outbox: sequence, id, tenant (null = operator-only), number, WABA, type, JSON |
| `wa_server_webhook_endpoints`, `…_deliveries` | [§4.4](#44-webhooks-out) |

Vault, OTP challenges, dedup claims and signup sessions stay in the
library's `KvStore` namespaces (`wa.token`, `wa.otp`, `wa.webhook.dedup`,
`wa.es.session`).

### 2.3 The event pipeline

- **Sinks run in sequence, inbox then outbox** (not concurrently as
  `FanoutSink` does): whoever sees an event can already read its history.
- **Routing is an allow-list.** An event naming a phone number goes to the
  tenant owning it; a WABA-level event (template, account, quality) to the
  tenant owning the WABA. `unknown`, `unparsed`, `partner_solution_updated`
  and events for unowned numbers or WABAs are operator-only rows (metric,
  log with size and digest), never shown to a tenant.
- Inserts are idempotent on `WebhookEvent::dedup_key`. A sink error answers
  Meta 500 and the batch is redelivered (OQ #30 applies unchanged).
- The service adds `number_connected`, `number_disconnected` and
  `number_reconnect_required` (after a 190 on a merchant's call) events.
- `data` is the library's `WebhookEvent` JSON, pinned by snapshot tests
  over Meta's documented examples: a library change that alters it fails
  the server's tests and forces an API-version decision.

### 2.4 Several replicas

| Concern | Behaviour |
| --- | --- |
| vault, OTP, signup sessions, dedup, inbox, outbox, keys | shared in Postgres; expiry by the database clock (NTP) |
| dedup lease (60 s) | a retry meeting a live lease on another replica gets 503 and Meta returns; a crashed replica's lease expires; the sink path (two inserts) stays far below 60 s |
| SSE | per replica, fed from the outbox by `LISTEN/NOTIFY`; `Last-Event-ID` resumes on any replica |
| webhooks-out | workers on every replica claim rows with `FOR UPDATE SKIP LOCKED` and a lease; no leader |
| rate limits | token buckets per replica (limit ÷ replicas); a shared limiter only if needed |
| API key cache | at most 30 s; a revocation `NOTIFY` purges every replica at once |
| housekeeping (`purge_expired`, outbox and idempotency purges) | any replica, under an advisory lock |
| migrations | at start, under the library's lock and a service advisory lock; expand-then-contract so rolling deploys can mix versions |

## 3. Tenancy and authentication

### 3.1 The model

```text
tenant (a merchant, or the platform's store) 1──n WABA 1──n phone number
  ├─ API keys, webhook endpoints, OTP settings     └─ business token in TokenVault, by WABA
  └─ id: the integrator's (e.g. the CMS merchant id), 1–64 of [A-Za-z0-9._:-],
     immutable (it is the OTP namespace: changing it invalidates codes in flight)
```

### 3.2 Credentials

| Kind | Sent as | Acts as | For |
| --- | --- | --- | --- |
| tenant key | `Authorization: Bearer wak_…` | its tenant | Medusa; any backend that is one tenant |
| platform key | the same, plus `WA-Tenant: <id>` | the named tenant, if among its allowed ones (`*` or a list) | the CMS backend, for its merchants |
| admin key | the same | `/v1/admin` only | the operator |

Keys carry scopes (`send`, `media`, `templates`, `inbox`, `events`,
`webhooks`, `signup`, `otp`, `numbers`), look like `wak_<key id>_<32 random
bytes, base62>` (the prefix lets secret scanners find leaks) and are stored
as the key id plus SHA-256 of the secret, compared in constant time (the
examples' scheme; random 256-bit secrets need no slow hash). Shown once;
several active per tenant; rotate by create, deploy, revoke. The first admin
key comes from the CLI (`wa-server admin create-admin-key`), not the
environment.

**Decision for owner (D2): credential model.** (a) Tenant keys only: the
CMS keeps one key per merchant, minted at onboarding; (b) platform key plus
header only; (c) both. A platform key is as powerful as every tenant it may
name, but the per-tenant checks still stop a CMS bug from crossing tenants.
*Recommendation: (c).*

### 3.3 Authorization order

Every tenant route, as middleware plus typed extractors (`OwnedNumber`,
`OwnedWaba`) that are the only way a handler obtains a token:

1. Authenticate the key before reading the body, else `401`.
2. Resolve the tenant (the key's, or `WA-Tenant` within the platform key's
   set), else 403 (`tenant_suspended` for a suspended one).
3. Check the scope, else `403 forbidden`.
4. **Ownership**: the path's `{pn}` or `{waba_id}` must be bound to that
   tenant, else `404 not_found`, the answer for a number that does not
   exist, so tenants cannot probe each other.
5. Only now read the vault: no entry is `409 number_not_connected`; an
   expired token, or a number marked after a 190, `409
   reconnect_required`. Then `client.with_token(token)`.

### 3.4 How numbers get bound

- **Embedded Signup** ([§4.6](#46-embedded-signup)): when `onboard`
  succeeds, or fails at a resumable step with the token stored, the
  verified `waba_id` and every number in `Onboarded::phone_number_ids` are
  bound to the calling tenant, in one transaction with the D4 check.
- **Admin attach** (the platform's own WABA): the admin gives the WABA id
  and a system user token; the service lists the WABA's numbers from Meta
  with it before `TokenVault::store` (which trusts its input). Ids are
  never bound on a caller's word.
- **Disconnect**: `DELETE /v1/wabas/{waba_id}` unsubscribes the app with
  the merchant's token, deletes the vault entry and the bindings. An
  `account_updated` with `PARTNER_APP_UNINSTALLED` or `ACCOUNT_DELETED`
  does the same and emits `number_disconnected`.

**Decision for owner (D3): browser access.** (a) Never browser-facing: the
CMS relays signup calls and live events (an SSE relay per open inbox, or
webhooks-out into its own realtime channel); (b) short-lived number-scoped
stream tokens plus a CORS allow-list, so browsers open SSE directly: one
hop fewer, but the service is exposed beyond `/webhooks/meta`.
*Recommendation: (a) in v1*; (b) can be added later without breaking
anything.

**Decision for owner (D4): one WABA, several tenants (OQ #6).** (a) Refuse
(`409 waba_owned_by_another_tenant`), checked on the claimed id before
`redeem` and on the verified id after `onboard` (by then the vault holds the
second merchant's token, also valid for that WABA; the binding stays);
(b) move the binding to the newest tenant (today's "last onboarding
wins"); (c) share it (one vault token per WABA: both act with the last one
stored). *Recommendation: (a)*, with an admin unbind.

## 4. The HTTP API

### 4.1 Conventions

- JSON under `/v1`, snake_case; Meta ids as strings (they overflow JS
  numbers); RFC 3339 UTC times; lists take `?limit=` (≤ 100) and an opaque
  `cursor` and answer `{"data", "next_cursor"}` (inbox cursors wrap the
  store's exclusive ones: no repeats, no gaps).
- **Phone numbers are E.164 with `+` everywhere**; digits-only is `422` on
  the field (Meta would read it as local to the sender's country).
  Recipients: `{"phone"}`, `{"user_id"}` (BSUID), both, or `{"group_id"}`.
- `X-Request-Id` echoed and logged; `Idempotency-Key` on sends
  ([§5.4](#54-idempotency-keys)). Body limits: 64 KiB, 16 KiB for signup
  completion, 100 MiB for uploads. Request DTOs reject unknown fields;
  responses may gain fields, which clients ignore.
- `{pn}` is a phone number id; `{contact}` (BSUID, `wa_id` or group id) and
  `{message_id}` are percent-encoded. Every tenant route may also answer
  `401`, `403`, `404` (not owned), `409 number_not_connected` /
  `reconnect_required`, `422 invalid_request`, `429 too_many_requests`.

### 4.2 Resources

**Numbers and profile** (scope `numbers`)

| Method and path | Does | Request → response |
| --- | --- | --- |
| `GET /v1/wabas`, `GET /v1/numbers` | the tenant's WABAs; its numbers with connection status | → `{data: [...]}` |
| `GET /v1/numbers/{pn}` | live details: display number, verified name, quality, name status, throughput | → object |
| `GET`, `PATCH /v1/numbers/{pn}/profile` | business profile: about, address, description, email, websites, vertical | partial profile → profile |
| `DELETE /v1/wabas/{waba_id}` | disconnect ([§3.4](#34-how-numbers-get-bound)); `502` if unsubscribing fails, nothing deleted | → 204 |

**Messages and media** (scopes `send`, `media`)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `POST /v1/numbers/{pn}/messages` | send free-form (Meta enforces the window), template or reaction ([§4.3](#43-message-content)) | `{to, type, <type>: {…}, reply_to?, callback_data?}` → `202 {message_id, contacts}` | 409 `customer_service_window_closed`, `marketing_opted_out`; 422 `template_*`; 429; 502/504 with `may_have_been_sent` |
| `POST /v1/numbers/{pn}/messages/{message_id}/read` | blue ticks; optional typing indicator (never replayed) | `{typing_indicator?}` → 204 | |
| `POST /v1/numbers/{pn}/media` | upload, type and size checked first | multipart `file`, `type` → `201 {media_id}` | 422 `type`, 413 |
| `GET /v1/numbers/{pn}/media/{media_id}` | download, SHA-256 verified before the first byte; `?max_bytes=` (default and cap 16 MiB), larger with `?stream=true` | → bytes, `X-WA-SHA256` | 413 `media_too_large`, 502 `integrity` |
| `DELETE /v1/numbers/{pn}/media/{media_id}` | delete | → 204 | |
| `POST /v1/numbers/{pn}/documents` (M4) | render `invoice`, `receipt` or `voucher` from its JSON input (`date` required: the renderer has no clock), upload | `{template, input, date, format}` → `201 {media_id, filename, mime_type}` | 422 on the input |

**Templates** (scope `templates`)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `GET /v1/wabas/{waba_id}/templates` | list, `?status=&name=&cursor=`, cached 60 s per WABA (Meta allows 200 management calls an hour per WABA) | → page of `{id, name, language, status, category, components}` | |
| `GET /v1/wabas/{waba_id}/templates/{id}` | one | → template | |
| `POST /v1/wabas/{waba_id}/templates` | create from Meta's JSON shape (`TemplateDefinition` deserializes it), validated locally | → `201 {id, status, category}` | 422 `template_rejected`; 409 `template_limit_reached` |
| `POST /v1/wabas/{waba_id}/templates/authentication` (M3) | copy-code, one-tap or zero-tap, several languages | → 201 | |
| `DELETE /v1/wabas/{waba_id}/templates?name=[&id=]` | every language of a name, or one | → 204 | |

Review results arrive as `template_status_updated` events.

**Inbox** (scope `inbox`)

| Method and path | Does | Request → response |
| --- | --- | --- |
| `GET /v1/numbers/{pn}/conversations` | newest activity first | → page of `{contact, last_message_at, last_inbound_at, last_text, unread, window}` |
| `GET /v1/numbers/{pn}/conversations/{contact}/messages` | history, newest first | → page of `{id, direction, kind, text, payload, status, timestamp, status_at, error}` |
| `GET /v1/numbers/{pn}/conversations/{contact}/window` | the 24-hour window (calls unseen: OQ #32) | → `{open, closes_at}` (`null`: templates only) |
| `POST /v1/numbers/{pn}/conversations/{contact}/messages` | reply as the merchant to the conversation's contact; free-form refused locally outside the window (`409`, nothing sent), templates exempt; recorded `accepted` | content → `202 {message_id}` |
| `POST /v1/numbers/{pn}/conversations/{contact}/read` | reset unread; `notify_customer: true` also marks the latest inbound message read on Meta | → 204 |

**Events and webhook endpoints** (scopes `events`, `webhooks`)

| Method and path | Does | Request → response |
| --- | --- | --- |
| `GET /v1/events` | poll after a sequence, `?after=&types=&phone_number_id=`; `410 cursor_expired` past retention | → `{data: [envelope], next_after}` |
| `GET /v1/events/{id}` | one event in full (for truncated deliveries) | → envelope |
| `GET /v1/events/stream`, `GET /v1/numbers/{pn}/events/stream` | SSE for the tenant, or one number ([§4.5](#45-live-updates-sse)); `429 too_many_streams` | `text/event-stream` |
| `POST /v1/webhook-endpoints` | forward events to a URL ([§4.4](#44-webhooks-out)); `422 url` outside the allowed destinations | `{url, types, tenants?}` → `201 {id, secret}` (shown once) |
| `GET`, `PATCH`, `DELETE /v1/webhook-endpoints[/{id}]` | list, read, change URL, types or enabled, delete | |
| `POST /v1/webhook-endpoints/{id}/rotate-secret`, `…/test` | new secret (the old one also signs for 24 h); send a `ping` | → `{secret}`; the delivery |
| `GET /v1/webhook-endpoints/{id}/deliveries`, `POST …/{delivery_id}/retry` | attempts (status, duration, next attempt; no bodies); redeliver a failed one | page; 202 |

`tenants` (a list or `"*"`) is for platform keys; a tenant key's endpoint
receives its own tenant's events.

**Embedded Signup and OTP** (scopes `signup`, `otp`)

| Method and path | Does | Request → response | Notable errors |
| --- | --- | --- | --- |
| `POST /v1/signup/sessions` | bind an attempt (15 min) to the tenant; what `FB.init` and `FB.login` need | `{coexistence?}` → `{state, expires_at, app_id, config_id, graph_api_version, launch_options}` | |
| `POST /v1/signup/complete` | redeem for this tenant, then onboard | `{state, code, event, pin?}` → `{status: connected \| cancelled, waba_id, phone_number_ids, steps_completed, needs_coexistence_sync, credit_allocation_id?}` | 403 `stale_attempt`; 409 `waba_owned_by_another_tenant`; 502 `onboarding_failed` (`step`, `resumable`) |
| `GET /v1/signup/attempts` | the tenant's resumable attempts | → `{data: [{waba_id, failed_step, updated_at}]}` | |
| `POST /v1/signup/resume` | rerun the steps after the stored token | `{waba_id, pin?}` → like `complete` | 404 `nothing_to_resume`; 502 `onboarding_failed` |
| `POST /v1/numbers/{pn}/coexistence-sync` | start the contacts and history sync, once, within 24 h (D7) | `{types}` → 202 | 409 `sync_not_allowed` |
| `POST /v1/otp/issue` | send a code with the tenant's authentication template | `{phone, purpose?}` → `{status: sent, challenge_id, expires_at}` or `{status: cooling_down \| rate_limited, retry_after_seconds}` | 422 `phone`, `recipient_not_supported`; 502/504 with `may_have_been_sent: true` |
| `POST /v1/otp/verify` | check the typed code; always counts as an attempt | `{phone, purpose?, code}` → `{result: verified \| invalid \| expired \| too_many_attempts \| not_found, attempts_left?}` | 422 |

The signup state travels in bodies only (URLs reach access logs). OTP
outcomes are `200` with a status, as the library returns them as `Ok`:
a caller's generic error handling can never swallow `invalid`.

**Admin and operations**

| Method and path | Does |
| --- | --- |
| `POST`, `GET`, `PATCH`, `DELETE /v1/admin/tenants[/{id}]` | create, list, suspend, configure (OTP sender and template, limits), delete (disconnects every WABA) |
| `…/tenants/{id}/keys[/{key_id}]`, `/v1/admin/platform-keys[/{key_id}]` | mint, list, revoke keys; platform keys with their allowed tenants |
| `POST /v1/admin/tenants/{id}/wabas`; `DELETE /v1/admin/wabas/{waba_id}/binding` | attach an own WABA, verified with Meta; unbind (D4) |
| `POST /v1/admin/vault/rotate` | re-encrypt every WABA's token under the active key, walking `wa_server_wabas` (the vault cannot list itself) |
| `GET /livez`, `/readyz`, `/metrics`, `/v1/openapi.json`, `/v1/version` | internal listener, no key; `version` reports server, wa-rs revision, Graph and API versions |

The public listener serves `GET|POST /webhooks/meta` and `GET /livez`,
nothing else.

### 4.3 Message content

A union owned by the service, named after Meta's Cloud API message object
(so Meta's pages describe it) and mapped onto `wa-client` builders, which
`OutboundMessage::validate` checks before any request (`OutboundMessage` is
serialize-only, so no library change). Types: `text`; `image`, `video`,
`audio`, `document`, `sticker` by `id` or `https://` `link` (Meta fetches
it, never the service); `location`, `contacts`, `reaction`; `template`
(Meta's object, deserialized into `TemplateMessage`); `interactive`
`button`, `list`, `cta_url`. Anything else, `MessageContent::Raw` and Direct
Send included, is `422 unsupported_message_type`. A streamed download
**aborts the connection** on a digest mismatch, so unverified bytes never
arrive as a complete body.

### 4.4 Webhooks-out

For backends such as Medusa that prefer not to hold a stream open.

```json
{"id": "evt_01J8Z6Q4M3", "sequence": 18342, "type": "message_received", "api_version": "v1",
 "tenant_id": "merchant-42", "phone_number_id": "106540352242922", "waba_id": "102290129340398",
 "received_at": "2026-09-24T10:00:01Z", "truncated": false,
 "data": {"event": "message_received", "…": "the wa-rs WebhookEvent JSON"}}
```

- **Signature: [Standard Webhooks](https://www.standardwebhooks.com/)**
  (`webhook-id`, `webhook-timestamp`, `webhook-signature: v1,<base64
  HMAC-SHA256(secret, "{id}.{timestamp}.{body}")>`, secrets `whsec_…`), with
  verifiers for TypeScript and most stacks. Receivers reject timestamps over
  5 minutes off, which Meta's signature cannot offer. Rotation sends both
  signatures for 24 h. Secrets are stored encrypted under
  `WA_SERVER_DATA_KEY` (signing needs them).
- **Delivery**: `POST`, 10 s timeout, 2xx is success, no redirects, at most
  1 KiB of the answer read. **At least once**: backoff with jitter from
  10 s, at most 1 h a step, for 72 h, then `failed` (retryable through the
  API); an endpoint failing the whole window is disabled. Unordered:
  receivers deduplicate on `id` and order on `sequence`.
- **Destinations** only within `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS`
  (hosts, CIDRs), HTTPS outside development; the address is resolved,
  checked and pinned per attempt (no DNS rebinding). Private ranges pass
  only when listed (Medusa in the same cluster usually is).
- `data` over 256 KiB (a history sync) is left out (`truncated: true`);
  `GET /v1/events/{id}` returns it.

### 4.5 Live updates (SSE)

- Frames `event: whatsapp`, `id: <sequence>`, `data: <envelope>`; a
  keepalive comment every 15 s; `event: lagged` when a slow client lost
  events (reload history, or poll from its last sequence). Reconnecting with
  `Last-Event-ID` replays from the outbox, on any replica, within retention.
- Each replica holds one `LISTEN` connection; `NOTIFY` carries only the
  sequence (well under Postgres's 8,000-byte payload limit). New rows are
  read once and shared (`Arc`) into channels per (tenant, number), created
  with their first subscriber: a stream never receives, or clones, another
  tenant's events, so OQ #31's cost does not arise.
- Limits: 20 streams per tenant, 1,000 per replica. Operator-only rows are
  never streamed. `EventSource` cannot send `Authorization`: browsers go
  through the CMS (D3).

### 4.6 Embedded Signup

```text
merchant's browser       CMS backend                     wa-server                           Meta
"Connect" ────────────► POST /connect ─────────────────► POST /v1/signup/sessions (WA-Tenant: m42)
          ◄─ launch data ◄────────────────────────────── {state, app_id, config_id, launch_options}
FB.login(…) ──────────────────────────────────────────────────────────────────────────────► popup
          ◄─ code (30 s, single use) + WA_EMBEDDED_SIGNUP event ◄─────────────────────────────┘
POST /connect/callback ► POST /v1/signup/complete ─────► redeem(state, m42), onboard ─────────► exchange, verify, store,
  {state, code, event, pin}                              bind WABA + numbers to m42             subscribe, [credit line], register
```

Kept from the `embedded_signup` example: the tenant comes from the
credential, never the body (`SignupSessions::redeem` does not burn the state
on a mismatch); inputs are parsed before `redeem`; `onboard` is never
retried; a failure after the token is stored saves a resumable attempt
(session info only; `resume` uses a placeholder code until L4); errors carry
step, kind and `resumable`, never Meta's text; the page gets only the app
id, configuration id, Graph API version and `LaunchOptions`.

| | Tech Provider (`WA_ONBOARDING_MODE=tech_provider`) | Solution Partner (`solution_partner`) |
| --- | --- | --- |
| Steps | exchange code, `debug_token`, verify assets, store token, subscribe app, register number | the same, plus sharing the partner's extended credit line after `subscribe_app` (`POST /{extended_credit_line_id}/whatsapp_credit_sharing_and_attach` with `waba_id`, `waba_currency`) with the **partner's system user token**, never the merchant's; the returned allocation config id is stored on the WABA |
| Who pays Meta | the merchant, after adding a payment method in WhatsApp Manager | the partner's credit line |
| Extra settings | — | `WA_PARTNER_SYSTEM_TOKEN`, `WA_CREDIT_LINE_ID`, `WA_WABA_CURRENCY` |
| Library | implemented | in flight (L3): the step's name, resume behaviour and error kinds come from it |

The mode is per deployment (per Meta app), not per tenant.

**Decision for owner (D5): onboarding mode in production (OQ #3).** (a) Tech
Provider: merchants pay Meta; (b) Solution Partner: the platform pays
through its credit line and bills merchants (needs partner status and a
default currency). Both are supported from M3. *Recommendation: (a) first*,
switching by configuration once partner status and billing exist.

**Decision for owner (D6): two-step PIN (OQ #4).** (a) Typed by the merchant
per attempt, never stored (today's behaviour); (b) generated and stored
encrypted by the service. *Recommendation: (a)*: one leak must not expose
every number's PIN.

**Decision for owner (D7): coexistence sync (OQ #7)**, due within 24 h of
onboarding a WhatsApp Business app number. (a) An explicit endpoint; (b)
automatic after onboarding; (c) the endpoint plus a per-tenant setting for
(b). *Recommendation: (c)*, automatic by default: 24 h is easy to miss. The
inbox still records neither echoes nor history (OQ #17).

### 4.7 OTP

- One `OtpService` per (tenant, sending number), cached, with
  `OtpConfig::namespace` = the tenant id: codes, cooldowns and issue limits
  never cross tenants, even on a shared number.
- Per-tenant settings (admin API): sending number, approved authentication
  template and language, `OtpConfig` (TTL = the template's expiry; defaults
  per OQ #13). `phone` is strict E.164; a BSUID alone is refused (131062).
- The code is in no response, log or error; OTP routes drop Meta's text
  and `details` (a template error could quote a parameter: the code).
  Per-IP throttling stays the caller's (the service sees only the backend).

**Decision for owner (D8): which number sends a tenant's codes.** (a) One
platform number for all (the user sees the platform's name); (b) each
merchant's own number (the user sees the merchant; each needs an approved
authentication template); (c) per tenant, defaulting to (a).
*Recommendation: (c).*

## 5. Error model

### 5.1 Body

```json
{"error": {"code": "customer_service_window_closed",
  "message": "More than 24 hours since the customer's last message: send a template.",
  "retryable": false, "may_have_been_sent": false, "field": null, "step": null, "resumable": null,
  "graph": {"code": 131047, "subcode": null, "fbtrace_id": "AbCdEf", "details": "…"},
  "request_id": "req_01J8Z6Q4M3"}}
```

`code` is stable (codes only grow within `v1`; an unknown one is handled
by its status class). `message` is the service's sentence for the code,
never Meta's message or an input value. `graph` appears when Meta answered
with an error; `details` is dropped on OTP and signup routes.

### 5.2 Codes and statuses

Meta-derived codes are the snake_case `ErrorKind` name
(`ErrorKind::TemplateParameterMismatch` → `template_parameter_mismatch`),
defined once in the library by `ErrorKind::as_str()` and pinned by a test
(L1): `ErrorKind` is non-exhaustive, so a service-side mapping would turn
each new kind into `unknown` silently.

| HTTP | Codes | Meaning |
| --- | --- | --- |
| 422 | `invalid_request` (local validation, with `field`), `invalid_parameter`, `unsupported_message_type`, `recipient_not_supported`, `undeliverable`, `template_parameter_mismatch`, `template_not_found`, `template_text_too_long`, `template_policy_violation`, `template_rejected`, `idempotency_key_reused` | fix the request; nothing was sent |
| 409 | `customer_service_window_closed`, `marketing_opted_out`, `blocked_by_business`, `experiment_holdout`, `template_paused`, `template_disabled`, `template_syncing`, `template_unavailable`, `template_limit_reached`, `flow_unavailable`, `registration`, `two_step_verification`, `sync_not_allowed`, `duplicate_onboarding`, `number_not_connected`, `reconnect_required` (also Meta's `authentication` on a merchant token), `waba_owned_by_another_tenant`, `idempotency_in_progress`, `outcome_unknown` | a state must change first |
| 403 | `permission`, `account_restricted`, `country_restricted`, `payment`, `feature_not_available`, `marketing_not_allowed`; `forbidden`, `tenant_suspended`, `stale_attempt` | not allowed, by Meta or the service |
| 429 | `rate_limited`, `pair_rate_limited`, `spam_rate_limited`, `ecosystem_engagement_limit`, `classification_limit_reached`, `too_many_requests`, `too_many_streams` | `Retry-After` when known; `retryable` says whether waiting helps (false for 131048, 131049) |
| 404, 410, 413 | `not_found`, `nothing_to_resume`; `cursor_expired`; `payload_too_large`, `media_too_large` | |
| 502 | `service_unavailable`, `unknown`, `upstream` (non-Graph answer), `integrity`, `media_download_failed`, `media_upload_failed`, `onboarding_failed` | Meta or the network failed |
| 504 | `timeout` | no answer in time: a send may have gone out |
| 503, 500 | `storage_unavailable`, `shutting_down`; `internal` (configuration, an undecryptable vault record) | |

### 5.3 Was it sent?

Every error from a sending route carries `may_have_been_sent`
(`Error::may_have_been_sent`) and `retryable` (`Error::is_retryable`). The
rule the docs and the SDK teach: resend only when `may_have_been_sent` is
`false`; otherwise reconcile through `status_updated` events carrying your
`callback_data`, or repeat with the same `Idempotency-Key`, which never
sends twice. The service adds no send retries of its own.

### 5.4 Idempotency keys

- `Idempotency-Key` on sends, inbox replies, OTP issue, uploads and
  template creation, scoped to the tenant; stored with a hash of (method,
  route, body), the state and the final answer.
- A request inserts the key `in_progress` (unique), runs, stores the answer.
  A repeat with the same body gets it back (`Idempotent-Replayed: true`); a
  different body is `422 idempotency_key_reused`; a concurrent one `409
  idempotency_in_progress`.
- **Released when the outcome proves nothing was sent**
  (`may_have_been_sent` is false: validation, 4xx, throttling), so the
  caller may retry with it; otherwise kept, success or not: a key never
  sends twice. After a crash the key reads as unknown once its lease (twice
  the Graph timeout) ends: repeats get `409 outcome_unknown` with
  `may_have_been_sent: true`, never a new send.
- Kept 24 h (`WA_SERVER_IDEMPOTENCY_TTL`). Callers derive keys from their
  own records (`order:1234:shipped`) and set `callback_data` to the same.

## 6. Security

| Threat | Control |
| --- | --- |
| Forged Meta deliveries | signature over raw bytes with any of N app secrets; missing or malformed header `401` before the body is read; 3 MiB; the public listener serves nothing else |
| Replayed Meta bodies | dedup for 7 days; bodies never logged |
| A tenant reading or sending as another | ownership before the vault ([§3.3](#33-authorization-order)); foreign numbers are `404`; extractors are the only path to a token |
| A stolen platform key | limited to its tenants and scopes; internal network only; revocation effective across replicas at once |
| A stolen database dump | tokens encrypted (vault key elsewhere), API keys hashed, OTP codes and numbers only as HMACs (pepper elsewhere), webhook secrets encrypted (data key elsewhere); inbox history is readable, so database encryption at rest is the operator's |
| Signup attributed to the wrong merchant | state bound to the tenant, redeemed for the credential's tenant; ids verified with Meta; D4 |
| OTP brute force, cross-tenant codes | the library's limits, per-tenant rate limits, namespace = tenant |
| SSRF | no URL fetching (media by id, through the library's host allow-list); webhooks-out allow-list, no redirects, pinned address |
| Exhaustion | per-route body limits, per-tenant rate limits, stream caps, timeouts, bounded media and rendering concurrency |

**Secrets** come from the environment or `<NAME>_FILE`; never the TOML
file, the image, the logs, or the database they protect.

| Secret | Protects | Rotation |
| --- | --- | --- |
| `WA_APP_SECRET` (+ `WA_APP_SECRET_PREVIOUS`) | Meta signatures, code exchange, app token | both listed while rolling out |
| `WA_VERIFY_TOKEN` | Meta's subscription check | with the App Dashboard |
| `WA_VAULT_KEY`, `WA_VAULT_KEY_ID`, `WA_VAULT_PREVIOUS_KEYS` | merchants' tokens | `VaultKeys::with_previous`, `POST /v1/admin/vault/rotate`, drop the old |
| `WA_OTP_PEPPER` | OTP hashes | invalidates codes in flight |
| `WA_SERVER_DATA_KEY` (+ previous) | webhook signing secrets | like the vault key |
| `WA_PARTNER_SYSTEM_TOKEN` | credit-line sharing | new token, deploy, revoke |

**Never logged or echoed**: API keys and `Authorization`, Meta tokens, the
app secret, the verify token, signup codes and states, PINs, OTP codes,
webhook secrets, bodies, query strings, signature headers, `WebhookEvent`'s
`Debug`; Meta's error messages go to `debug` logs only. Request logs carry
the route template, not the raw path (contacts identify customers).

**Rate limits** per tenant and replica, overridable by the admin: sends
20/s (burst 40), reads 50/s, template management 2/s, OTP issue 5/s, 20
streams; `429` with `Retry-After`. Meta's limits (80 messages/s per number,
portfolio limits) still apply; pacing campaigns is the caller's job.

**CORS**: none (D3). **TLS** terminates at the ingress: valid certificate,
body limit of at least 3 MiB, no body rewriting or decompression, a timeout
above the slowest sink. The internal listener is reachable only from the
private network (a `NetworkPolicy` admitting the two backends). Egress:
`graph.facebook.com`, `lookaside.fbsbx.com`, allowed webhook destinations.

## 7. Operations

### 7.1 Docker image

- Multi-stage: `rust:1.98.1` (the pinned toolchain), then
  `gcr.io/distroless/cc-debian12:nonroot` (glibc, no shell, non-root). Not
  `distroless/static`: a static musl build of aws-lc-rs needs extra tooling
  and musl's allocator is slower; revisit if size matters more.
- The image sets both binds to `0.0.0.0` explicitly (loopback is
  unreachable in a container); the binary's default stays loopback.
- `HEALTHCHECK` runs `wa-server healthcheck` (no curl). Read-only root file
  system (fonts bundled). amd64 and arm64, SBOM and provenance attestations.

**Decision for owner (D9): image name and registry.** (a) Public on GHCR
next to the public repository; (b) a private registry. The name follows the
crate names still open in OQ #1. *Recommendation: (a)*, named with OQ #1.

### 7.2 Environment

| Variable | Default | What |
| --- | --- | --- |
| `DATABASE_URL` | required outside development | Postgres |
| `WA_SERVER_ENV` | `production` | `development` allows memory storage, a throwaway vault key, plain-HTTP webhook targets |
| `WA_SERVER_PUBLIC_BIND`, `WA_SERVER_INTERNAL_BIND` | `127.0.0.1:8080`, `127.0.0.1:8081` | must differ |
| `WA_APP_ID`, `WA_APP_SECRET`, `WA_VERIFY_TOKEN`, `WA_ES_CONFIG_ID` | — | the Meta app |
| `WA_VAULT_KEY*`, `WA_OTP_PEPPER`, `WA_SERVER_DATA_KEY*` | — | [§6](#6-security) |
| `WA_ONBOARDING_MODE` | `tech_provider` | `solution_partner` needs `WA_PARTNER_SYSTEM_TOKEN`, `WA_CREDIT_LINE_ID`, `WA_WABA_CURRENCY` |
| `WA_GRAPH_API_VERSION`, `WA_GRAPH_ENDPOINT` | `ApiVersion::DEFAULT` (v25.0), Graph | the version is also handed to the signup page; the endpoint serves proxies and test stubs |
| `WA_SERVER_WEBHOOK_ALLOWED_DESTINATIONS` | none | hosts and CIDRs for webhooks-out |
| `WA_SERVER_OUTBOX_RETENTION`, `…_IDEMPOTENCY_TTL`, `…_WEBHOOK_RETRY_WINDOW` | 7 d, 24 h, 72 h | |
| `WA_SERVER_MEDIA_MAX_BYTES`, `WA_SERVER_SHUTDOWN_GRACE`, `WA_SERVER_MIGRATE` | 100 MiB, 25 s, `auto` | `skip` when a job runs `wa-server migrate` |
| `RUST_LOG`, `WA_SERVER_LOG_FORMAT`, `OTEL_EXPORTER_OTLP_ENDPOINT` | `info`, `json`, unset | |

### 7.3 Start, observability, shutdown

- `wa-server serve` validates the configuration, runs `postgres::migrate`
  and the service's migrations (unless `skip`), opens both listeners.
  Also: `migrate`, `openapi`, `healthcheck`, `admin …`, `vault rotate`.
- Logs: `tracing` JSON, a span per request (request id, route template,
  tenant, status, duration); library events keep their levels
  ([production guide](../guides/production.md)); optional OpenTelemetry.
- Prometheus metrics, labelled by route template, event type, code and
  outcome, never by id, number or contact (`tenant` only on request):
  requests and latency; Meta webhooks by outcome (a run of `in_flight`
  means sinks outlast the lease); events by type (a rising `unknown`: Meta
  shipped a field worth typing); Graph errors by code; outbox lag; pending
  webhooks-out; open streams; OTP outcomes; idempotent replays.
- `SIGTERM`: `/readyz` fails, listeners stop accepting, open requests get
  `WA_SERVER_SHUTDOWN_GRACE`, streams end with `event: shutdown` (clients
  resume elsewhere), workers finish or release their lease. A Meta delivery
  cut mid-way is redelivered after its lease expires.

### 7.4 Backups

| State | Lose it and | Backup |
| --- | --- | --- |
| vault **and the vault key** | every merchant reconnects; a backup without the key is useless | database PITR; the key backed up in the secret manager, separately |
| tenants, key hashes, bindings, webhook endpoints (+ data key) | integrations stop until recreated | PITR |
| inbox history, outbox and deliveries | history; events never delivered to the integrator | PITR (subject to D10) |
| idempotency records (24 h) | a repeat after a restore could send twice | PITR |
| dedup claims (7 d) | Meta's retries delivered twice (sinks are idempotent) | PITR, not critical |
| OTP challenges, signup sessions | codes and attempts in flight fail | not needed |

**Decision for owner (D10): retention and erasure.** The service stores
customers' messages and identifiers. Choose retention for inbox history
(keep, or purge after N days), the outbox (7 days proposed) and delivery
logs (30 days proposed), and whether an erasure endpoint (one contact on one
number) is required; erasure needs a `ConversationStore` port change (L5).
*Recommendation*: configurable, keeping history by default; erasure in M2
if the platform's privacy obligations require it. A legal and product call.

### 7.5 Versioning

Within `/v1`, changes are additive; a breaking change is `/v2`, served
beside `/v1` for a deprecation period; webhook endpoints keep their
`api_version`. The spec is committed (`crates/wa-server/openapi/v1.json`):
CI fails when the generated one differs, and `oasdiff` checks breaking
changes against the last release. Image, spec `info.version` and the
TypeScript client share one semver; `/v1/version` adds the wa-rs revision.

## 8. Client SDKs

Generate types, not a runtime: `openapi-typescript` turns the committed
spec into types, `openapi-fetch` (a few kB, `fetch`, Node 20+) types calls
by path and method; no Java toolchain, no generated classes to review. A
thin hand-written layer adds the credential and `WA-Tenant` headers, an
`Idempotency-Key` option, a `WaServerError` whose `code` is a union of the
documented codes, an SSE reader that sends headers and resumes, and
`verifyWebhook()`. It lives in `clients/typescript`, built and tested in CI.

**Decision for owner (D11): publishing the client.** (a) Public npm under
the organization's scope; (b) GitHub Packages (consumers need a token even
for a public repository); (c) vendored by consumers. *Recommendation: (a)*,
matching the public, MIT-licensed repository.

**Decision for owner (D12): a Medusa module or plugin.** (a) None: Medusa's
developers call the client from their own module; (b) a Medusa v2 plugin in
its own repository: a notification provider (order templates), an auth
provider (WhatsApp OTP), a route turning webhooks-out into Medusa events;
(c) (a) now, (b) as a fifth milestone after the first integration.
*Recommendation: (c)*: before one integration exists, a plugin guesses at
Medusa's workflows.

## 9. Delivery plan

Each milestone goes through the project's pipeline (implement, adversarial
review with distinct lenses, remediate, re-run the original failure, `just
ci` on the final head) and names its companion docs and skills in the PR.
Tests use `ScriptedTransport` (method, path, token, exact JSON,
`remaining() == 0`); live tests are `live_*`, and `just test-live` gains
`-p wa-server` under `WA_RS_REQUIRE_LIVE=1`.

| # | Library change (own PR, own parity) | When | Kind |
| --- | --- | --- | --- |
| L1 | `ErrorKind::as_str()`, stable snake_case, pinned by a test | M1 | additive |
| L2 | `OtpConfig::namespace` required | in flight, before M3 | breaking |
| L3 | Solution Partner credit-line step in onboarding | in flight, before M3's partner mode | additive |
| L4 | a code-less `OnboardingRequest` for `resume` (OQ #10) | M3, optional | additive |
| L5 | `ConversationStore` erasure | if D10 asks | port change |
| L6 | [architecture.md](../architecture.md): dependency rule for binaries, a "Service" section | M1 | docs |

| | Scope | Docs and skills it adds |
| --- | --- | --- |
| **M1** skeleton, auth, messages and templates, webhooks in | the crate, fail-closed configuration, both listeners, storage and migrations, tenants, keys, admin API and CLI bootstrap, admin attach, the authorization order, messages, media, templates (list, get, create, delete), `/webhooks/meta` into inbox and outbox, `GET /v1/events`, errors (L1), idempotency, rate limits, health, metrics, tracing, the committed spec | `docs/guides/server.md` (run, configure, tenants, keys, first send); a README section "Not writing Rust? Run the service"; L6; a `docs/coverage.md` row; skills `wa-rs-server` (hub for HTTP callers: deploy, credentials, errors, idempotency, routing) and `wa-rs-server-send` (messages, templates, media); the skills gate below |
| **M2** inbox, live updates, webhooks out | inbox routes, SSE (`LISTEN/NOTIFY`, `Last-Event-ID`), `GET /v1/events/{id}`, webhook endpoints, dispatcher, retries, destination allow-list, the service's number events | `server.md` inbox and events; skill `wa-rs-server-inbox` (inbox API, relaying live events to the CMS's browsers, receiving and verifying webhooks-out) |
| **M3** Embedded Signup in both modes, OTP | signup routes, persisted attempts, disconnection, coexistence sync, authentication templates, OTP and its per-tenant settings; needs L2 (and L3 for partner mode) | `server.md` onboarding and OTP; skills `wa-rs-server-onboarding` (the CMS connect flow through the service: page, relay, PIN, resume, both modes) and `wa-rs-server-otp` |
| **M4** TypeScript client, Docker image, docs | `clients/typescript`, the image and its CI, a Compose file, the documents route, the deployment guide | `server.md` deployment (Docker, Compose, Kubernetes notes); a `docs/guides/README.md` row; skill `wa-rs-server-typescript` (install, calls, errors, idempotency, SSE, webhook verification in Medusa or any Node backend); `wa-rs-production` points to the service |

Acceptance tests. "Decisive" names the guard whose removal must make the
test fail.

| # | Test |
| --- | --- |
| M1.1 | `just ci` exits 0 with the crate included (lint, doc, deny with the new dependencies) |
| M1.2 | A body signed with `wa_rs::webhooks::sign` is `200` and one outbox row for the owning tenant; no signature is `401` without the body being polled; the same body twice is one row; 3 MiB + 1 byte is `413`; `unknown` and `unparsed` are operator-only. Decisive: routing an unowned number's event to a tenant |
| M1.3 | Table-driven over every `{pn}` and `{waba_id}` route in the spec (a new route cannot skip it): tenant B's key on A's number is `404`, and a counting vault wrapper records zero reads. Decisive: step 4 of [§3.3](#33-authorization-order) |
| M1.4 | Sends carry the merchant's vault token; a digits-only `to.phone` is refused before any request; a scripted timeout is `504` with `may_have_been_sent: true` and the same `Idempotency-Key` replays it with no second request; a scripted 131047 is `409` and releases the key |
| M1.5 | Every `ErrorKind` maps to a code and status (iterating L1's list); a sentinel in a scripted Graph error message reaches no response |
| M1.6 | One test per start-up refusal ([§2.2](#22-configuration-and-storage)); the generated spec equals the committed one |
| M1.7 | Live: two instances on one database deduplicate the same webhook; parallel migrations succeed. Captured `tracing` output of a send and a webhook holds no token, secret, key, message text, phone number or contact |
| M2.1 | On Postgres: inbound webhook → conversation list → history → reply in the window (recorded `accepted`) → a status webhook moves it to `delivered`; a free-form reply outside the window is `409` with zero requests |
| M2.2 | Live, two replicas on one database: a webhook into A reaches a stream on B within 2 s; `Last-Event-ID` replays exactly the missed events; a stream on tenant B's number sees none of A's |
| M2.3 | No `unknown` event reaches any stream, poll or endpoint. Decisive: the routing allow-list |
| M2.4 | Webhooks-out: Standard Webhooks test vectors; retry schedule under a fake clock; redirects not followed; a destination outside the allow-list refused at creation and at delivery (a resolver stub that turns private on the second lookup); two signatures during rotation; over 256 KiB delivered truncated and fetchable only by its tenant |
| M2.5 | 200 open streams at 100 events/s with bounded memory |
| M3.1 | The happy path over HTTP with Meta's documented responses binds the verified WABA and all its numbers; no response contains the token |
| M3.2 | Another tenant's state is `403 stale_attempt` and the rightful tenant still completes; a malformed PIN is `422` and the state survives |
| M3.3 | A scripted 133005 at registration is `502 onboarding_failed` (`register_phone`, resumable); a new process on the same database resumes with a corrected PIN; another tenant's resume is `404` |
| M3.4 | Partner mode: the credit-line request carries the partner's system token (asserted header), never the merchant's; missing partner settings refuse the start. D4: a second tenant onboarding a bound WABA gets `409`, binding unchanged. `PARTNER_APP_UNINSTALLED` removes the vault entry and bindings |
| M3.5 | OTP: every outcome; tenant A's code verifies at no other tenant on the same number (decisive: the namespace); logs hold neither code nor number; a sentinel in a scripted Graph error on issue reaches no response |
| M4.1 | The image builds for both architectures, runs non-root on a read-only file system, has no shell; `wa-server healthcheck` works in it |
| M4.2 | A Compose smoke test in CI (Postgres, the image, a Graph stub via `WA_GRAPH_ENDPOINT`): CLI admin key, tenant, attach, send, a signed Meta webhook, a webhooks-out delivery verified at a stub receiver |
| M4.3 | The client is generated from the committed spec; `tsc --noEmit` passes on it and on every TypeScript excerpt of the server skills; a Node test verifies a real delivery with `verifyWebhook()`; a breaking change within `v1` fails the spec diff; the invoice fixture renders byte-identically |

**The skills gate for HTTP callers (M1).** `crates/wa-rs/tests/skills.rs`
assumes Rust (no TypeScript fences; backticked names must exist in
`crates/`), so server skills would fail it or pass unchecked if
allow-listed. M1 extends it for `skills/wa-rs-server*`: a `ts` fence must be
an excerpt of the skill's `examples/*.ts`, type-checked by a new `just
skills-ts` (tsc against the generated client, Node pinned) inside `just
ci`; backticked routes, schemas and codes are checked against the committed
spec. Stamps, the 160-line limit and hub listing apply unchanged.

**Decision for owner (D13): where the server skills live.** (a) This
repository's `skills/` (`npx skills add vaam-apps/wa-rs -s wa-rs-server …`),
changed in the same PR as the API; (b) a separate skills repository with
its own coverage gate. *Recommendation: (a)*: API, spec and skills change
atomically and are checked against the same commit.

## 10. Decisions for the owner

| # | Question | Options | Recommendation | Needed by |
| --- | --- | --- | --- | --- |
| D1 | Tenancy per deployment | per tenant / per Meta app / per product | one multi-tenant deployment per Meta app | M1 |
| D2 | Credentials | tenant keys / platform key + header / both | both | M1 |
| D3 | Browser access | never (the CMS relays) / stream tokens + CORS | never, in v1 | M2 |
| D4 | One WABA, several tenants (OQ #6) | refuse / move / share | refuse, admin unbind | M3 |
| D5 | Onboarding mode in production (OQ #3) | Tech Provider / Solution Partner | Tech Provider first, switch by configuration | M3 |
| D6 | Two-step PIN (OQ #4) | per attempt, never stored / generated and stored | per attempt | M3 |
| D7 | Coexistence sync (OQ #7) | endpoint / automatic / both, per tenant | both, automatic by default | M3 |
| D8 | Who sends a tenant's OTP codes | platform number / merchant's / per tenant | per tenant, platform number by default | M3 |
| D9 | Image name and registry (with OQ #1) | public GHCR / private registry; the name | public on GHCR, name settled with OQ #1 | M4 |
| D10 | Retention and erasure of customers' messages | keep / purge after N days; erasure or not | configurable, keep by default; erasure if required (L5) | M2 |
| D11 | Publishing the TypeScript client | npm / GitHub Packages / vendored | public npm | M4 |
| D12 | A Medusa plugin | none / now / after the first integration | after the first integration | after M4 |
| D13 | Where the server skills live | this repository / a separate one | this repository | M1 |

**Inherited from [OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md).** Until
decided, the service keeps the library's behaviour and makes it visible to
callers: #5 (multi-WABA signups), #8 (no token refresh: the service reports
`reconnect_required`), #9 (vault key custody and cadence: rotation becomes
one call), #10 (resume without a code), #13 (OTP issue limit), #18 (U+0000
stored as U+FFFD), #30 (a permanently failing sink holds back its batch),
#32 (calls do not reopen the inbox's window), #33 (message ids unique per
store).
