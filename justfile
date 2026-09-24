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

# The wa-webhooks line builds all targets: the framework-free API
# (tests/signature.rs uses SIGNATURE_HEADER) must build without axum. The doc
# line builds rustdoc without default features (wa-rs included): a link to a
# feature-gated item must be gated with it; `just doc` covers --all-features.
#
# Each feature on its own, so a missing cfg gate cannot hide behind --all-features
features:
    cargo check -p wa-adapters --no-default-features
    cargo check -p wa-adapters --no-default-features --features memory
    cargo check -p wa-adapters --no-default-features --features sinks
    cargo check -p wa-adapters --no-default-features --features reqwest
    cargo check -p wa-adapters --no-default-features --features postgres
    cargo check -p wa-adapters --no-default-features --features redis
    cargo check -p wa-webhooks --no-default-features --all-targets
    cargo check -p wa-webhooks --features axum
    cargo check -p wa-client --no-default-features
    cargo check -p wa-client --features flows-endpoint
    cargo check -p wa-rs --no-default-features
    cargo check -p wa-rs --no-default-features --features reqwest
    cargo check -p wa-rs --no-default-features --features memory
    cargo check -p wa-rs --no-default-features --features sinks
    cargo check -p wa-rs --no-default-features --features postgres
    cargo check -p wa-rs --no-default-features --features redis
    cargo check -p wa-rs --no-default-features --features axum
    cargo check -p wa-rs --no-default-features --features typst
    cargo check -p wa-rs --no-default-features --features flows-endpoint
    cargo check -p wa-rs
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-default-features --no-deps

# Licenses, advisories, duplicate versions, sources
deny:
    cargo deny check

# The gate. CI runs exactly this.
ci: lint check test doc features deny test-live

# Mirror Meta's WhatsApp docs as Markdown into .meta-docs/ (gitignored; the
# docs are Meta's, never commit them). Agents grep this instead of guessing.
meta-docs *args:
    cargo run -q --manifest-path .xtask/Cargo.toml -- meta-docs {{args}}
