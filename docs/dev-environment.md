# Development environment

## The dev container

`.devcontainer/` is a Claude Code sandbox adapted from Anthropic's reference
devcontainer (`github.com/anthropics/claude-code/.devcontainer`), for Rust:

| Piece | What |
| --- | --- |
| `Dockerfile` | `node:24-bookworm` base (Claude Code is an npm package), Rust 1.98.1 via rustup, `just`, `cargo-deny`, `typst` CLI, `git-delta`, zsh, `psql`/`redis-cli` |
| `compose.yaml` | `dev` (your shell), `postgres:18-alpine`, `redis:8-alpine`; named volumes for shell history, Claude config and the cargo registry |
| `init-firewall.sh` | default-deny egress, run on every start |
| `github-meta-snapshot.json` | GitHub's IPv4 ranges as of the date inside it: the firewall's fallback (copied to `/etc/meta-whatsapp-rs-firewall/`) |
| `devcontainer.json` | VS Code extensions (Claude Code, rust-analyzer, tinymist for Typst, Even Better TOML, CodeLLDB, just, GitLens) |

Open it with VS Code's "Reopen in Container" or `devcontainer up`. Inside,
`META_WHATSAPP_RS_TEST_POSTGRES_URL` and `META_WHATSAPP_RS_TEST_REDIS_URL`
point at the sidecars, so `just test-live` (and `just ci`) use them instead
of starting `compose.test.yaml`.

The Compose project is `meta-whatsapp-rs-dev` (it was `wa-rs-dev` before
the rename), and named volumes are per project: a container built before
the rename kept its shell history, Claude config and cargo caches in the
`wa-rs-dev_*` volumes, which the renamed one does not mount. Log in to
Claude Code again, or copy the old volumes' contents over; remove them
with `docker volume rm` once you no longer need them.

### The firewall

On start (`postStartCommand`: `sudo /usr/local/bin/init-firewall.sh`), the
script drops all outbound traffic except:

- DNS, SSH, loopback, the container's own network (the sidecars);
- GitHub (the `web`, `api` and `git` ranges from `api.github.com/meta`);
- npm, the Anthropic API, VS Code's marketplace;
- crates.io (`crates.io`, `index.crates.io`, `static.crates.io`),
  `static.rust-lang.org`, `docs.rs`;
- `graph.facebook.com`, `developers.facebook.com` (for `just meta-docs`),
  `lookaside.fbsbx.com` (media downloads);
- telemetry domains (`sentry.io`, `statsig.com`, `statsig.anthropic.com`)
  when they resolve.

IPv6 egress is dropped entirely.

**It fails closed.** The first thing the script does is set the `DROP`
policies (IPv4 and IPv6); only then does it flush the old rules, add the
base allowances above, and start resolving and fetching. An `EXIT` trap
re-asserts `DROP` if anything fails, so an error part-way leaves the
container with *fewer* allowances, never with open egress. At the end it
checks itself: `example.com` must be unreachable, `index.crates.io` and
`api.github.com` reachable, else it exits non-zero (policies still `DROP`).

**GitHub's ranges.** `api.github.com/meta` allows 60 unauthenticated
requests per hour per IP, which a few restarts behind one NAT use up. The
script tries it three times (not at all again after a `403`/`429` rate-limit
answer) and otherwise falls back to `github-meta-snapshot.json`, baked into
the image, with a warning naming the snapshot's date. GitHub hosts added
after that date stay blocked until a live fetch succeeds; refresh the
snapshot with the command in the `Dockerfile` when you touch the firewall.
The snapshot lives under `/etc` (root-owned) because `/usr/local/share`
belongs to `node`, and a snapshot the agent could rewrite would let it widen
the allowlist.

**When the script fails, the container still starts.** A failing
`postStartCommand` does not stop the container: `devcontainer up` (CLI
0.89.0, tried with a `postStartCommand` of `exit 1`) exits 1 with
`"outcome":"error"` and "postStartCommand from devcontainer.json failed",
and the container keeps running and accepts `docker exec`. VS Code reports
the failed command in its terminal; it was not tried here whether it still
attaches. Because the script fails closed, such a container has almost no
egress (cargo and npm fail) rather than all of it. Fix the cause and run
`sudo /usr/local/bin/init-firewall.sh` again.

**Addresses are resolved once.** Domains are allowed by the IPv4 addresses
they resolve to at start. Some rotate: `graph.facebook.com` answers with a
different Meta edge address from one query to the next (TTL 30 s), so a
call to it can be refused later until you rerun the script. (That is also
why the self-check does not test it.)

Fixes relative to the reference script: it set the `DROP` policies only at
the very end, after fetching GitHub's ranges and resolving every domain, so
any error on the way (a rate-limited or unreachable `api.github.com/meta`,
a required domain that did not resolve) exited with every rule flushed and
every policy `ACCEPT`: unrestricted egress. An ad-blocking resolver's sinkholed
answers (`0.0.0.0`, `127.x`) are now skipped, and `ipset add -exist`
tolerates duplicates.

The script's two test hooks, `WA_FIREWALL_GITHUB_META_URL` and
`WA_FIREWALL_GITHUB_SNAPSHOT`, are environment variables: `sudo` strips them
("you are not allowed to set the following environment variables"), so the
`node` user cannot use them to point the allowlist elsewhere. To prove the
fail-closed path, run the image as root with a network capability and both
hooks pointing nowhere:

```bash
docker build -t meta-whatsapp-rs-devcontainer:test .devcontainer
docker run --rm --user root --cap-add=NET_ADMIN --cap-add=NET_RAW meta-whatsapp-rs-devcontainer:test \
  bash -c 'WA_FIREWALL_GITHUB_META_URL=https://192.0.2.1/meta WA_FIREWALL_GITHUB_SNAPSHOT=/nonexistent \
    /usr/local/bin/init-firewall.sh >/dev/null 2>&1; echo "exit=$?"; iptables -S | grep "^-P";
    curl -s -o /dev/null --connect-timeout 5 https://example.com && echo "example.com ALLOWED" || echo "example.com BLOCKED"'
```

Expected: `exit=1`, three `DROP` policies, `example.com BLOCKED`.

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
| `just skills-check` | every skill stamp's commit is on main (directly or squash-merged) |
| `just squash-body <pr>` | the squash commit body for a PR (CONTRIBUTING.md § Merging) |
| `just meta-docs [--force]` | mirror Meta's docs into `.meta-docs/` |

Builds are heavy (typst, aws-lc-sys, sqlx). On a shared machine, cap
parallelism: `CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=4 just ci`.
