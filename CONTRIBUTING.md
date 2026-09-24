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
just ci        # lint, check, test, skills-check, doc, features, deny, test-live
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
  skill you verified with `Verified against wa-rs <full sha> (<date>)`.

Say in the PR description what happened to each, with a link or
`n/a — <reason>`.

### How the consumer skills are kept true

- Each skill is `skills/<name>/SKILL.md` (flat, one job per skill, at
  most 160 lines; long tables in `references/`). Its Rust code lives in
  `skills/<name>/examples/*.rs`, compiled and tested by
  `crates/wa-rs/tests/skills.rs` through `tests/skill_examples/mod.rs`
  (add a `#[path]` line for a new file; the test fails until you do).
- A ```` ```rust ```` block in a skill is an excerpt of such a file (or of
  `crates/wa-rs/examples/*.rs`): edit the example, run `just fmt`, then
  copy the lines. The test names the block that drifted. Label every
  fence with its language (Rust is exactly `rust`); keep example files
  free of block comments, `macro_rules!` and any `cfg` but `cfg(test)`
  (in `crates/wa-rs/examples/*.rs`: but the `postgres` arms; they are
  built as examples, so `cfg(test)` never holds there), whose code a
  block could quote without it ever compiling. A block may not quote
  lines inside a string (plain, raw, byte or C) or a block comment of any
  example (a line comment is quoted as a comment, which is harmless), nor
  an item under a `cfg` that `--all-features` never enables (the
  `#[cfg(not(feature = …))]` arms of `crates/wa-rs/examples/*.rs`) or
  under a `cfg_attr` that carries a `cfg`.
- Each `references/*.md` carries a `Verified against wa-rs <full
  sha> (<date>)` stamp under its title, like its `SKILL.md`, checked the same
  way.
- Backticked Rust names in the prose must exist in `crates/` or in the
  skill's own examples, and `Type::member` must belong to that type;
  `skills/.allowlist` lists the few that are
  another crate's or not Rust at all. Relative links stay inside the
  skill (each is installed on its own); link anything else on GitHub.
- No `SKILL.md` outside `skills/<name>/` and `.claude/skills/<name>/`:
  the installer would offer it (a root one hides every other skill).
- `just skills-check` (part of `just ci`) checks that every stamp's commit
  exists and is an ancestor of HEAD.
- Developer skills in `.claude/skills/` carry `metadata: internal: true`
  so the installer does not offer them. Check what it offers from your
  checkout with `npx -y skills add <path-to-checkout> --list`: only the
  consumer skills may appear.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/):
`feat(webhooks): …`, `fix(client): …`, `docs(skills): …`.
