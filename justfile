# wa-rs — task runner. `just ci` is the gate: CI runs exactly it, and
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

# Adapter tests against real Postgres and Redis. Uses WA_RS_TEST_POSTGRES_URL /
# WA_RS_TEST_REDIS_URL when set (the devcontainer sets them to its sidecars),
# otherwise starts compose.test.yaml. WA_RS_REQUIRE_LIVE=1 turns a missing
# service into a failure instead of a skip.
test-live:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -z "${WA_RS_TEST_POSTGRES_URL:-}" ] || [ -z "${WA_RS_TEST_REDIS_URL:-}" ]; then
        docker compose -f compose.test.yaml up -d --wait
        export WA_RS_TEST_POSTGRES_URL="postgres://wa:wa@127.0.0.1:55432/wa"
        export WA_RS_TEST_REDIS_URL="redis://127.0.0.1:56379"
    fi
    WA_RS_REQUIRE_LIVE=1 cargo test -p wa-adapters --all-features live_ -- --test-threads=4

# Stop the compose.test.yaml services
test-live-down:
    docker compose -f compose.test.yaml down -v

# Formatting and clippy, warnings are errors
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Format everything
fmt:
    cargo fmt --all

# Rustdoc with broken links and missing docs as errors
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps

# Each feature on its own, so a missing cfg gate cannot hide behind --all-features
features:
    cargo check -p wa-adapters --no-default-features
    cargo check -p wa-adapters --no-default-features --features memory
    cargo check -p wa-adapters --no-default-features --features sinks
    cargo check -p wa-adapters --no-default-features --features reqwest
    cargo check -p wa-adapters --no-default-features --features postgres
    cargo check -p wa-adapters --no-default-features --features redis
    cargo check -p wa-webhooks --no-default-features
    cargo check -p wa-webhooks --features axum
    cargo check -p wa-client --no-default-features
    cargo check -p wa-client --features flows-endpoint
    cargo check -p wa-rs --no-default-features
    cargo check -p wa-rs

# Licenses, advisories, duplicate versions, sources
deny:
    cargo deny check

# The gate. CI runs exactly this.
ci: lint check test doc features deny test-live

# Mirror Meta's WhatsApp docs as Markdown into .meta-docs/ (gitignored; the
# docs are Meta's, never commit them). Agents grep this instead of guessing.
meta-docs *args:
    cargo run -q -p xtask -- meta-docs {{args}}
