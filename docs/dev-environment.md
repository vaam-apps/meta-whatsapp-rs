# Development environment

## The dev container

`.devcontainer/` is a Claude Code sandbox adapted from Anthropic's reference
devcontainer (`github.com/anthropics/claude-code/.devcontainer`), for Rust:

| Piece | What |
| --- | --- |
| `Dockerfile` | `node:24-bookworm` base (Claude Code is an npm package), Rust 1.98.1 via rustup, `just`, `cargo-deny`, `typst` CLI, `git-delta`, zsh, `psql`/`redis-cli` |
| `compose.yaml` | `dev` (your shell), `postgres:18-alpine`, `redis:8-alpine`; named volumes for shell history, Claude config and the cargo registry |
| `init-firewall.sh` | default-deny egress, run on every start |
| `devcontainer.json` | VS Code extensions (Claude Code, rust-analyzer, tinymist for Typst, Even Better TOML, CodeLLDB, just, GitLens) |

Open it with VS Code's "Reopen in Container" or `devcontainer up`. Inside,
`WA_RS_TEST_POSTGRES_URL` and `WA_RS_TEST_REDIS_URL` point at the sidecars,
so `just test-live` (and `just ci`) use them instead of starting
`compose.test.yaml`.

### The firewall

On start, `init-firewall.sh` drops all outbound traffic except:

- DNS, SSH, loopback, the container's own network (the sidecars);
- GitHub (from `api.github.com/meta`);
- npm, the Anthropic API, VS Code's marketplace;
- crates.io (`crates.io`, `index.crates.io`, `static.crates.io`),
  `static.rust-lang.org`, `docs.rs`;
- `graph.facebook.com`, `developers.facebook.com` (for `just meta-docs`),
  `lookaside.fbsbx.com` (media downloads);
- telemetry domains (`sentry.io`, `statsig.com`, `statsig.anthropic.com`)
  when they resolve.

IPv6 egress is dropped entirely. The script verifies itself on every start:
`example.com` must be unreachable, `index.crates.io` and `api.github.com`
reachable, or the container start fails.

Two fixes relative to the reference script, both found by running it behind
an ad-blocking resolver: sinkholed DNS answers (`0.0.0.0`, `127.x`) are
skipped, and `ipset add -exist` tolerates duplicates. The reference script
aborted on the second `0.0.0.0` *before* installing any rule, leaving the
container with unrestricted egress.

To allow another domain, add it to the `required` (or `optional`) loop in
`init-firewall.sh` and rebuild. Inside the firewall,
`claude --dangerously-skip-permissions` is reasonable: the network boundary,
not the prompt, is what contains a runaway agent. It does not protect
secrets you mount into the container.

## Without the container

See [CONTRIBUTING.md](../CONTRIBUTING.md#environment). The only extra step is
Docker for `just test-live`.

## Recipes

| Recipe | Does |
| --- | --- |
| `just` | list recipes |
| `just ci` | the gate (what CI runs) |
| `just lint` | `cargo fmt --check`, clippy pedantic with `-D warnings` |
| `just check` / `just test` / `just doc` | all targets, all features |
| `just features` | each adapter/client feature compiled alone |
| `just test-live` / `just test-live-down` | live adapter tests; stop the services |
| `just deny` | licenses, advisories, bans, sources |
| `just meta-docs [--force]` | mirror Meta's docs into `.meta-docs/` |

Builds are heavy (typst, aws-lc-sys, sqlx). On a shared machine, cap
parallelism: `CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=4 just ci`.
