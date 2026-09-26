# meta-whatsapp-rs — agent guide

Rust toolkit for Meta's WhatsApp Business Platform: typed Graph API client,
webhooks, storage/transport/sink adapters, Typst documents. Built for three
products — e-commerce marketing, in-app merchant↔customer chat in a CMS,
and WhatsApp OTP authentication.

**Read [`docs/architecture.md`](docs/architecture.md) before designing
anything.** It is the spec. [`docs/coverage.md`](docs/coverage.md) says what
exists and what does not. [`OPEN_QUESTIONS.md`](OPEN_QUESTIONS.md) lists
decisions reserved for the maintainer — surface them, never pick a default.
[`CONTRIBUTING.md`](CONTRIBUTING.md) and
[`docs/dev-environment.md`](docs/dev-environment.md) cover setup and process.

## Commands

| Command | What it proves |
| --- | --- |
| `just ci` | **The gate.** CI runs exactly this. Nothing is "verified" until it exits 0 on the final head. |
| `just lint` | `cargo fmt --check` + clippy (pedantic, `-D warnings`) |
| `just test` | unit + in-process tests; `live_*` tests *skip* here |
| `just test-live` | adapter and service (`meta-whatsapp-server`) tests against real Postgres + Redis, with `META_WHATSAPP_RS_REQUIRE_LIVE=1` so a missing service **fails** |
| `just test-live-clean` | drops the `wa_test_*` databases and `wa_test_*` / `wa_server_test_*` schemas live runs left on the test Postgres, and their `wa-test:*` keys on the test Redis (killed runs; a panicking live test deletes its own); not during a live run |
| `just doc` | rustdoc with `-D warnings` (broken intra-doc links fail) |
| `just features` | each adapter feature compiled alone |
| `just skills-check` | every consumer skill's `Verified against meta-whatsapp-rs <sha>` stamp is a commit in HEAD's history, directly or listed as `Squashed-commit:` by a squash commit on main (the rest of the skill checks run in `just test`: `crates/meta-whatsapp-rs/tests/skills.rs`) |
| `just skills-ts` | the server skills' TypeScript examples type-check against types generated from `crates/meta-whatsapp-server/openapi/v1.json` (Node pinned in `tools/skills-ts/.nvmrc`; part of `just ci`) |
| `just squash-body <pr>` | the body of a PR's squash commit: its commits as `Squashed-commit:` lines plus their co-authors (CONTRIBUTING.md § Merging) |
| `just meta-docs` | mirror Meta's docs as Markdown into `.meta-docs/` (gitignored) |

Toolchain is pinned in `rust-toolchain.toml` (1.98.1, edition 2024); `just
ci` also needs Node, the major version of `tools/skills-ts/.nvmrc` (24),
and npm's registry, for `just skills-ts`. Run
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

- **Errors**: `meta_whatsapp_core::Error` tree, `thiserror` for typed nodes, `anyhow`
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
- **Tests** use `meta_whatsapp_core::testing::ScriptedTransport`; assert method, path,
  query, auth and exact JSON; assert `remaining() == 0`.
- **Lints**: `unsafe_code = forbid`, `missing_docs`, clippy pedantic.
- **Commits**: Conventional Commits (`feat(webhooks): …`). PRs are
  squash-merged: cite "PR #N", not the branch's own commits, and use
  `just squash-body <pr>` as the squash body (CONTRIBUTING.md § Merging).

## Verification discipline

- A skipped test is not a passing test. `live_*` tests skip without a
  service URL; only `just test-live` (which sets `META_WHATSAPP_RS_REQUIRE_LIVE=1`)
  proves them.
- When local disagrees with CI, CI is the evidence.
- A sub-agent's "all green" is a claim. Check the branch resolves, the files
  exist, and `just ci` exits 0 yourself.
- Name the decisive test when reviewing: delete the guard and confirm a test
  fails.

## Companion docs and skills

- Developer skills (working **on** meta-whatsapp-rs): `.claude/skills/`.
- Consumer skills (working **with** meta-whatsapp-rs, e.g. in the e-commerce or CMS
  repo): `skills/<name>/`, one task each, installable with
  `npx skills add vaam-apps/meta-whatsapp-rs`. Each carries a
  `Verified against meta-whatsapp-rs <full sha> (<date>)` stamp; its Rust blocks are
  excerpts of its `examples/*.rs`, which `just test` compiles and runs
  (`crates/meta-whatsapp-rs/tests/skills.rs`; CONTRIBUTING.md § "How the consumer
  skills are kept true"). The service's skills (`meta-whatsapp-rs-server*`)
  speak HTTP instead: their `ts` blocks are excerpts of `examples/*.ts`,
  which `just skills-ts` type-checks against the committed OpenAPI
  document, and their routes, schemas, codes and variables are checked
  against it and the service's source. Developer skills are `internal`:
  the installer never offers them.
- A change to a public API is not done until `docs/`, `skills/` and the
  rustdoc agree with it. Say in the PR what happened to each (a link, or
  `n/a — <reason>`).
