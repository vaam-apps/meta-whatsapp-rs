# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/) once published.

## [Unreleased] — 0.1.0

First feature set. See [docs/coverage.md](docs/coverage.md) for the full
matrix and [OPEN_QUESTIONS.md](OPEN_QUESTIONS.md) for decisions still open.

### Added

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
- **Examples**: send a message, CMS inbox server, Embedded Signup server,
  OTP login, invoice document.
- **Tooling**: `just ci` gate, CI running it, `cargo xtask meta-docs`,
  Claude Code dev container with a fail-closed default-deny firewall,
  project skills and agents, consumer skills (`npx skills add vaam-apps/wa-rs`).

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
