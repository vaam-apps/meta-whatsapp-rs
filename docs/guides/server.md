# Run the service: meta-whatsapp-server

Apps not written in Rust (a Medusa store in TypeScript, a CMS backend in
any stack) use meta-whatsapp-rs through **meta-whatsapp-server**, an HTTP
service built on the library. This guide runs it, configures it, creates
tenants and keys, and makes a first authenticated call. The design, with
its reasons and the owner's decisions, is
[docs/design/server.md](../design/server.md); the rules it keeps are in
[architecture.md](../architecture.md#service-meta-whatsapp-server).

> **What exists today (milestones M1a and M1b).** Tenants, API keys, the
> admin API, attaching the platform's own WhatsApp Business Accounts
> (WABAs), the numbers and business profile routes, vault key rotation,
> sending messages (with idempotency keys), read receipts, media upload,
> verified download and delete, template listing, creation and deletion,
> per-tenant rate limits, health, metrics and the OpenAPI document. **Not
> yet**: receiving Meta's webhooks (M1c: `POST /webhooks/meta` answers
> `405`, so delivery statuses do not reach you yet), the inbox and live
> events (M2), Embedded Signup, OTP and authentication templates (M3), the
> Docker image and the TypeScript client (M4).
> [coverage.md](../coverage.md) tracks it.
>
> **Do not point a Meta app's callback URL at an M1a deployment**: Meta's
> subscription check passes, but every delivery is refused (`405`) and
> Meta retries each for up to seven days.

## The shape of it

One deployment serves one Meta app, for many tenants (your merchants, or
your store). It has two listeners:

| Listener | Default bind | Serves | Who reaches it |
| --- | --- | --- | --- |
| public | `127.0.0.1:8080` | `GET /webhooks/meta` (Meta's subscription check), `GET /livez` | Meta, through your HTTPS ingress |
| internal | `127.0.0.1:8081` | the `/v1` API, `/v1/admin`, `/livez`, `/readyz`, `/metrics`, `/v1/openapi.json`, `/v1/version` | your backends and operators, on the private network only |

Both default to loopback; in a container, set them to `0.0.0.0:…`
explicitly and keep the internal one off the internet (a network policy
admitting your backends). The service answers no CORS request: browsers
never call it, your backend does.

## Build and run

There is no image yet (M4). Build the binary from this repository:

```bash
cargo build --release -p meta-whatsapp-server
./target/release/meta-whatsapp-server --help
```

The commands:

| Command | Does |
| --- | --- |
| `meta-whatsapp-server serve` | validates the configuration, migrates (unless `WA_SERVER_MIGRATE=skip`), serves both listeners until `SIGTERM` |
| `meta-whatsapp-server migrate` | creates or upgrades the tables, then exits (for a one-off job) |
| `meta-whatsapp-server openapi` | prints the OpenAPI 3.1 document |
| `meta-whatsapp-server healthcheck` | exits 0 when the internal listener's `/livez` answers 200 |
| `meta-whatsapp-server admin create-admin-key [--expires-at …]` | mints an admin key and prints it once |
| `meta-whatsapp-server admin create-platform-key --tenants '*' --scopes numbers [--expires-at …]` | mints a platform key and prints it once |
| `meta-whatsapp-server admin list-keys [--tenant <id>]` | lists the admin and platform keys (or a tenant's), never a secret |
| `meta-whatsapp-server admin revoke-key <key_id>` | revokes a key of any kind |
| `meta-whatsapp-server vault rotate` | re-encrypts every WABA's token under the active vault key (as `POST /v1/admin/vault/rotate`, without its request deadline; Postgres only: memory storage is per process) |

## Configure

Everything comes from the environment. Every secret may instead come from
a file: set `<NAME>_FILE` to its path (not both; one trailing newline is
dropped). `WA_SERVER_CONFIG`, the design's TOML file of non-secrets, is not
read yet: set, it stops the start.

| Variable | Default | What |
| --- | --- | --- |
| `DATABASE_URL` | required outside development | Postgres (a secret: it holds a password) |
| `WA_SERVER_ENV` | `production` | `development` allows memory storage, a throwaway vault key and a plain-`http` `WA_GRAPH_ENDPOINT` |
| `WA_SERVER_PUBLIC_BIND`, `WA_SERVER_INTERNAL_BIND` | `127.0.0.1:8080`, `127.0.0.1:8081` | must differ |
| `WA_APP_SECRET` | required | the Meta app's secret (`WA_APP_SECRET_PREVIOUS` too while rotating) |
| `WA_VERIFY_TOKEN` | required | the token you enter in the App Dashboard for the webhook |
| `WA_APP_ID`, `WA_ES_CONFIG_ID` | — | read now, used from M3 (Embedded Signup) |
| `WA_VAULT_KEY` | required with Postgres | base64 of 32 random bytes (`openssl rand -base64 32`): encrypts the business tokens |
| `WA_VAULT_KEY_ID` | `k1` | the key's id, recorded with each token |
| `WA_VAULT_PREVIOUS_KEYS` | — | older keys still read, `<id>:<base64>` comma-separated; every id once, the active one's included |
| `WA_OTP_PEPPER` | required with Postgres | at least 32 bytes, kept out of the database (OTP arrives in M3) |
| `WA_ONBOARDING_MODE` | `tech_provider` | `solution_partner` also needs `WA_PARTNER_SYSTEM_TOKEN`, `WA_PARTNER_SYSTEM_USER_ID`, `WA_CREDIT_LINE_ID`, `WA_WABA_CURRENCY` (AUD, EUR, GBP, IDR, INR or USD) |
| `WA_GRAPH_API_VERSION`, `WA_GRAPH_ENDPOINT` | `v25.0`, Graph | a proxy or a test stub: every token travels to it, so `https` outside development, and `serve` warns with its host |
| `WA_SERVER_MIGRATE` | `auto` | `skip` when a job runs `meta-whatsapp-server migrate` |
| `WA_SERVER_SHUTDOWN_GRACE` | `25s` | how long open requests get after `SIGTERM` |
| `WA_SERVER_LOG_FORMAT`, `RUST_LOG` | `json`, `info` | `text` for humans |
| `WA_SERVER_IDEMPOTENCY_TTL` | `24h` | how long an `Idempotency-Key`'s answer is kept (more than the key's one-minute lease) |
| `WA_SERVER_MEDIA_MAX_BYTES` | `104857600` (100 MiB) | the largest upload, and the largest streamed download |
| `WA_SERVER_MEDIA_CONCURRENCY` | `4` | uploads and whole downloads held in memory at once on a replica, one tenant holding half of them at most (the next is `429`) |
| `WA_SERVER_MEDIA_STREAMS` | `16` | streamed downloads (`?stream=true`) at once on a replica, one tenant holding half of them at most (the next is `429`) |
| `WA_SERVER_RATE_SEND`, `WA_SERVER_RATE_SEND_BURST` | `20`, `40` | requests a second per tenant and replica for writes (sends, read receipts, media, profile) |
| `WA_SERVER_RATE_READ`, `WA_SERVER_RATE_READ_BURST` | `50`, `50` | the same for reads |
| `WA_SERVER_RATE_TEMPLATES`, `WA_SERVER_RATE_TEMPLATES_BURST` | `2`, `2` | the same for template management |

**The service refuses to start** rather than run unsafely: on a missing
or blank app secret or verify token, a missing vault key or a pepper
under 32 bytes with Postgres, memory storage outside
`WA_SERVER_ENV=development`, identical binds, Solution Partner mode
without its four settings, a plain-`http` Graph endpoint outside
development, a vault key id used twice, a value it cannot parse, or a
variable set both directly and as a file. The message names the
variable, never its value.

A minimal production start:

```bash
export DATABASE_URL=postgres://wa:…@db.internal:5432/wa
export WA_APP_SECRET_FILE=/run/secrets/wa_app_secret
export WA_VERIFY_TOKEN_FILE=/run/secrets/wa_verify_token
export WA_VAULT_KEY_FILE=/run/secrets/wa_vault_key
export WA_OTP_PEPPER_FILE=/run/secrets/wa_otp_pepper
export WA_SERVER_PUBLIC_BIND=0.0.0.0:8080 WA_SERVER_INTERNAL_BIND=0.0.0.0:8081
meta-whatsapp-server serve
```

For a local try without Postgres, `WA_SERVER_ENV=development` runs on
memory with a throwaway vault key; everything is lost on restart. The
admin CLI talks to the database and cannot reach it, so `serve` writes a
one-time admin key for that process to standard error at start (never
to the logs). For a local Postgres, `docker compose -f compose.test.yaml
up -d --wait` starts one on port 55432 (`postgres://wa:wa@127.0.0.1:55432/wa`).

### Storage and migrations

Postgres holds everything: the library's tables (`wa_kv`, the inbox's
`wa_messages` and `wa_conversations`) and the service's (`wa_server_tenants`,
`wa_server_api_keys`, `wa_server_wabas`, `wa_server_numbers`,
`wa_server_idempotency`), each with its own migration history. `serve` and `migrate` run both under an
advisory lock, so replicas starting together never interleave them.
Service migrations only ever add (expand, then contract in a later
release), so replicas of two versions can share the database during a
rolling deploy. Back up the database and, separately, the vault key: a
backup without the key cannot decrypt a single token.

## Tenants and keys

A tenant is one of your merchants (or your own store). Its id is yours
(the CMS merchant id, say): 1 to 64 characters of `[A-Za-z0-9._:-]`,
immutable. Three kinds of API key, all `wak_<key id>_<secret>`, sent as
`Authorization: Bearer <key>`:

| Key | Acts as | For |
| --- | --- | --- |
| tenant key | its tenant | a backend that is one tenant (the store) |
| platform key | the tenant named in the `WA-Tenant` header, if among its allowed tenants (`*` or a list) | the CMS backend, acting for each merchant |
| admin key | nobody: `/v1/admin` only | operators |

Tenant and platform keys carry scopes (`numbers`, `send`, `media` and
`templates` today; `inbox`, `events`, `webhooks`, `signup`, `otp` for the
routes to come). The service keeps only each key's id and the SHA-256 of its
secret, and shows the key once. Rotate by minting a new one, deploying
it, then revoking the old one: revocation and suspension take effect on
the next request, on every replica.

**1. The first admin key** comes from the CLI, never from the
environment:

```bash
ADMIN_KEY=$(DATABASE_URL=… meta-whatsapp-server admin create-admin-key --name ops)
```

**2. A tenant and its key**, through the admin API:

```bash
curl -sS -X POST http://127.0.0.1:8081/v1/admin/tenants \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"id": "merchant-42", "name": "Lucky Shrub"}'
curl -sS -X POST http://127.0.0.1:8081/v1/admin/tenants/merchant-42/keys \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"scopes": ["numbers"], "name": "medusa"}'
```

The second answer is `{"key": "wak_…", "api_key": {…}}`: store `key` in
your backend's secret manager now; it is not shown again. A platform key
for the CMS is `POST /v1/admin/platform-keys` with
`{"tenants": "*", "scopes": ["numbers"]}` (or a list of tenant ids).

**3. A WABA for the tenant.** Merchants will connect their own numbers
with Embedded Signup (M3). The platform's own WABA is attached by an
operator with a system user token that can reach it; the service asks
Meta for the WABA's numbers with that token, binds exactly those to the
tenant, stores the token encrypted and subscribes the app to the WABA's
webhooks. Keep the token out of your shell history and the process list:
pass the body from a file.

```bash
# attach.json: {"waba_id": "102290129340398", "token": "EAAG…"} (then delete it)
curl -sS -X POST http://127.0.0.1:8081/v1/admin/tenants/merchant-42/wabas \
  -H "Authorization: Bearer $ADMIN_KEY" -H "Content-Type: application/json" \
  -d @attach.json
```

A token Meta rejects while the numbers are listed is `422
invalid_request` on `token`, and nothing is bound or stored. Subscribing
the app comes last, once the WABA is bound and the token stored: if Meta
refuses it, the WABA stays attached and the call answers Meta's error
with `"step": "subscribe_app"` and `"resumable": true`; repeat it once
fixed. A token Meta rejects at that point is `409 reconnect_required`
(its numbers are marked so): repeat the attach with a valid token. A
WABA bound to one tenant is refused to
another (`409 waba_owned_by_another_tenant`, decision D4, also for one of
its numbers); `GET /v1/admin/wabas/{waba_id}` says who holds it, and
`DELETE /v1/admin/wabas/{waba_id}/binding` frees it: the service
unsubscribes the app with the stored token if it still works (Meta
refusing does not stop it), then deletes the token and the bindings. That
is also the way to free a WABA whose token no longer works: its tenant
cannot disconnect it. Suspend a tenant with `PATCH
/v1/admin/tenants/{id}` and `{"status": "suspended"}`; delete it with
`DELETE /v1/admin/tenants/{id}`, which first disconnects each of its
WABAs from Meta and stops at the first that fails (`409` for one without
a usable token: unbind it first). A tenant id already taken is `409
tenant_exists`.

**4. Rotating the vault key.** Put the new key in `WA_VAULT_KEY` (with a
new `WA_VAULT_KEY_ID`) and the old one in `WA_VAULT_PREVIOUS_KEYS`,
deploy, then call `POST /v1/admin/vault/rotate` (or run
`meta-whatsapp-server vault rotate`): it re-encrypts every bound WABA's
token under the new key and answers `{wabas, rotated, failed}`. Drop the
old key once `failed` is empty. The route is a request like any other,
cut at 55 s: a walk too large to finish in time answers `504 timeout`
with part of the tokens rotated. Repeating it is safe (tokens already
under the new key are only read), and the command has no deadline.

This advice holds because, in M1a, every record in the vault belongs to
a bound WABA, which is what the rotation walks. From M3, Solution
Partner onboarding keeps records past a WABA's binding (its credit
ledger outlives the token, and a business's revocation marker outlives
both); until the rotation walks those too, `failed` being empty will
not mean that nothing still needs the old key. M3's notes will say when
it does.

## A first call

With the tenant key (or the platform key and `WA-Tenant: merchant-42`):

```bash
curl -sS http://127.0.0.1:8081/v1/numbers -H "Authorization: Bearer $KEY"
curl -sS http://127.0.0.1:8081/v1/numbers/106540352242922 -H "Authorization: Bearer $KEY"
```

The first lists the tenant's numbers and their connection status from the
service's own records; the second asks Meta for the number's display
number, verified name, quality rating, name status and throughput, with
the WABA's token. Every call checks, in this order: the key (`401`), the
tenant (`403`), the scope (`403`), that the tenant owns the number or WABA
in the path (`404` otherwise, exactly as for a number that does not
exist), and only then reads the token. `GET`/`PATCH
/v1/numbers/{pn}/profile` reads and changes the business profile, and
`DELETE /v1/wabas/{waba_id}` disconnects a WABA: the service unsubscribes
the app with the WABA's token, and deletes the token and bindings only if
Meta agreed.

## Send messages

`POST /v1/numbers/{pn}/messages` (scope `send`) takes Meta's message
object under the service's envelope: `to`, `type`, the object `type`
names, and optionally `reply_to` (a received message's id) and
`callback_data` (echoed in the message's status events):

```bash
curl -sS -X POST http://127.0.0.1:8081/v1/numbers/106540352242922/messages \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -H "Idempotency-Key: order:1234:shipped" \
  -d '{"to": {"phone": "+16505551234"}, "type": "text",
       "text": {"body": "Your order 1234 has shipped."}, "callback_data": "order:1234:shipped"}'
```

It answers `202 {"message_id": "wamid.…", "contacts": [...]}`: Meta
accepted the message; delivery arrives later as status events (M1c).

- **Recipients**: `{"phone": "+16505551234"}` in E.164 **with** its `+`
  (a number without it is refused, `422` on `to.phone`, before any
  request: Meta would read it as local to the sending number's country),
  `{"user_id": "US.13491208655302741918"}` (the business-scoped user id a
  webhook carries), both (Meta uses the phone number), or
  `{"group_id": "…"}`.
- **Types**: `text`; `image`, `video`, `audio`, `document`, `sticker` by
  uploaded `id` or `https://` `link` (Meta fetches it, the service never
  does); `location`, `contacts`, `reaction`; `template` (Meta's object:
  `name`, `language`, `components`); `interactive` of type `button`,
  `list` or `cta_url`. Each is written as Meta's page for it writes it.
  Anything else (Direct Send included) is `422 unsupported_message_type`.
  Every limit Meta documents is checked before the request (`422
  invalid_request` with `field`).
- Meta enforces the 24-hour window: a free-form message outside it is
  `409 customer_service_window_closed` (send a template).
- `POST /v1/numbers/{pn}/messages/{message_id}/read` shows the blue ticks
  on a received message; `{"typing_indicator": true}` shows "typing…" too,
  only when you are about to reply.

### Idempotency keys

Sends, uploads and template creation take an `Idempotency-Key` header (1
to 255 visible ASCII characters), scoped to the tenant. Derive it from
your own records (`order:1234:shipped`) and set `callback_data` to the
same. With one:

- the same key and request never reach Meta twice: a repeat gets the
  first answer back, byte for byte, with `Idempotent-Replayed: true`,
  including a `504 timeout` (the message may have gone out: reconcile
  through its status events, never with a new key);
- an answer that proves nothing was sent (`may_have_been_sent: false`: a
  validation refusal, a 4xx from Meta such as `409
  customer_service_window_closed`, throttling) **releases** the key, so
  you may repeat the request with it once the cause is fixed;
- the same key with another request is `422 idempotency_key_reused`; a
  repeat while the first still runs `409 idempotency_in_progress`; a
  repeat after the first died mid-way (its lease, twice the Graph
  timeout, ran out) `409 outcome_unknown`, `may_have_been_sent: true`,
  never a new send.

Answers are kept `WA_SERVER_IDEMPOTENCY_TTL` (24 hours), then the key is
free again.

## Media

`POST /v1/numbers/{pn}/media` (scope `media`) uploads a
`multipart/form-data` form with `file` and `type` (the MIME type, one of
Meta's supported types) and answers `201 {"media_id": "…"}`, the id to
send in a message:

```bash
curl -sS -X POST http://127.0.0.1:8081/v1/numbers/106540352242922/media \
  -H "Authorization: Bearer $KEY" -F type=image/png -F file=@voucher.png
```

Type and size are checked first: an unsupported type is `422` on `type`,
a file over its type's limit (5 MiB for images, 16 MiB for audio and
video, 500 KiB for stickers, 100 MiB for documents) or over
`WA_SERVER_MEDIA_MAX_BYTES` `413 media_too_large`. Send `type` before
`file`: the upload is then read no further than its type allows. A
form's framing (what precedes each part's data) is read up to 16 KiB;
past it, `422` on `body`.

`GET /v1/numbers/{pn}/media/{media_id}` downloads a file (an upload's or
a received message's), verified against the SHA-256 Meta reports, and
answers it with Meta's MIME type, `X-WA-SHA256` (hex), `Content-Disposition:
attachment` and a sandboxing `Content-Security-Policy` (a customer's
HTML or SVG never runs as a page). By default the whole file is read,
verified, and only then answered: at most `?max_bytes=` (default and
cap 16 MiB; more is `413 media_too_large`), and a mismatch is `502
integrity` with not one byte of the file. With `?stream=true` (up to
`WA_SERVER_MEDIA_MAX_BYTES`) the bytes are forwarded as they arrive, one
chunk behind, and a mismatch **aborts the connection** before the last
chunk: the body never ends cleanly (curl: "transfer closed with
outstanding read data remaining", or "Empty reply from server" for a
small file). Write the bytes somewhere temporary and use them only once
the body ended cleanly. `DELETE /v1/numbers/{pn}/media/{media_id}`
deletes an upload.

**A media id is the number's.** It is digits (`422` on `media_id`
otherwise). Both routes ask Meta with `phone_number_id={pn}`, so Meta
acts only on that number's media, and a deletion first checks the id is
that media (an id names any Graph object a token reaches: a flow, a
group). Another number's media id, another tenant's included, answers
`404 not_found`, exactly like one that does not exist. Meta documents
the check for media uploaded on the number; that it accepts media
received on it by webhook is not documented. If Meta refuses those, a
received file answers `404` too: report it, as the check stays (it is
what keeps one tenant from reading another's files when one token
reaches both).

## Templates

With scope `templates`, on a WABA of the tenant:

| Route | Does |
| --- | --- |
| `GET /v1/wabas/{waba_id}/templates` | a page of `{id, name, language, status, category, components}`, `?status=`, `?name=`, `?limit=`, `?cursor=`; cached 60 s per WABA (Meta allows 200 management calls an hour per WABA) |
| `GET /v1/wabas/{waba_id}/templates/{id}` | one template of this WABA (two management calls: its name, then the WABA's templates of that name) |
| `POST /v1/wabas/{waba_id}/templates` | create from Meta's JSON (`name`, `language`, `category`, `components`), checked locally first; `201 {id, status, category}`; takes an `Idempotency-Key` |
| `DELETE /v1/wabas/{waba_id}/templates?name=…[&id=…]` | every language of a name, or one (with an id, the WABA's templates of that name are listed first) |

A definition breaking a documented limit is `422 invalid_request` with
`field`, before any request, and so is a key the service would not pass
on to Meta (a misspelling, or a field it does not know): never silently
dropped. Meta refusing a definition is `422 template_rejected`, a WABA
at its limit `409 template_limit_reached`. Review results will arrive
as `template_status_updated` events (M1c). A creation or deletion drops
the WABA's cached pages on the replica that made it; others may answer
the old list for up to 60 seconds.

**A template id is the WABA's.** Meta's template object does not say
which WABA it belongs to, so the service looks the id up among the
WABA's own templates: another WABA's template id, another tenant's
included, answers `404 not_found` like one that does not exist, and a
deletion by that id deletes nothing. A deletion is an `audit` event
with the public id of the key that asked it.

## Rate limits

Each tenant has a budget per class of route, per replica (a deployment
of N replicas allows N times as much): writes (sends, read receipts,
media, profile changes) 20 a second with bursts of 40, reads 50 a second,
template management 2 a second. A platform key acting for a tenant draws
from that tenant's budget. Past it: `429 too_many_requests`, `retryable:
true`, with `Retry-After` in seconds, before anything is read or sent.
Change them with `WA_SERVER_RATE_*` (the same for every tenant: per-tenant
limits are not settable yet). Meta's own limits (80 messages a second per
number, pair and portfolio limits) still apply and answer their own codes
(`rate_limited`, `pair_rate_limited`, …); pacing campaigns is yours.

## The contract

The whole contract is the OpenAPI document: `GET /v1/openapi.json`, or
[crates/meta-whatsapp-server/openapi/v1.json](../../crates/meta-whatsapp-server/openapi/v1.json)
in the repository. Generate TypeScript types from it with
`npx openapi-typescript`.

## Errors

Every error answers one body:

```json
{"error": {"code": "reconnect_required",
  "message": "The number's token is no longer valid: connect it again.",
  "retryable": false, "may_have_been_sent": false, "field": null,
  "step": null, "resumable": null,
  "graph": {"code": 190, "subcode": 463, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn", "details": null},
  "request_id": "req_4f1c2a9b0d3e5f67"}}
```

- Branch on `code`, never on `message` (the service's sentence) or on the
  status alone. Codes only grow within `v1`; treat one you do not know by
  its status class. A failure Meta reported carries Meta's error kind as
  its code (`template_not_found`, `marketing_opted_out`, …; the list is
  the `ErrorCode` schema of the OpenAPI document) and Meta's code under
  `graph`; Meta's own error message never reaches you. `graph.details` is
  Meta's text, not the service's (at most 512 characters, without control
  or format characters or line separators): show it to an operator,
  never branch on it.
- A path called with a method it does not take is `405
  method_not_allowed`, with `Allow`.
- **Resend only when `may_have_been_sent` is `false`.** A `504 timeout`
  or a `502` may have taken effect at Meta.
- `invalid_request` names the offending `field`; `401` is always
  `unauthenticated`; a number, WABA, media id or template id of another
  tenant is `404 not_found`, like one that does not exist.
- A token Meta rejects (`190`) marks the WABA's numbers
  `reconnect_required`: later calls answer `409 reconnect_required`
  without asking Meta, until the WABA is attached (or onboarded) again.

## Operations

- `/livez` answers while the process runs; `/readyz` fails when Postgres
  does not answer or after `SIGTERM` (the listeners stop accepting at the
  same moment: keep a preStop delay in Kubernetes if the load balancer
  must see the replica unready first).
- Limits, per listener: a request head must arrive within 10 s (slow
  clients are cut off), 1,024 connections on the public listener and
  4,096 on the internal one (more wait), and every request is answered
  within 55 s (past it, `504 timeout` with `may_have_been_sent: true`).
- `/metrics` (Prometheus) counts requests by listener, method, route
  template, status and error code, their duration, failed Graph calls by
  code, idempotent repeats by outcome (`replayed`, `reused`,
  `in_progress`, `outcome_unknown`) and rate-limited requests by class;
  never an id, a number or a key.
- Expired idempotency records are purged every 10 minutes by one replica
  at a time.
- Logs are JSON, one line per request with its id (`X-Request-Id`, echoed
  or generated), route template (never the raw path), tenant, the public
  id of the key that made it, status and duration; every change an
  operator makes is also an `audit` event (action, admin key id, the
  tenant, key or WABA touched), and so is a tenant's template deletion
  (with the key's id). No secret, token, message text or phone number is
  logged; Meta's error texts only at `debug`.
- Idempotency records keep each kept answer for
  `WA_SERVER_IDEMPOTENCY_TTL`: a send's holds the recipient's phone
  number, WhatsApp id or BSUID, in plain text. Database encryption at
  rest is yours.
- Known limit: key digests are unpeppered SHA-256 of random 256-bit
  secrets. Nobody can reverse one, but whoever can write the database's
  keys table can plant a key: guard write access to it.
- `SIGTERM` fails `/readyz`, stops both listeners from accepting and gives
  open requests `WA_SERVER_SHUTDOWN_GRACE`.

## Not yet

`POST /webhooks/meta` into the inbox and the event outbox, `GET /v1/events`
(M1c); the inbox routes, SSE and webhooks-out (M2); Embedded Signup,
disconnection by Meta's webhooks, coexistence sync and OTP (M3); the
Docker image, a Compose file, the documents route and the TypeScript
client (M4). Tenant settings (OTP sender and template, limits) in `PATCH
/v1/admin/tenants/{id}` come with OTP (M3); until then the rate limits
are the deployment's, for every tenant. The design's TOML file of
non-secrets (`WA_SERVER_CONFIG`) is not read (set, it stops the start),
and OpenTelemetry export is not wired. The `/v1/version` revision reads
`unknown` unless the build sets `META_WHATSAPP_RS_REVISION`.
