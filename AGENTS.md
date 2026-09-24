# wa-rs — agent guide

Rust toolkit for Meta's WhatsApp Business Platform: typed Graph API client,
webhooks, storage/transport/sink adapters, Typst documents. Built for three
products — e-commerce marketing, in-app merchant↔customer chat in a CMS,
and WhatsApp OTP authentication.

**Read [`docs/architecture.md`](docs/architecture.md) before designing
anything.** It is the spec. [`docs/coverage.md`](docs/coverage.md) says what
exists and what does not.

## Commands

| Command | What it proves |
| --- | --- |
| `just ci` | **The gate.** CI runs exactly this. Nothing is "verified" until it exits 0 on the final head. |
| `just lint` | `cargo fmt --check` + clippy (pedantic, `-D warnings`) |
| `just test` | unit + in-process tests; `live_*` tests *skip* here |
| `just test-live` | adapter tests against real Postgres + Redis, with `WA_RS_REQUIRE_LIVE=1` so a missing service **fails** |
| `just doc` | rustdoc with `-D warnings` (broken intra-doc links fail) |
| `just features` | each adapter feature compiled alone |
| `just meta-docs` | mirror Meta's docs as Markdown into `.meta-docs/` (gitignored) |

Toolchain is pinned in `rust-toolchain.toml` (1.98.1, edition 2024). Run
`just ci`, not a reconstruction of it: the flags you drop are the ones that
were set on purpose.

## Meta's docs are the source of truth

Meta serves every doc page as Markdown: append `.md` to the URL, e.g.
`https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/reference/messages/text.md`.
`just meta-docs` mirrors all of them into `.meta-docs/` with the same paths.
Before writing or reviewing an endpoint, **read its page there** and copy
request/response examples into tests. Never guess a field name. Never
commit `.meta-docs/` — the docs are Meta's copyrighted material; write our
own notes in our own words instead.

Graph API version: `ApiVersion::DEFAULT` = v25.0 (what the docs use as of
2026-09-24). Business-scoped user IDs (BSUID) are mandatory from 2026: every
webhook carries `user_id`, and `wa_id` may be absent — never key a user by
phone number alone.

## Conventions

- **Errors**: `wa_core::Error` tree, `thiserror` for typed nodes, `anyhow`
  only as the opaque leaf for code we don't own. Branch on `ErrorKind`,
  never on messages. Multi-step flows use `Error::in_step`.
- **Requests** only through `GraphRequest` (auth, retries, error decoding,
  credential host allowlist live there). Endpoint modules never touch the
  transport.
- **Serde**: never `deny_unknown_fields` on responses/webhooks; extensible
  enums get an `Unknown`/`Other` catch-all. A new Meta field must never
  break parsing.
- **Secrets** (`AccessToken`, `AppSecret`, OTP codes, PINs) never reach
  `Debug`, logs, error messages, or URLs we log. `GraphRequest`'s `Debug`
  omits the query for this reason.
- **No unwrap/expect/panic in library code** (clippy denies them via
  `-D warnings`); tests may.
- **Tests** use `wa_core::testing::ScriptedTransport`; assert method, path,
  query, auth and exact JSON; assert `remaining() == 0`.
- **Lints**: `unsafe_code = forbid`, `missing_docs`, clippy pedantic.
- **Commits**: Conventional Commits (`feat(webhooks): …`).

## Verification discipline

- A skipped test is not a passing test. `live_*` tests skip without a
  service URL; only `just test-live` (which sets `WA_RS_REQUIRE_LIVE=1`)
  proves them.
- When local disagrees with CI, CI is the evidence.
- A sub-agent's "all green" is a claim. Check the branch resolves, the files
  exist, and `just ci` exits 0 yourself.
- Name the decisive test when reviewing: delete the guard and confirm a test
  fails.

## Companion docs and skills

- Developer skills (working **on** wa-rs): `.claude/skills/`.
- Consumer skills (working **with** wa-rs, e.g. in the e-commerce or CMS
  repo): `skills/`, installable with `npx skills add vaam-apps/wa-rs`. Each
  carries a `Verified against wa-rs <sha> (<date>)` stamp.
- A change to a public API is not done until `docs/`, `skills/` and the
  rustdoc agree with it. Say in the PR what happened to each (a link, or
  `n/a — <reason>`).
