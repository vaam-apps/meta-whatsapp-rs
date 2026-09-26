# Run the service: meta-whatsapp-server

Apps not written in Rust (a Medusa store in TypeScript, a CMS backend in
any stack) use meta-whatsapp-rs through **meta-whatsapp-server**, an HTTP
service built on the library. This guide runs it, configures it, creates
tenants and keys, and makes a first authenticated call. The design, with
its reasons and the owner's decisions, is
[docs/design/server.md](../design/server.md); the rules it keeps are in
[architecture.md](../architecture.md#service-meta-whatsapp-server).

> **What exists today (milestones M1a, M1b and M1c).** Tenants, API
> keys, the admin API, attaching the platform's own WhatsApp Business
> Accounts (WABAs), the numbers and business profile routes, vault key
> rotation, sending messages (with idempotency keys), read receipts,
> media upload, verified download and delete, template listing, creation
> and deletion, per-tenant rate limits, Meta's webhooks into the inbox
> and the event outbox, polling events (`GET /v1/events`), health,
> metrics and the OpenAPI document. **Not yet**: the inbox routes, live
> events (SSE) and webhooks to your backend (M2), Embedded Signup, OTP
> and authentication templates (M3), the Docker image and the TypeScript
> client (M4).
> [coverage.md](../coverage.md) tracks it.

## The shape of it

One deployment serves one Meta app, for many tenants (your merchants, or
your store). It has two listeners:

| Listener | Default bind | Serves | Who reaches it |
| --- | --- | --- | --- |
| public | `127.0.0.1:8080` | `GET /webhooks/meta` (Meta's subscription check), `POST /webhooks/meta` (Meta's deliveries), `GET /livez` | Meta, through your HTTPS ingress |
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
| `WA_SERVER_OUTBOX_RETENTION` | `7d` | how long the event outbox keeps an event (a positive duration, `30d`, `72h`); retention is the owner's open decision D10, so this default is the design's proposal until then |
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
`wa_server_idempotency`, the event outbox `wa_server_events` and its
per-tenant `wa_server_event_streams`), each with its own migration
history. `serve` and `migrate` run both under an
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

Tenant and platform keys carry scopes (`numbers`, `send`, `media`,
`templates` and `events` today; `inbox`, `webhooks`, `signup`, `otp` for
the routes to come). The service keeps only each key's id and the SHA-256 of its
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
a usable token: unbind it first); its events go with it, and platform
keys listing it stop allowing it, for good (see
[Receiving Meta's webhooks](#receiving-metas-webhooks)). A tenant id
already taken is `409 tenant_exists`.

**One token, several tenants.** Nothing stops you attaching the same
system user token to WABAs of different tenants, and the service does
not ask Meta what else a token reaches when you attach it. Such a token
reaches every one of those WABAs, so what keeps one of those tenants out
of another's media and templates is only the service's check, on each
route taking an id, that the id is the path's number's or WABA's own
(see [Media](#media) and [Templates](#templates)). Where you can,
attach a token that reaches that tenant's WABAs only.

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
`callback_data` (echoed in the message's status events as
`data.status.biz_opaque_callback_data`, Meta's name for it):

```bash
curl -sS -X POST http://127.0.0.1:8081/v1/numbers/106540352242922/messages \
  -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -H "Idempotency-Key: order:1234:shipped" \
  -d '{"to": {"phone": "+16505551234"}, "type": "text",
       "text": {"body": "Your order 1234 has shipped."}, "callback_data": "order:1234:shipped"}'
```

It answers `202 {"message_id": "wamid.…", "contacts": [...]}`: Meta
accepted the message; delivery arrives later as `status_updated` events
(`GET /v1/events`), whose `data.status.id` is that `message_id` and
`data.status.status` the new state (`sent`, `delivered`, `read`,
`played`, `failed`). After a `504`, which gives you no `message_id`, look for your
`callback_data` in `data.status.biz_opaque_callback_data` before
concluding the message never went out.

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
past it, `422` on `body`. An upload is one request, and every request
is answered within 55 s: your file must reach the service, and the
service's copy reach Meta, within that time, or the answer is `504
timeout`. A 100 MiB document needs about 15 Mbit/s for your part
alone.

Uploads and whole-file downloads share `WA_SERVER_MEDIA_CONCURRENCY`
slots on a replica, streamed downloads `WA_SERVER_MEDIA_STREAMS`, and
one tenant holds half of either at most: past its share, the next
transfer is `429 too_many_requests` (`Retry-After: 1`) before its body
is read or Meta is asked. Retry it once one of yours has finished.

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
reaches both); the remedy planned is
[OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md) #43.

## Templates

With scope `templates`, on a WABA of the tenant:

| Route | Does |
| --- | --- |
| `GET /v1/wabas/{waba_id}/templates` | a page of `{id, name, language, status, category, components}`, `?status=`, `?name=`, `?limit=`, `?cursor=`; cached 60 s per WABA (Meta allows 200 management calls an hour per WABA) |
| `GET /v1/wabas/{waba_id}/templates/{id}` | one template of this WABA (2 to 6 management calls: its name, then the WABA's templates of that name, below) |
| `POST /v1/wabas/{waba_id}/templates` | create from Meta's JSON (`name`, `language`, `category`, `components`), checked locally first; `201 {id, status, category}`; takes an `Idempotency-Key` |
| `DELETE /v1/wabas/{waba_id}/templates?name=…[&id=…]` | every language of a name, or one (with an id, the WABA's templates of that name are listed first) |

A definition breaking a documented limit is `422 invalid_request` with
`field`, before any request, and so is a key the service would not pass
on to Meta (a misspelling, or a field it does not know): never silently
dropped. The template definitions and sends of Meta's examples pass,
`body_text` as a flat list (`["Pablo", "860198"]`, the positional
parameters syntax) included, but for shapes the library cannot carry
yet, refused on the key named:

- in a send's `template`, the payment buttons' `order_details` and
  `payment_request` (India and Brazil payments are out of scope,
  docs/coverage.md row 32);
- in a definition, `optimization_spec` and a URL button's
  `app_deep_link` (Marketing Messages API bidding and app deep links),
  and `package_name` and `signature_hash` on a one-tap or zero-tap
  button itself, which Meta accepts on Graph API v20.0 and older only:
  write them in the button's `supported_apps`;
- a template as `GET` answers it, which is not a definition: its `id`,
  `status` or `correct_category`.

Meta refusing a definition is `422 template_rejected`, a WABA
at its limit `409 template_limit_reached`. Review results arrive as
`template_status_updated` events (`GET /v1/events`). A creation or deletion drops
the WABA's cached pages on the replica that made it; others may answer
the old list for up to 60 seconds.

**A template id is the WABA's.** Meta's template object does not say
which WABA it belongs to, so the service looks the id up among the
WABA's own templates: another WABA's template id, another tenant's
included, answers `404 not_found` like one that does not exist, and a
deletion by that id deletes nothing. A deletion is an `audit` event
with the public id of the key that asked it.

The lookup has limits to know about:

- It lists the WABA's templates with the list's `name=` filter, which
  Meta's reference for that list does not document (the service's tests
  cannot prove Meta honours it). If Meta ignored it, a template would
  be looked for among all the WABA's templates, as far as the pages
  below reach.
- It reads at most 5 pages of 100 templates (a name has one template
  per language, so one page is the rule): a template of the WABA beyond
  them answers `404 not_found`.
- Each lookup costs 2 to 6 of the WABA's 200 management calls an hour:
  `GET …/templates/{id}` asks for the id's name, then 1 to 5 pages; a
  deletion by id reads 1 to 5 pages, then deletes. Lists answered from
  the cache cost nothing, lookups are never cached.

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

## Receiving Meta's webhooks

One callback URL per Meta app receives every merchant's events: point
the app at the public listener, and the service routes each event to the
tenant that owns its number.

**1. Point the Meta app at the service.** In the App Dashboard
(WhatsApp, Configuration), set the callback URL to
`https://<your public host>/webhooks/meta` and the verify token to
`WA_VERIFY_TOKEN`'s value; Meta's check (`GET /webhooks/meta`) answers
the challenge. Subscribe the fields you need (`messages` at least).
Attaching a WABA subscribes the app to it (Embedded Signup will too, M3):
without that, Meta sends nothing for its numbers.

Your ingress must pass bodies of 3 MiB untouched (no decompression, no
rewriting: the signature covers the raw bytes), buffer each request
before it reaches the service (slow clients stay at the ingress), wait
longer than the slowest delivery, and **admit to `/webhooks/meta` only
Meta's webhook IP ranges, or mutual TLS with Meta's client
certificate**. A signature is only checked once a body is read, and
anyone can send a well-formed one: without that filter, anyone on the
internet makes the service read bodies. The app secret signs every
tenant's deliveries: whoever holds it can forge any tenant's events.

**2. What the service does with a delivery** (`POST /webhooks/meta`):

- `401` without a well-formed `X-Hub-Signature-256` (before reading the
  body), or when no app secret produced it (`WA_APP_SECRET`, or
  `WA_APP_SECRET_PREVIOUS` while rotating); `413` past 3 MiB; `408` when
  the body takes over 15 s to arrive.
- A replica reads at most 64 deliveries at once and records at most 4
  (its pool of 10 connections keeps room for API calls): past that,
  `503` and Meta retries. So does a delivery whose tenant's events stay
  locked over 2 s by others being recorded.
- Each event is claimed for 60 s in the shared store: a delivery another
  replica is handling right now answers `503` and Meta retries; one
  already recorded is acknowledged without being recorded again.
- **Routing is an allow-list.** An event naming a business phone number
  goes to the tenant that number is bound to (and only when Meta names the
  WABA the service bound it under); one naming only a WABA (template
  reviews, account and quality updates) to the WABA's tenant; and only
  if Meta dated it no earlier than that WABA's attaching, so a WABA moved
  to another tenant does not bring the first one's late events along.
  It is recorded in the inbox first, then in the event outbox.
- **Operator-only events** are recorded without a tenant, never shown to
  one, logged with their size and digest and counted
  (`wa_server_webhook_events_total{audience="operator"}`): events of a
  number or WABA no tenant holds, a field the library does not type
  (`unknown`), a signed body that is not a webhook (`unparsed`), partner
  solution updates, any event type the service has not reviewed yet
  (today Conversation Routing's `standby_observed` and
  `thread_control_changed`, and `user_action_reported`, a marketing
  message's click),
  events Meta dated before the WABA's attaching, and replays (dated more
  than 7 days and an hour ago, what the dedup markers remember). A rising
  count usually means a WABA is subscribed but not attached.
- `200` once every event is recorded; `500` when recording failed: Meta
  retries the batch at once, then with decreasing frequency for up to 7
  days (`webhooks/create-webhook-endpoint`), the events already recorded
  are acknowledged as duplicates, and neither the inbox nor the outbox
  records one twice. An event that fails every time (a permanent sink
  error) holds its whole batch back for those 7 days, after which Meta
  drops it: the events after it in the same body are lost with it
  ([OPEN_QUESTIONS.md](../../OPEN_QUESTIONS.md) #30; watch
  `wa_server_webhook_sink_failures_total`).
- Errors and bodies that are not webhooks carry no id: they are told
  apart by the body they came in and their place in it, for an hour
  (design D23, a coordinator's decision the owner may change). This
  assumes Meta redelivers the same bytes, which Meta does not document.
  The same body again after that hour is recorded again, as a new event
  with its own id (the same error can legitimately recur): an outage
  longer than an hour, the database answering `500` while Meta retries,
  records the batch's errors twice, under new ids.

**3. Poll the events** with a key holding the `events` scope:

```bash
curl -sS "http://127.0.0.1:8081/v1/events?limit=100" -H "Authorization: Bearer $KEY"
curl -sS "http://127.0.0.1:8081/v1/events?after=18342&types=message_received,status_updated" \
  -H "Authorization: Bearer $KEY"
```

The answer is `{"data": [...], "next_after": 18350}`. Store `next_after`
and pass it as `after` next time: it is the last event's `sequence` when
more follow (poll again at once), else the tenant's newest sequence (wait
a little). Each tenant has its own sequence, increasing (with gaps)
for its events alone; a tenant created again under a deleted tenant's id
goes on after the deleted one's last sequence. Omitting `after` starts
at the oldest event kept. Each event is an envelope:

```json
{"id": "evt_3f9c…", "sequence": 18342, "type": "message_received", "api_version": "v1",
 "tenant_id": "merchant-42", "phone_number_id": "106540352242922", "waba_id": "102290129340398",
 "received_at": "2026-09-25T10:00:01Z", "truncated": false,
 "data": {"event": "message_received", "message": {"id": "wamid.…", "type": "text", "…": "…"}}}
```

`data` is meta-whatsapp-rs's `WebhookEvent` JSON (Meta's fields,
normalized; key customers by `contact.user_id`, the BSUID, since
`wa_id` may be absent). Deduplicate on `id` (an event recorded again,
Meta's late retry or a replay, keeps its id, unless `WA_APP_SECRET` was
rotated in between: ids are derived with it) and order on `sequence`;
what your backend does per message (an order confirmation), make
idempotent on the message id too (`data.message.id`), as a last guard.
Filter with `types` (comma-separated, or repeated) and `phone_number_id`;
a page stops before 8 MiB of `data` (history syncs are large). A
filtered poll's `next_after` moves past the events the filter left out
(to the tenant's newest sequence when no more match), so keep one cursor
per filter set (`types`, `phone_number_id`): a cursor saved by one
filter skips, for another, what the first left out. A cursor
older than what was purged, by retention or with a deleted tenant of the
same id, is `410 cursor_expired`: resynchronise (from the inbox, when it
lands in M2) and start again without `after`. A cursor past the tenant's
newest sequence (a restored database) is `422` on `after`.

**Restoring the database.** A point-in-time restore rolls the tenants'
sequences back: the events recorded after it are numbered again from
the restore point, so a sequence an integrator already saw can name
another event. The `422` above only catches a cursor still ahead when
it polls; one that polls after new events have passed it skips them
without an error. After a restore, tell every integrator to resynchronise
and reset their cursors (poll without `after`), as after `410`.

Events are kept `WA_SERVER_OUTBOX_RETENTION` (7 days by default, until
the owner's retention decision D10); every replica runs housekeeping
every 10 minutes, one at a time, which also deletes the expired webhook
dedup markers. Deleting a tenant deletes its events (design D22, a
coordinator's decision the owner may still change, like the default
above), and takes it out of every platform key's allowed tenants (D24,
likewise): a tenant created again with the same id starts with neither.
No route or command edits a platform key's allowed tenants, so mint a new
platform key for a tenant created again (a key allowing every tenant,
`*`, allows it at once).

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
  `reconnect_required`: later calls on those numbers answer `409
  reconnect_required` without asking Meta, until the WABA is attached
  (or onboarded) again (a WABA route, templates say, asks Meta and gets
  the same answer). It is `409` on a route naming a media or template id
  too: a dead token is never taken for a missing object, and neither is
  Meta failing (a `5xx` stays `502`).

## Operations

- `/livez` answers while the process runs; `/readyz` fails when Postgres
  does not answer or after `SIGTERM` (the listeners stop accepting at the
  same moment: keep a preStop delay in Kubernetes if the load balancer
  must see the replica unready first).
- Limits, per listener: a request head must arrive within 10 s (slow
  clients are cut off), 256 connections on the public listener and
  4,096 on the internal one (more wait), and every request is answered
  within 55 s (past it, `504 timeout` with `may_have_been_sent: true`).
- `/metrics` (Prometheus) counts requests by listener, method, route
  template, status and error code, their duration, failed Graph calls by
  code, idempotent repeats by outcome (`replayed`, `reused`,
  `in_progress`, `outcome_unknown`), rate-limited requests by class,
  Meta's deliveries by outcome (`delivered`, `unauthenticated`,
  `payload_too_large`, `slow_body`, `busy`: the replica at capacity,
  `in_flight`: a run of them means recording outlasts the 60 s lease,
  `failed`), recorded events by type and audience (`tenant`,
  `operator`), duplicates and recording failures by stage; never an id,
  a number or a key.
- Expired idempotency records are purged every 10 minutes by one replica
  at a time, with the outbox's housekeeping.
- Logs are JSON, one line per request with its id (`X-Request-Id`, echoed
  or generated), route template (never the raw path), tenant, the public
  id of the key that made it, status and duration; every change an
  operator makes is also an `audit` event (action, admin key id, the
  tenant, key or WABA touched), and so is a tenant's template deletion
  (with the key's id). No secret, token, message text or phone number is
  logged, and no webhook body or event content (an operator-only event
  is logged by type, size and digest); refused deliveries at most once a
  minute per reason; Meta's error texts only at `debug`.
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

The inbox routes, `GET /v1/events/{id}`, SSE, webhooks-out and the
service's own number events (M2); Embedded Signup,
disconnection by Meta's webhooks, coexistence sync and OTP (M3); the
Docker image, a Compose file, the documents route and the TypeScript
client (M4). Tenant settings (OTP sender and template, limits) in `PATCH
/v1/admin/tenants/{id}` come with OTP (M3); until then the rate limits
are the deployment's, for every tenant. The design's TOML file of
non-secrets (`WA_SERVER_CONFIG`) is not read (set, it stops the start),
and OpenTelemetry export is not wired. The `/v1/version` revision reads
`unknown` unless the build sets `META_WHATSAPP_RS_REVISION`.
