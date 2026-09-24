# wa-rs architecture

This is the spec. Code that disagrees with it is a bug in one of the two;
fix whichever is wrong, in the same PR. For task-oriented integration
walkthroughs (Meta setup, onboarding, inbox, OTP, production), see the
[integrator guides](guides/README.md).

## Goals

A Rust toolkit for Meta's WhatsApp Business Platform that serves three
concrete products:

1. **E-commerce marketing** — templates (create, get approved, send), the
   Marketing Messages API, In-App Signup opt-ins, catalogs and product
   messages, delivery/read analytics, respecting opt-outs (`131050`) and
   per-user marketing limits (`131049`).
2. **CMS in-app chat** — each merchant onboards *their own* WhatsApp number
   through **Embedded Signup**; their customers' messages arrive by
   webhook, are stored per conversation, streamed live to the merchant's
   inbox UI, and answered with the merchant's business token, inside the
   24-hour customer service window (templates outside it).
3. **Authentication** — OTP over authentication templates (copy code,
   one-tap, zero-tap), issued, stored hashed, verified with attempt limits.

## Layout

```
crates/
  wa-core       error tree, ids, config, ports (traits). No I/O, no runtime.
  wa-client     Graph API client; one module per endpoint family.
  wa-webhooks   verify, parse, normalize, dedup, dispatch; axum router (feature).
  wa-adapters   port implementations: reqwest, memory/Postgres/Redis stores, sinks.
  wa-typst      Typst → PDF/PNG for document and image messages.
  wa-rs         facade: re-exports, prelude, `client(token)`, the CMS inbox,
                feature flags, runnable examples. What integrators depend on.
.xtask          repo automation (`cargo xtask meta-docs`): a workspace of its own,
                with its own Cargo.lock, excluded from the root one, so its ureq
                (rustls with `ring`) never enters a library or test build.
```

Dependency rule: everything depends on `wa-core`; nothing depends on
`wa-rs`; `wa-client` and `wa-webhooks` never depend on each other or on
`wa-adapters` (except as a dev-dependency for tests). An adapter never
leaks its library's types through a port.

## Ports (`wa-core`)

| Port | Methods | Contract lives in |
| --- | --- | --- |
| `transport::HttpTransport` | `send`, `send_streaming` | rustdoc; non-2xx is *not* an error at this layer |
| `store::KvStore` | `get`, `put`, `put_if_absent`, `compare_and_swap`, `delete` | `wa_adapters::store::conformance` (executable) |
| `store::ConversationStore` | `append`, `append_synced` (a batch of coexistence history: no window, never unread), `fill_media_placeholder`, `revoke` (number and direction scoped; a tombstone when the message is not stored yet), `update_status` (scoped: `phone_number_id, id, status, at, error`), `messages`, `conversations`, `mark_read`, `last_inbound_at` | `wa_adapters::store::conversation_conformance` (executable) |
| `sink::EventSink<E>` | `deliver` | rustdoc |
| `clock::Clock` | `now` | — |

Typed stores are built **on `KvStore`**, never as new ports: token vault,
OTP challenges, webhook dedup, Embedded Signup sessions. An adapter author
implements five methods once and every feature works.

## Error tree

`wa_core::Error` is the root; every public fallible function returns
`wa_core::Result<T>`. See `crates/wa-core/src/error/mod.rs` for the tree.

- Every public fallible function returns `wa_core::Result<T>`, with one
  exception: pure, I/O-free functions (signature/token verification,
  Flows endpoint crypto, Typst rendering, and every public `validate()`)
  may return their precise error (`CryptoError`, `WebhookError`,
  `wa_typst::RenderError`, `Result<(), ValidationError>`); `?` lifts them
  into `wa_core::Error`.
- `thiserror` for every typed node. `anyhow::Error` only as the opaque leaf
  for failures raised by code we do not own (adapters, integrators):
  `TransportError::{Connect, Backend}`, `StorageError::Backend`,
  `SinkError::Delivery`, `Error::Other`.
- Branch on `Error::kind()` → `ErrorKind` (classified from Graph error
  `code`, per Meta's guidance) and `Error::is_retryable()`. Never on message
  text, HTTP status, or subcode. One local refusal has a Graph kind: the
  inbox's closed-window refusal
  (`ValidationError::customer_service_window_closed()`) is
  `CustomerServiceWindowClosed`, like Meta's `131047`, so one condition has
  one kind.
- Multi-step flows (Embedded Signup onboarding) wrap failures with
  `Error::in_step("stable_step_name")` so callers know how far they got.
- Examples and binaries use `anyhow::Result` at the edge.
- No module adds a variant to the root for its own convenience; a new leaf
  needs a reason in this document.

## Client (`wa-client`)

`Client` = `Arc<Shared{transport, endpoint, retry, timeout, user_agent}>` +
optional `AccessToken`. `client.with_token(t)` is the multi-tenant switch.

Endpoint modules follow one pattern (`src/messages/mod.rs`, abridged):

```rust
impl Client { pub fn messages(&self, id: impl Into<PhoneNumberId>) -> Messages }
impl Messages {
    pub async fn send(&self, message: &OutboundMessage) -> Result<SendResponse> {
        message.validate()?; // documented limits, before any request (rule 3)
        self.client.post_at(&[self.phone_number_id.as_str(), "messages"])
            .json(message)
            .context(SEND_CONTEXT)
            .send_private() // the response names the recipient: see rule 8
            .await
    }
}
```

Rules for every endpoint module:

1. **Build requests only through `GraphRequest`** (`client.get_at/post_at/
   delete_at(&[segments])` for any path with an id; `client.get/post/delete`
   for literal paths only). It owns auth, retries, error decoding, the
   credential host allowlist, and segment-safe paths (an id containing `/`,
   `?` or `#` stays inside its segment; empty, `.` and `..` are refused).
2. **Requests are typed structs with `Serialize`, responses typed with
   `Deserialize`.** Unknown response fields are ignored (never
   `deny_unknown_fields`); enums Meta may extend get a catch-all variant
   so a new value never breaks parsing. Which name and shape all of them
   should share, and the `#[non_exhaustive]` policy, are open
   (`OPEN_QUESTIONS.md`). Until decided, follow the module you are in, and
   add no new unit `#[serde(other)] Unknown` variants: they drop Meta's
   value, and a type that also derives `Serialize` writes its own name
   (e.g. `"UNKNOWN"`) back instead.
3. **Validate locally what Meta documents as a hard limit** (lengths,
   counts, formats) and return `ValidationError` naming the field. Do not
   invent limits the docs do not state.
4. **Idempotency**: POSTs are non-idempotent by default. Mark a POST
   `.idempotent(true)` only when replaying it cannot duplicate an effect
   (e.g. setting a field to a value).
5. **Lists** return `Page<T>` and take their cursors in their query
   struct (`after`, `before`: the next page is the same query with
   `after = page.next_cursor()`); each offers a `…_stream()` via
   `GraphRequest::paginate`, which manages the cursors and refuses a query
   that already holds one (a `ValidationError`, the stream's single item).
   A list whose page documents no pagination (`waba.subscribed_apps`,
   `templates.library`) takes no cursor and says so.
6. **Tests** use `wa_core::testing::ScriptedTransport`: assert method, path,
   query, auth header and exact JSON body; feed responses copied from the
   docs' examples. Every test that scripts N responses asserts
   `remaining() == 0`.
7. **Docs**: every public item has rustdoc; each module doc names the Meta
   doc paths it implements (relative to
   `https://developers.facebook.com/documentation/business-messaging/whatsapp/`).
8. **Responses that name people** (the send responses of `Messages::send`
   and `Marketing::send` echo the recipient's number) decode with
   `GraphRequest::send_private`: a decode error carries a placeholder
   instead of the body snippet, and the error category and position instead
   of serde's message (which quotes the offending value).
9. **One type per concept.** A type two endpoint families share is
   defined once in `wa_client::common` (`MediaSource`, `FlowAction`,
   `QualityRating`) and re-exported by each module that uses it; two
   types of one module never share a name with a type of another
   (`flows::endpoint::EndpointAction` is the endpoint request's `action`,
   `common::FlowAction` the `flow_action` a Flow starts with). New code
   types every Graph id with a `wa_core::ids` newtype. Known exceptions,
   still `String` (typing them is a breaking change each): the groups'
   `request_id` and `join_request_id`; `MessagingCustomerBase::id`,
   `CreatedMessagingCustomerBase::messaging_customer_base_id` and the
   signups' `default_messaging_customer_base_id`; `OnboardingRequested::request_id`;
   the calling settings' `app_id`; `SubscribedAppData::id` and
   `AssignedUser::id`; `CommerceSettings::id`; `LibraryTemplate::id`; the
   template's `ad_*_id`s; `preverified_id`; the phone number
   `request_id`; the ids of an Embedded Signup session event
   (`ad_account_ids`, `page_ids`, `dataset_ids`, `catalog_ids`,
   `instagram_account_ids`, `session_id`), the launch's `solution_id` and
   the token's `user_id`. Merchant-chosen ids (product retailer ids,
   button ids) and our own (an OTP challenge id) are strings on purpose.
10. **List queries are named `List*`** (`ListQrCodes`, `ListSignups`,
   `ListFlows`, `ListFlowAssets`, `ListAssignedUsers`, `ListClientWabas`,
   `ListGroups`, `ListBlockedUsers`, …) and carry `after`/`before`; the
   one-page method sends them, the `…_stream(&query)` refuses them. The
   older `*Query` names stay as they are (renaming them would break
   integrators for no gain): `PhoneNumbersQuery`, `WabaListQuery`,
   `TemplateListQuery`, `LibraryQuery`, `TemplateAnalyticsQuery`,
   `TemplateGroupAnalyticsQuery`, `GroupAnalyticsQuery`.

### Retries

`RetryPolicy` (`src/retry.rs`): idempotent requests retry on any retryable
error; non-idempotent requests only when the error proves Meta rejected the
request before processing (throttling). A timeout on a send is never
replayed — a duplicate OTP or order confirmation is worse than an error.

### Credentials never leave Meta

`GraphRequest` attaches a token to exactly two origins: the configured
Graph endpoint (same scheme, host and port; `https://graph.facebook.com`
unless `ClientBuilder::endpoint` names a proxy, in which case production
Graph is just another host) and `https://lookaside.fbsbx.com` on the default
port, where media download URLs point. Any other URL with a token (another
Meta host such as `*.whatsapp.net` or `*.fbcdn.net`, a subdomain, a
look-alike — the host is compared exactly after URL parsing, so suffixes,
trailing dots and IDN homographs do not match —, another port, plain HTTP)
fails with `ValidationError` on `url` before the transport sees it. Inside the
library, the only request to an absolute URL is `Media::download_with_info`;
integrators reach the same check through `Client::request_url`. Pagination
re-issues the original request with `after=` rather than following
`paging.next`.

## Feature modules

### Messages (`wa_client::messages`)

`OutboundMessage { recipient: Recipient (flattened), context?, biz_opaque_callback_data?, category? (Direct Send), ttl_seconds? (Direct Send), direct_send_config? (Direct Send), content: MessageContent }`
where `MessageContent` is an internally tagged enum on `type`: text, image,
audio, video, document, sticker, location, contacts, interactive (button,
list, cta_url, location_request_message, flow, product, product_list,
catalog_message, carousel, voice_call, call_permission_request,
request_contact_info, address_message), reaction, pin/unpin, template, and a
`Raw` escape hatch that refuses envelope keys. Constructors cover the common
cases (`OutboundMessage::text(to, body)`, …). Every documented limit is
checked locally before sending; templates run `TemplateMessage::validate`.
`mark_read(message_id)` (idempotent) and
`mark_read_with_typing_indicator(message_id)` (not replayed: a late retry
could show "typing…" after the reply). `SendResponse { messaging_product,
contacts: [{input, wa_id?, user_id?, parent_user_id?}], messages: [{id,
group_id?, message_status?}] }` — shared with the MM API.

Recipient addressing follows the BSUID rules in `wa_core::recipient`.

### Media (`wa_client::media`)

Upload (multipart: `file`, `type`, `messaging_product`; MIME type and size
checked first), `url(media_id)` → `{url, mime_type, sha256, file_size, id}`,
`download(media_id)` → streaming body with a `verified()` adaptor that
hashes while streaming (a mismatch is `TransportError::Integrity`,
retryable), `download_bytes` with a size cap (pre-allocation never trusts
Meta's reported size beyond 16 MiB), `delete`. Resumable Upload API
(`/{app_id}/uploads` → `upload:<id>` sessions, `file_offset`, `Authorization:
OAuth` on every step so the token never sits in a URL) returning a
`wa_core::ids::UploadHandle` for template `header_handle`s.

### Templates (`wa_client::templates`)

CRUD + library + migrate + compare. Two builder families that must not be
confused:

- **Definition** (creating/editing a template): `TemplateDefinition { name,
  language, category, parameter_format, components: [Header|Body|Footer|Buttons|Carousel|LimitedTimeOffer…] }` with examples.
- **Invocation** (sending): `TemplateMessage { name, language, components:
  [header params, body params, button params by index/sub_type] }`, embedded
  in `MessageContent::Template`.

Named and positional parameters are both supported (`parameter_format`).

### Authentication (`wa_client::authentication`)

Authentication template definitions (copy code, one-tap with
`supported_apps` package/signature hash, zero-tap with
`zero_tap_terms_accepted`), `add_security_recommendation`,
`code_expiration_minutes`, previews, bulk upsert.

`OtpService` on `KvStore` + `Clock`:

- Recipients are **strict E.164 with `+`**. Meta prepends the sending
  number's country code to a number without `+`, so a digits-only key would
  let `12015553931` (delivered to `+91 12015553931`) verify `+12015553931` —
  an account takeover. The code is sent to exactly `+<digits>` and keyed by
  those digits. BSUID-only recipients are refused locally (OTP buttons need
  a phone number; Meta's `131062`).
- Challenges are **scoped to the service**: the store key is
  HMAC(pepper, `"wa.otp.key" | scope | digits | purpose`), where the scope is
  the sending `phone_number_id` and the required `OtpConfig::namespace`
  (the tenant; `OtpConfig::new(namespace)`, no `Default`),
  netstring-encoded so neither can be shifted into the other. Services
  sharing a store and a pepper (several merchants of one integrator) never
  see each other's codes, cooldowns or issue limits, on their own numbers
  or on a shared one. A blank namespace (or one with edge whitespace or
  control characters) is a config error; changing the
  scope (or upgrading across the commit that introduced it) invalidates
  outstanding codes. Making the namespace required kept the encoding: a
  service that had set one derives the same keys.
- `issue(recipient, purpose) → IssueOutcome { Sent(Challenge{id, expires_at,
  message_id}), CoolingDown{retry_after}, RateLimited{retry_after} }`:
  CSPRNG numeric code (length 4–8), only an HMAC-SHA256 of it stored under a
  server pepper (`SecretBytes`): HMAC(pepper, `"wa.otp.code" | key |
  challenge id | code`), so a record copied to another key (by anyone who
  can write the store but lacks the pepper) never verifies there. Keys are
  HMACs too (no raw phone number in the store); the namespace and the
  purpose are server-side constants, never request input. The code goes out through `Messages::send`
  (`OutboundMessage::template`), so the send checks and the private
  response decoding apply. Resend cooldown (30 s) and a per-recipient
  issue limit (default 5 per sliding hour per number and purpose, per
  service scope; opting out is explicit) bound brute force to ~0.06 %/day
  for 6 digits. A challenge is removed only
  when the send was provably rejected (4xx, throttling); after a timeout or
  5xx it stays, because the code may have been delivered. The line is
  `Error::may_have_been_sent`, the one integrators use for their own
  sends.
- `verify(recipient, purpose, code) → VerifyOutcome { Verified, Invalid {
  attempts_left }, Expired, TooManyAttempts, NotFound }`: constant-time
  compare; attempts counted with `compare_and_swap` before comparing, so
  concurrent guesses cannot exceed the limit; a verified challenge is
  consumed (single use).
- The code never appears in logs, errors, or `Debug` output.

### Embedded Signup (`wa_client::embedded_signup`)

The most important flow. Frontend (Facebook JS SDK) runs `FB.login` with
the app's configuration id; the page receives a `WA_EMBEDDED_SIGNUP`
message event (`FINISH` with `waba_id`, `phone_number_id`, `business_id`;
`CANCEL` with `current_step`; `ERROR`) and a short-lived `code`. The backend:

1. `exchange_code(code)` → business integration system user token
   (`GET /oauth/access_token?client_id&client_secret&code`, no bearer).
2. Optionally `debug_token` (app token) to confirm granular scopes and the
   WABA ids the token can reach — used when the session info is missing.
3. `waba(waba_id).subscribe_app(Option<&CallbackOverride>)`
   (`POST /{waba}/subscribed_apps`) so webhooks flow; `Some` sends this
   WABA's webhooks to another callback.
4. `phone_number(id).register(&TwoStepPin, Option<&DataLocalizationRegion>)`
   for Cloud API numbers (two-step PIN; optional local storage).
5. Persist the token in `TokenVault` (on `KvStore`, **encrypted at rest**
   with AES-256-GCM under an integrator-supplied key, key id recorded for
   rotation) keyed by WABA id, with a phone-number → WABA index.

`EmbeddedSignup::onboard(&request, &vault) → Onboarded` runs, in order:
exchange code → `debug_token` → **verify** that the WABA id from the browser
event is among the token's grants and that the phone number belongs to that
WABA (browser-supplied ids are never trusted — otherwise one merchant could
overwrite another's vault entry) → **store** the token → subscribe app →
register number. The token is stored *before* the fallible later steps
because the code is single-use and short-lived: storing last would lose the
token whenever subscribe or register fails (e.g. wrong PIN, `133005`).
`verify_assets` also reads the WABA's owning business from Meta
(`owner_business_info`); a `business_id` claimed by the browser is ignored
(it is what credit-line sharing keys on). Every number Meta lists on the
WABA is stored in the phone → WABA index, so onboarding a second number
never unroutes the first. `resume(&waba_id, &request, &vault)` loads the
stored token and reruns subscribe/register, refusing a session that names
another WABA or an unverified number; `request.code` is not used, but an
`OnboardingRequest` cannot be built without one, so after a restart a
caller passes a placeholder (`OPEN_QUESTIONS.md` #10). `SignupSessions::redeem(state, tenant)` checks the tenant
inside the library, is single-use, and does not consume the state on a
tenant mismatch. Each failure is
`Error::in_step("exchange_code" | "debug_token" | "verify_assets" |
"store_token" | "subscribe_app" | "register_phone")`. **Retrying the whole
`onboard` call is not safe** (the code is spent); retry with `resume()`.
`LaunchOptions` builds the JSON for `FB.login` `extras` (version,
`featureType` — incl. coexistence `whatsapp_business_app_onboarding`,
`setup` pre-fill). `SessionInfo` parses the message event.
`SignupSessions` (on `KvStore`) binds an opaque state id to the merchant that
started the flow, so a callback can't be attributed to another tenant.

Coexistence: `smb_app_data` sync (contacts, history) within 24h.

### In-App Signup (`wa_client::signups`), WABA (`waba`), phone numbers (`phone_numbers`), business profile, QR codes, block users, commerce, analytics, marketing (MM API), flows, groups, calling

Typed endpoint wrappers per the rules above. Flows additionally provides
the data-endpoint crypto (feature `flows-endpoint`): decrypt
`{encrypted_flow_data, encrypted_aes_key, initial_vector}` with the business
RSA private key (RSA-OAEP-SHA256 → AES-128-GCM), and encrypt the response
with the same key and the bit-flipped IV. RSA goes through `aws-lc-rs`
(constant-time), never the `rsa` crate (RUSTSEC-2023-0071, Marvin timing
attack, unfixed) — the endpoint decrypts attacker-supplied ciphertext.

## Webhooks (`wa-webhooks`)

```
POST body ─► verify X-Hub-Signature-256 (HMAC-SHA256, constant-time, any of N app secrets)
         ─► parse WebhookPayload{object, entry[{id, time?, changes[{field, value}]}]}
         ─► normalize into Vec<WebhookEvent> (one per message/status/change)
         ─► optional DedupGuard: lease a claim (pending, 60 s) → EventSink<WebhookEvent>
             → confirm (done, TTL 7 days + 1 h — Meta retries for 7) or release on sink error
```

Dedup is a **lease, not a marker**: a marker written before delivery would
swallow Meta's retry if the request died between marking and delivering
(timeout, crash, deploy), losing the event for 7 days. A retry that finds a
live lease gets `WebhookError::ClaimInFlight` (answered `503`, Meta retries
later). Every claim transition is a version-checked `compare_and_swap`, so an
expired lease can't overwrite a newer claim. Dedup keys are hashed before
they reach the store (group status keys contain participants' phone numbers).
Logs carry sizes, digests and field names only — never payload values.

- `verify_subscription(query, &VerifyToken) -> Result<String /*challenge*/>`
  (constant-time token compare, `hub.mode == "subscribe"`).
- Typed values for every documented field (`messages` — all inbound types,
  statuses with pricing, errors; template status/quality/category/components
  updates; phone number name/quality; account update/review/alerts;
  business capability; security; user preferences (marketing opt-out);
  history, smb app state sync, smb message echoes (coexistence); partner
  solutions; payment configuration; calls; flows; groups;
  business_username_updates, user_id_update). Unknown fields and unknown
  message types parse into `Unknown { … raw: serde_json::Value }` — **a new
  Meta field must never fail a delivery**.
- `WebhookEvent` carries `waba_id`, `phone_number_id` (when the field has
  one), and the user identity (`wa_id?`, `user_id?` BSUID, `parent_user_id?`,
  `username?`).
- A body that verifies but fails to parse is acknowledged (so Meta stops
  retrying for 7 days) and surfaced as `WebhookEvent::Unparsed{raw}` +
  a `tracing::error!` carrying only size and digest. A bad signature is
  rejected (401); blank app secrets and blank verify tokens fail closed.
  Default body limit: 3 MiB (Meta: payloads up to 3 MB).
- `axum` feature: `router(handler)` with `GET` verify + `POST` receive
  (raw bytes, body limit), and an SSE helper that turns a broadcast
  subscription (filtered by phone number id) into `text/event-stream` for
  a live inbox. The `POST` route checks that `X-Hub-Signature-256` is
  present and well-formed (`sha256=` + 64 hex; no secret or body needed)
  before reading the body, so an unsigned request is a `401` without being
  buffered; `SIGNATURE_HEADER` is defined outside the `axum` feature for
  framework-free integrations. Every SSE subscriber's broadcast receiver
  clones every event before its filter runs (`OPEN_QUESTIONS.md` #31).

## Adapters (`wa-adapters`)

- `http::ReqwestTransport` (feature `reqwest`, rustls): streaming download,
  multipart, per-request timeout, error mapping (timeout → `Timeout`,
  connect → `Connect`, else `Backend`).
- `store::{MemoryKvStore, MemoryConversationStore}` (feature `memory`).
- `store::{PostgresKvStore, PostgresConversationStore}` (feature
  `postgres`, sqlx, embedded migrations, `wa_` table prefix configurable).
- `store::RedisKvStore` (feature `redis`; CAS via Lua). Requires the
  `noeviction` policy: every key with a TTL here enforces a limit (OTP
  issue logs, dedup markers), and Redis evicts silently.
- `sink::{ChannelSink, BroadcastSink, FanoutSink, FilterSink, FnSink,
  TracingSink}` — feature `sinks`, generic over the event type, `Debug`
  redacted. (The inbox sink needs webhook event types, so it lives in the
  facade: `wa_rs::inbox::InboxSink`.)
- Postgres: one table-wide version sequence (versions never reused, even
  after purge); deleted keys leave marker rows purged after 10 minutes;
  migrations are templates with a validated table prefix and a per-prefix
  history table, applied under a database-wide lock. **Limitation:** Postgres
  cannot store U+0000; the stores refuse a value containing it (memory and
  Redis accept it). `InboxSink` and `Inbox::send` replace U+0000 with
  U+FFFD in the content they record (kind, text, payload, status error;
  ids and contacts are stored as Meta sent them, so a NUL there is still
  refused). Lossy, and provisional: `OPEN_QUESTIONS.md` #18 records it as a
  maintainer decision taken without asking. `messages.id` is the primary key on its own, so
  a message id is stored once per store whatever the business number (the
  memory store does the same; `OPEN_QUESTIONS.md` #33).
- Redis: per-namespace version counter (a per-key counter would leak one key
  per dedup marker forever); every mutating op is one Lua script. **TLS**
  (`rediss://`) is not wired: redis-rs uses the process-wide rustls provider,
  which panics when both aws-lc-rs and ring are linked — pass your own
  connection with a provider installed at startup.
- reqwest: errors never carry the request URL; redirects send no `Referer`
  (it would carry the query to the redirect target); `HTTP(S)_PROXY` is
  honoured.
- Live tests are named `live_*`, read `WA_RS_TEST_POSTGRES_URL` /
  `WA_RS_TEST_REDIS_URL`, skip when unset, and **fail** when unset under
  `WA_RS_REQUIRE_LIVE=1` (`just test-live` sets it). Every `KvStore`
  adapter runs `store::conformance`, every `ConversationStore` adapter runs
  `store::conversation_conformance` (including concurrency, collation and
  real-time expiry under load).

## CMS inbox (`wa_rs::inbox`)

`InboxSink` (an `EventSink<WebhookEvent>`) records inbound messages,
status updates, and coexistence echoes and history into a
`ConversationStore`; `Inbox` (one per merchant phone
number, built with that merchant's token) lists conversations and history,
exposes the 24-hour `CustomerServiceWindow`, and sends replies.

- The library knows WABAs and phone numbers, not the integrator's tenants.
  Whoever builds an `Inbox` for a request first checks that the
  authenticated tenant owns that phone number, *before* reading the token
  vault (whose tokens belong to every merchant). The `cms_inbox` and
  `embedded_signup` examples show where: a bearer-token stand-in for the
  CMS's sessions, the tenant taken only from it, and a tenant → phone
  number table.
- Keys: business phone number id + contact, where contact is the BSUID
  when Meta sent one, else the `wa_id`, else (group messages) the group id.
- Replies to a `wa_id` go to `+<wa_id>` (Meta prepends the business
  number's country code to numbers without `+`). `Inbox::send` refuses a
  message not addressed to the conversation's contact.
- Free-form replies outside the window are refused locally
  (`ValidationError::customer_service_window_closed()`, kind
  `CustomerServiceWindowClosed`); templates and Direct Send are exempt. The
  window is computed from recorded inbound *messages*: a customer's call,
  which reopens it on Meta's side, is not seen (`OPEN_QUESTIONS.md` #32).
- Statuses and revokes change only a message of the business number they
  arrived on (`update_status` takes the `phone_number_id`); a revoke also
  only a message of its direction (`ConversationStore::revoke`: a customer
  revokes what they sent, the business what it sent).
- Content never fails a delivery: U+0000 in recorded content is stored as
  U+FFFD (see Adapters). Storage errors still do (500, Meta redelivers the
  batch).
- A storage (or serialization) failure *after* a successful send is
  logged without the message's content, never returned — an error would
  invite a retry that sends twice.
- Revokes mark the original `Deleted` (its content is kept,
  `OPEN_QUESTIONS.md` #38). A revoke that arrives before its message
  stores a tombstone (kind `revoked`, no content, `Deleted`, no window,
  not unread) under the message's id, so the message, live or synced, is
  never stored with the content its sender deleted.
- Coexistence (a merchant who keeps the WhatsApp Business app): echoes
  (`MessageEchoed`, messages the merchant sent from the app) are recorded
  as `Outbound`, status `Sent` (the payload has none), in the customer's
  conversation (BSUID, else `wa_id`/`to` without `+`); an echoed revoke
  deletes the original. Synced history (`HistorySynced`) is recorded
  message by message in its documented direction (`from` = the business
  number → `Outbound` with its `history_context` status; else `Inbound`),
  each with its own device timestamp (bounded by the sink's clock plus 5
  minutes, so a phone with a wrong clock cannot pin a conversation to the
  top), so chunks may arrive in any order and a redelivered one is a no-op
  (ids are stored once); the exception is a revoke in a chunk that arrives
  before the chunk carrying its message, which leaves a tombstone in the
  message's place. Each chunk is one `append_synced` batch (the Postgres
  adapter: one statement), then its revokes: a history webhook can carry
  thousands of messages, and one round trip each could outlast the
  webhook's dedup lease. A declined sync
  (`2593109`) records nothing. One malformed history item never fails the
  delivery: an item without a direction or customer, with U+0000 in an
  id, or that does not parse on its own (when one bad item turned the
  whole value into `WebhookEvent::Unknown`) is skipped and logged by
  position, without content; storage errors still fail it.
  Synced history goes through `ConversationStore::append_synced`: an
  inbound synced message neither moves `last_inbound_at` (Meta opens no
  window for a message sent before onboarding,
  `embedded-signup/onboarding-business-app-users`) nor counts as unread
  (the merchant read it in the app). The media content Meta sends after a
  `media_placeholder` (`history`'s `messages` / `message_echoes`) replaces
  the placeholder's kind, text and payload through
  `ConversationStore::fill_media_placeholder`; the row keeps the thread's
  conversation, direction, status and timestamp. Both are part of the
  executable conformance suite, so every adapter proves them. The Postgres
  adapter needs no schema change: the summary is maintained when a row is
  written, so the difference lives in the write.

## Typst (`wa-typst`)

Render a Typst source with JSON inputs (`sys.inputs.data`, read with
`json(bytes(sys.inputs.data))`) to PDF or PNG, with bundled fonts so output
is byte-identical on every machine. `today()` comes only from
`Renderer::with_today` (never the system clock). The world is a sandbox: no
file reads beyond the main source, no packages, no network; PNG size is
bounded (`MAX_PNG_PIXELS`) and ppi validated before any allocation; the main
file is always `/main.typ` (typst interns file ids process-wide). Ships templates for
e-commerce: `invoice`, `receipt` (order confirmation), `voucher` (coupon /
gift card image for marketing headers). Output is bytes + MIME + filename,
ready for `media().upload()` and a document/image message or template
header. Never renders OTP codes (those go through authentication
templates only).

## Verification

`just ci` is the gate (lint, check, test, doc, per-feature builds, deny,
live adapter tests). CI runs exactly it. See `AGENTS.md`.
