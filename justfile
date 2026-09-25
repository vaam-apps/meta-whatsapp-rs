# meta-whatsapp-rs — task runner. `just ci` is the gate: CI runs exactly it, and
# nothing is "verified" until it exits 0 on the final head.

set shell := ["bash", "-euo", "pipefail", "-c"]

# List recipes
default:
    @just --list

# Type-check every crate, every target, every feature
check:
    cargo check --workspace --all-targets --all-features

# Unit and in-process tests. Live adapter tests are compiled but skip unless
# their service URL is set; `test-live` is the recipe that makes them count.
test:
    cargo test --workspace --all-features

# Adapter and service tests against real Postgres and Redis. Uses META_WHATSAPP_RS_TEST_POSTGRES_URL /
# META_WHATSAPP_RS_TEST_REDIS_URL when set (the devcontainer sets them to its sidecars),
# otherwise starts compose.test.yaml. META_WHATSAPP_RS_REQUIRE_LIVE=1 turns a missing
# service into a failure instead of a skip.
test-live:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -z "${META_WHATSAPP_RS_TEST_POSTGRES_URL:-}" ] || [ -z "${META_WHATSAPP_RS_TEST_REDIS_URL:-}" ]; then
        docker compose -f compose.test.yaml up -d --wait
        export META_WHATSAPP_RS_TEST_POSTGRES_URL="postgres://wa:wa@127.0.0.1:55432/wa"
        export META_WHATSAPP_RS_TEST_REDIS_URL="redis://127.0.0.1:56379"
    fi
    META_WHATSAPP_RS_REQUIRE_LIVE=1 cargo test -p meta-whatsapp-adapters --all-features live_ -- --test-threads=4
    META_WHATSAPP_RS_REQUIRE_LIVE=1 cargo test -p meta-whatsapp-server --all-features live_ -- --test-threads=4

# Stop the compose.test.yaml services
test-live-down:
    docker compose -f compose.test.yaml down -v

# Formatting and clippy, warnings are errors. `.xtask` is a workspace of its
# own (see .xtask/Cargo.toml), so it is checked by manifest path.
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo fmt --manifest-path .xtask/Cargo.toml --check
    cargo clippy --manifest-path .xtask/Cargo.toml --all-targets -- -D warnings

# Format everything
fmt:
    cargo fmt --all
    cargo fmt --manifest-path .xtask/Cargo.toml

# Rustdoc with broken links and missing docs as errors
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps

# The meta-whatsapp-webhooks line builds all targets: the framework-free API
# (tests/signature.rs uses SIGNATURE_HEADER) must build without axum. The doc
# line builds rustdoc without default features (meta-whatsapp-rs included): a link to a
# feature-gated item must be gated with it; `just doc` covers --all-features.
# It leaves meta-whatsapp-server out: the service turns on the facade's
# reqwest, memory, postgres and axum, and in one --workspace build those
# features would be on for every crate, hiding an ungated link.
#
# Each feature on its own, so a missing cfg gate cannot hide behind --all-features
features:
    cargo check -p meta-whatsapp-adapters --no-default-features
    cargo check -p meta-whatsapp-adapters --no-default-features --features memory
    cargo check -p meta-whatsapp-adapters --no-default-features --features sinks
    cargo check -p meta-whatsapp-adapters --no-default-features --features reqwest
    cargo check -p meta-whatsapp-adapters --no-default-features --features postgres
    cargo check -p meta-whatsapp-adapters --no-default-features --features redis
    cargo check -p meta-whatsapp-webhooks --no-default-features --all-targets
    cargo check -p meta-whatsapp-webhooks --features axum
    cargo check -p meta-whatsapp-client --no-default-features
    cargo check -p meta-whatsapp-client --features flows-endpoint
    cargo check -p meta-whatsapp-rs --no-default-features
    cargo check -p meta-whatsapp-rs --no-default-features --features reqwest
    cargo check -p meta-whatsapp-rs --no-default-features --features memory
    cargo check -p meta-whatsapp-rs --no-default-features --features sinks
    cargo check -p meta-whatsapp-rs --no-default-features --features postgres
    cargo check -p meta-whatsapp-rs --no-default-features --features redis
    cargo check -p meta-whatsapp-rs --no-default-features --features axum
    cargo check -p meta-whatsapp-rs --no-default-features --features typst
    cargo check -p meta-whatsapp-rs --no-default-features --features flows-endpoint
    cargo check -p meta-whatsapp-rs
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --exclude meta-whatsapp-server --no-default-features --no-deps

# Licenses, advisories, duplicate versions, sources
deny:
    cargo deny check
    # .xtask is its own workspace (own Cargo.lock): check it too.
    cargo deny --manifest-path .xtask/Cargo.toml --config deny.toml check

# Everything else about the consumer skills (compiled examples, excerpts,
# frontmatter, links, names) is crates/meta-whatsapp-rs/tests/skills.rs, run by `test`.
# This needs the git history: CI checks out with fetch-depth 0, and a shallow
# clone fails here.
#
# Consumer skills: every `Verified against meta-whatsapp-rs <sha>` stamp is a commit in HEAD's history, or a branch commit a squash commit on main lists
skills-check:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "$(git rev-parse --is-shallow-repository)" = true ]; then
        echo "shallow clone: the stamps' commits cannot be checked (fetch the full history)"
        exit 1
    fi
    status=0
    for skill in skills/*/SKILL.md; do
        if ! grep -qE 'Verified against meta-whatsapp-rs [0-9a-f]{40} ' "$skill"; then
            echo "$skill: no 'Verified against meta-whatsapp-rs <full sha>' stamp"
            status=1
        fi
    done
    stamps=$(grep -rhoE 'Verified against meta-whatsapp-rs [0-9a-f]+' skills | awk '{print $4}' | sort -u)
    if [ -z "$stamps" ]; then
        echo "no stamps found under skills/"
        exit 1
    fi
    # A squash merge replaces a branch's commits with one, whose body lists
    # them (`Squashed-commit: <sha>`, `just squash-body`). Only listings
    # already on main count: on a pull request, a line in the branch's own
    # commits proves nothing (the branch's real stamps are its ancestors).
    onmain=$(git merge-base HEAD origin/main 2>/dev/null) || {
        echo "skills-check: no origin/main to check squash listings against (fetch it)"
        exit 1
    }
    for sha in $stamps; do
        files=$(grep -rlE "Verified against meta-whatsapp-rs $sha([^0-9a-f]|$)" skills | wc -l)
        if [ "${#sha}" -ne 40 ]; then
            echo "stamp $sha: not a full 40-character commit id ($files files)"
            status=1
        elif git cat-file -e "$sha^{commit}" 2>/dev/null && git merge-base --is-ancestor "$sha" HEAD; then
            echo "stamp $sha: ok ($files files)"
        else
            # No pipe into `head`: under pipefail, git dying of SIGPIPE on a
            # second listing would fail the recipe at random.
            squash=$(git log -n 1 -E --format=%H --grep="^Squashed-commit: $sha[[:space:]]*\$" "$onmain")
            if [ -n "$squash" ]; then
                echo "stamp $sha: ok, squashed into $squash ($files files)"
            elif git cat-file -e "$sha^{commit}" 2>/dev/null; then
                echo "stamp $sha: not an ancestor of HEAD, and no squash commit on main lists it ($files files)"
                status=1
            else
                echo "stamp $sha: no such commit, and no squash commit on main lists it ($files files)"
                status=1
            fi
        fi
    done
    exit "$status"

# The server skills' TypeScript examples (skills/meta-whatsapp-rs-server*/examples/*.ts),
# type-checked against types generated from the committed OpenAPI document of
# crates/meta-whatsapp-server. Node is pinned in tools/skills-ts/.nvmrc (CI
# installs exactly it; locally the major version must match) and the packages
# by tools/skills-ts/package-lock.json. The rest of the server skills' checks
# (excerpts, routes, codes, variables) are crates/meta-whatsapp-rs/tests/skills.rs.
skills-ts:
    #!/usr/bin/env bash
    set -euo pipefail
    cd tools/skills-ts
    want="$(cat .nvmrc)"
    have="$(node --version)"
    have="${have#v}"
    if [ "${have%%.*}" != "${want%%.*}" ]; then
        echo "skills-ts: Node ${want%%.*}.x expected (tools/skills-ts/.nvmrc: $want), found $have"
        exit 1
    fi
    npm ci --no-audit --no-fund
    for examples in ../../skills/meta-whatsapp-rs-server*/examples; do
        npx --no-install openapi-typescript ../../crates/meta-whatsapp-server/openapi/v1.json \
            --output "$examples/meta-whatsapp-server.d.ts"
    done
    npx --no-install tsc -p tsconfig.json

# The body of a pull request's squash commit: one `Squashed-commit: <sha>`
# line per commit of the PR, as GitHub lists them (so stamps naming a branch
# commit still resolve on main: `skills-check`), then the commits'
# `Co-authored-by:` lines (a custom body replaces GitHub's default one).
# Refuses a PR without commits. Usage: CONTRIBUTING.md § Merging.
squash-body pr:
    #!/usr/bin/env bash
    set -euo pipefail
    commits=$(gh pr view {{pr}} --json commits --jq '.commits[].oid')
    if [ -z "$commits" ]; then
        echo "PR {{pr}}: no commits listed" >&2
        exit 1
    fi
    for c in $commits; do echo "Squashed-commit: $c"; done
    coauthors=$(gh pr view {{pr}} --json commits --jq '.commits[].messageBody' | grep -iE '^co-authored-by:' | sort -u || true)
    if [ -n "$coauthors" ]; then printf '\n%s\n' "$coauthors"; fi

# The gate. CI runs exactly this.
ci: lint check test skills-check skills-ts doc features deny test-live

# Mirror Meta's WhatsApp docs as Markdown into .meta-docs/ (gitignored; the
# docs are Meta's, never commit them). Agents grep this instead of guessing.
meta-docs *args:
    cargo run -q --manifest-path .xtask/Cargo.toml -- meta-docs {{args}}
