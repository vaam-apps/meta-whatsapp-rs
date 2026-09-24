# Contributing to wa-rs

Read [AGENTS.md](AGENTS.md) first — it is the short version of everything
below and applies to humans and coding agents alike.
[docs/architecture.md](docs/architecture.md) is the spec;
[docs/coverage.md](docs/coverage.md) says what exists;
[OPEN_QUESTIONS.md](OPEN_QUESTIONS.md) lists decisions nobody has made yet.

## Environment

Either open the repo in the dev container (see
[docs/dev-environment.md](docs/dev-environment.md)) or install locally:

- Rust via rustup — `rust-toolchain.toml` pins 1.98.1 and rustup fetches it.
- [`just`](https://github.com/casey/just) ≥ 1.58 and
  [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) 0.20.
- Docker, for `just test-live` (Postgres 18 and Redis 8 from
  `compose.test.yaml`, on ports 55432/56379).

## The gate

```bash
just ci        # lint, check, test, doc, features, deny, test-live
```

CI runs exactly this. A change is verified when `just ci` exits 0 on its
final head — not when a subset passes, and not when a reconstruction of the
commands passes. `just test` *skips* the live Postgres/Redis tests; only
`just test-live` (which sets `WA_RS_REQUIRE_LIVE=1`) proves them.

## Meta's docs

`just meta-docs` mirrors Meta's WhatsApp docs as Markdown into `.meta-docs/`
(gitignored — the docs are Meta's copyrighted material; never commit them).
Every field name, enum value and limit in the code must come from those
pages; copy their example payloads into tests.

## Making a change

- Adding or changing an endpoint: `.claude/skills/add-graph-endpoint`.
  Paths with ids use `client.get_at/post_at/delete_at(&[..])`, never
  `format!`.
- Adding a webhook field or message type: `.claude/skills/add-webhook-field`.
- Errors: `.claude/skills/error-tree`. Branch on `ErrorKind`, never on
  message text.
- Typst templates: `.claude/skills/typst-templates`.
- Tests use `wa_core::testing::ScriptedTransport` and assert method, path,
  query, auth header, exact JSON body and `remaining() == 0`.
- Secrets never reach `Debug`, logs, errors or logged URLs.
- No `unwrap`/`expect`/`panic` in library code.

## Review pipeline

Non-trivial work goes: implement → adversarial review with a distinct lens
(the `wa-sabotage-reviewer` agent: every field against the docs, every guard
mutated to prove a test catches it) → remediate → re-run the original
failure → `just ci`. Security-relevant changes (webhooks, onboarding,
tokens, OTP, crypto) also get the `wa-security-reviewer` lens.

When mutation-testing, cap each test run's memory and time (e.g.
`systemd-run --user --scope -p MemoryMax=4G timeout 180 cargo test …`) but
**never** cap the build with `ulimit -v` — it kills the linker and makes
every mutant look "killed". Bound any test that collects a stream.

## Docs and skills parity

A public API change is not done until these agree with it:

- rustdoc (every public item; module docs name the Meta pages implemented),
- `docs/architecture.md` and `docs/coverage.md`,
- the consumer skills in `skills/` (they instruct other repos' coding
  agents — a stale skill generates wrong code at scale). Re-stamp every
  skill you verified with `Verified against wa-rs <sha> (<date>)`.

Say in the PR description what happened to each, with a link or
`n/a — <reason>`.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/):
`feat(webhooks): …`, `fix(client): …`, `docs(skills): …`.
