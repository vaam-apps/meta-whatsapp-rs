# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/) once published.

## [Unreleased] — 0.1.0

First feature set. See [docs/coverage.md](docs/coverage.md) for the full
matrix and [OPEN_QUESTIONS.md](OPEN_QUESTIONS.md) for decisions still open.

### Added

- **Coexistence in the CMS inbox**: `InboxSink` records the merchant's
  WhatsApp Business app messages (`MessageEchoed`: outbound, `Sent`, in the
  customer's conversation; an echoed revoke deletes the original) and the
  synchronized chat history (`HistorySynced`: each message in its documented
  direction and status, idempotent by message id, chunks in any order, a
  declined sync records nothing). One malformed history item is skipped
  and logged by position instead of failing the delivery, including when
  it made the whole `history` value arrive as `WebhookEvent::Unknown`.
  Synced inbound history still counts towards the local window and unread
  count, and media contents are not merged into recorded placeholders:
  both need a `ConversationStore` port change (`OPEN_QUESTIONS.md` #35).
- **Adoption helpers**: `Error::may_have_been_sent()` (whether a failed send
  could still have been delivered — the line between "fix and resend" and
  "reconcile first"), `Inbox::window_is_open` (the reply window by the
  inbox's own clock, the same check `reply` makes), a `testing` feature on
  `wa-rs` (no second pinned `wa-core` dev-dependency), and a `redis`
  re-export next to the `sqlx` one.
- **Granular consumer skills**: 24 task-shaped skills with compiled example
  files, and a gate (`crates/wa-rs/tests/skills.rs`, `just skills-check`)
  that keeps snippets, API names, links, frontmatter and stamps true. The
  developer skills in `.claude/skills/` are marked internal so
  `npx skills add vaam-apps/wa-rs` offers only the consumer skills.

- **Workspace**: `wa-core` (error tree, ids, ports), `wa-client`,
  `wa-webhooks`, `wa-adapters`, `wa-typst`, and the `wa-rs` facade with a
  prelude and a `client(token)` shortcut. Graph API v25.0 by default.
- **Error tree**: `thiserror` nodes with `anyhow` opaque leaves;
  `ErrorKind` classifies every documented Graph error code; retry policy
  never replays a non-idempotent send on a timeout.
- **Embedded Signup**: launch options, session events, code exchange,
  `debug_token`, verification of browser-supplied ids (grants, phone
  ownership, owner business), encrypted token vault with key rotation and a
  phone → WABA index, tenant-bound signup sessions, `onboard`/`resume`,
  coexistence sync.
- **Webhooks**: verify-token and `X-Hub-Signature-256` checks (multi-secret,
  fail-closed), typed payloads for every documented field, BSUID
  identities, normalized events, leased dedup, PII-free logs, axum router
  and SSE.
- **Messages and media**: every message type incl. interactive, commerce,
  flows, calling and group sends; read receipts and typing indicators;
  verified streaming media downloads; Resumable Upload.
- **Templates and authentication**: management, library, migration, typed
  definition and send-time components; authentication templates; an OTP
  service bound to E.164 numbers with hashed storage, CAS-counted attempts,
  cooldown and a per-number issue limit.
- **Also**: In-App Signup, WABA and phone number management, business
  profile, commerce settings, Flows (management + data-endpoint crypto via
  aws-lc-rs), Marketing Messages API, analytics, QR codes, block users,
  Groups, Calling signalling, Direct Send.
- **CMS inbox** (`wa_rs::inbox`): webhook events into a conversation store,
  24-hour-window-checked replies with the merchant's token.
- **Adapters**: reqwest transport; memory, Postgres and Redis stores with
  executable conformance suites; channel, broadcast, fan-out, filter, fn and
  tracing sinks.
- **Typst**: deterministic, sandboxed invoice/receipt/voucher rendering to
  PDF/PNG.
- **Examples**: send a message, CMS inbox server, Embedded Signup server
  (both bearer-authenticated per tenant, on `127.0.0.1` by default), OTP
  login, invoice document.
- **Tooling**: `just ci` gate, CI running it, `cargo xtask meta-docs`,
  Claude Code dev container with a fail-closed default-deny firewall,
  project skills and agents, consumer skills (`npx skills add vaam-apps/wa-rs`).

### Changed

Breaking for anyone pinned to an earlier revision (nothing is released
yet): `ConversationStore::update_status(phone_number_id, id, status, at,
error)`; the `validate()` of `TemplateDefinition`, `TemplateEdit`,
`TemplateMessage`, `AuthenticationTemplate`, `AuthenticationUpsert` and
`OtpConfig` returns `Result<(), ValidationError>` like every other public
`validate()`; `OtpConfig` has a `namespace` field; `.xtask` is a workspace
of its own (`cargo xtask …` still works through the alias). Added:
`ValidationError::{CUSTOMER_SERVICE_WINDOW, customer_service_window_closed,
is_customer_service_window_closed}`,
`MarketingBusiness::client_wabas_with_status_stream`, and
`wa_webhooks::SIGNATURE_HEADER` without the `axum` feature.

Consumer skills: the seven broad skills became 24 task-shaped ones
(`wa-rs` routes to the others; `wa-rs-messaging`, `wa-rs-templates-otp` and
`wa-rs-webhooks` are gone, split into `wa-rs-send-messages`,
`wa-rs-interactive-messages`, `wa-rs-media`, `wa-rs-templates`,
`wa-rs-send-templates`, `wa-rs-otp-login`, `wa-rs-webhook-endpoint`,
`wa-rs-webhook-events`, `wa-rs-live-updates`, …). Each ships compiled,
tested example code; `just ci` checks excerpts, frontmatter, links, names
and stamps (`just skills-check`). The developer skills in `.claude/skills/`
are marked `internal`, so `npx skills add vaam-apps/wa-rs` no longer offers
them.

### Security

Found and fixed during review, before any release: an OTP account takeover
through Meta's country-code prefixing (OTPs now require E.164 with `+`); an
inbox reply to a `wa_id` reaching the wrong person for the same reason;
id path injection into Graph URLs (segment-safe path builder); webhook dedup
markers that could swallow a retried event for 7 days (now a lease);
forged signatures accepted with a blank app secret; `client_secret` leaks via
reqwest error URLs, redirect `Referer` headers and `Debug` output; an
unverified `business_id` stored from the browser; a Postgres CAS that could
resurrect a used OTP challenge; a devcontainer firewall that failed open.
The first firewall fix covered only one trigger (an ad-blocking resolver's
sinkholed answers); any other error before the last lines, such as a
rate-limited `api.github.com/meta`, still left egress wide open. The script
now installs the default-deny policies first and fails closed on any error,
with a dated snapshot of GitHub's ranges for when the live list is
unavailable.

The example servers, which integrators copy, were open: both listened on
every interface, `cms_inbox` served any merchant's conversations and
replies to anyone, `embedded_signup` took the tenant from the query string
(one merchant could resume another's onboarding with their own PIN), and
one shared PIN registered every merchant's number. They now require a
tenant bearer token, check that the tenant owns the phone number before
reading the token vault, take the PIN from the merchant per attempt, and
bind to `127.0.0.1` unless told otherwise. The Flows endpoint's handler
example now verifies `X-Hub-Signature-256` before decrypting, and the
Redis store's docs require `noeviction` (a `volatile-*` policy silently
evicts OTP issue logs and dedup markers).

The final security review of 8ee6fab found, and fixed before 7940d15:

- **H1 — an OTP code was valid across merchants.** Store keys were
  HMAC(pepper, number | purpose), so `OtpService`s sharing a store and a
  pepper (several merchants of one integrator) shared records: a code
  merchant A sent verified at merchant B for the same number and purpose,
  an issue at A replaced B's code, and cooldowns and issue limits were
  pooled. Codes are now bound to the sending `phone_number_id` and an
  optional `OtpConfig::namespace` (new field; a blank one is a config
  error). Upgrading changes every key once: outstanding codes answer
  `NotFound` and issue logs restart.
- **M1 — one NUL in a customer's message blocked the whole webhook batch on
  Postgres.** Postgres cannot store U+0000, so `InboxSink` failed every
  delivery of that batch until Meta dropped it after 7 days, with every
  other event in it. `InboxSink` and `Inbox::send` now store U+0000 in
  message content as U+FFFD (lossy, and provisional: the choice was
  reserved for the maintainer, `OPEN_QUESTIONS.md` #18).
- **L1 — a status or revoke on one number could change another number's
  message.** `ConversationStore::update_status` matched the message id
  alone; it now takes the business `phone_number_id` first and both stores
  match on it (breaking for custom stores).
- **L3 — the body was read before the signature was looked at.** The axum
  route buffered an unsigned request up to the body limit (and answered
  `413` rather than `401` when it was larger). A missing or malformed
  `X-Hub-Signature-256` is now refused with `401` before the body is read.
- **Credential allowlist narrowed.** Tokens went to any `*.fbsbx.com`,
  `*.facebook.com` or `*.whatsapp.net` host on any port, and to
  `graph.facebook.com` even when the client was configured for a proxy.
  Now only the configured Graph endpoint's origin and
  `https://lookaside.fbsbx.com` on the default port (media downloads)
  receive one; the host is compared exactly, so subdomains, suffix and
  trailing-dot tricks and IDN look-alikes are refused before sending.
- **OTP codes go through `Messages::send`.** `OtpService` posted its own
  body, so `OutboundMessage::validate` never ran on it and it kept a
  private copy of the response decoding.
- **Send decode errors no longer quote the recipient.** An unreadable
  response of `Messages::send` or `Marketing::send` (which echo the
  recipient's number) is reported without the body snippet and without
  serde's message.
