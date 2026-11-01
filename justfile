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

# Adapter, inbox (meta-whatsapp-rs's tests/inbox_events.rs), bot retention
# (meta-whatsapp-bot's tests/retention.rs) and service tests against real
# Postgres, Redis and MongoDB. The facade's filter is `live_postgres_`, not
# `live_`: its unit and skill tests have names containing `live_`
# (`…_live_duplicates_…`, the `live_updates` skill), which would keep its count
# above zero with no live test run. Uses META_WHATSAPP_RS_TEST_POSTGRES_URL /
# META_WHATSAPP_RS_TEST_REDIS_URL / META_WHATSAPP_RS_TEST_REDISS_URL /
# META_WHATSAPP_RS_TEST_MONGODB_URL when set (the
# devcontainer sets them to its sidecars), otherwise generates the test-only
# TLS certificates (tools/test-tls/generate.sh, into .test-tls/) and starts
# compose.test.yaml. The `rediss://` tests (roadmap L21c) also read the PEM
# files in META_WHATSAPP_RS_TEST_REDISS_CA, `…_CLIENT_CERT` and `…_CLIENT_KEY`.
# The MongoDB URL (roadmap S8) is a single-node replica set's, with
# `directConnection=true`: the set's member address is the container's own.
# META_WHATSAPP_RS_REQUIRE_LIVE=1 turns a missing service into a failure
# instead of a skip.
test-live:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -z "${META_WHATSAPP_RS_TEST_POSTGRES_URL:-}" ] || [ -z "${META_WHATSAPP_RS_TEST_REDIS_URL:-}" ] || [ -z "${META_WHATSAPP_RS_TEST_REDISS_URL:-}" ] || [ -z "${META_WHATSAPP_RS_TEST_MONGODB_URL:-}" ]; then
        tools/test-tls/generate.sh
        if ! docker compose -f compose.test.yaml up -d --wait; then
            # A service that exits or never turns healthy says why only in
            # its own log: print the last lines of each before failing.
            docker compose -f compose.test.yaml ps -a || true
            docker compose -f compose.test.yaml logs --no-color --tail 40 || true
            exit 1
        fi
        export META_WHATSAPP_RS_TEST_POSTGRES_URL="postgres://wa:wa@127.0.0.1:55432/wa"
        export META_WHATSAPP_RS_TEST_REDIS_URL="redis://127.0.0.1:56379"
        export META_WHATSAPP_RS_TEST_REDISS_URL="rediss://localhost:56380"
        export META_WHATSAPP_RS_TEST_REDISS_CA="$PWD/.test-tls/ca.crt"
        export META_WHATSAPP_RS_TEST_REDISS_CLIENT_CERT="$PWD/.test-tls/client.crt"
        export META_WHATSAPP_RS_TEST_REDISS_CLIENT_KEY="$PWD/.test-tls/client.key"
        export META_WHATSAPP_RS_TEST_MONGODB_URL="mongodb://127.0.0.1:57017/?directConnection=true"
    fi
    META_WHATSAPP_RS_REQUIRE_LIVE=1 cargo test -p meta-whatsapp-adapters --all-features live_ -- --test-threads=4
    META_WHATSAPP_RS_REQUIRE_LIVE=1 cargo test -p meta-whatsapp-rs --all-features live_postgres_ -- --test-threads=4
    META_WHATSAPP_RS_REQUIRE_LIVE=1 cargo test -p meta-whatsapp-bot --all-features live_postgres_ -- --test-threads=4
    META_WHATSAPP_RS_REQUIRE_LIVE=1 cargo test -p meta-whatsapp-server-store-postgres --all-features live_ -- --test-threads=4
    META_WHATSAPP_RS_REQUIRE_LIVE=1 cargo test -p meta-whatsapp-server --all-features live_ -- --test-threads=4

# Stop the compose.test.yaml services
test-live-down:
    docker compose -f compose.test.yaml down -v

# Drop what live tests left on the test services: `wa_test_*` databases and
# `wa_test_*` / `wa_server_test_*` schemas on Postgres, `wa-test:*` keys on
# Redis and the TLS Redis, `wa_test_*` databases on MongoDB. The live tests delete their own, when they panic too (the
# adapters' tests/common PgCleanup and RedisCleanup, tests/redis_tls.rs's
# RedissCleanup, tests/live_mongodb.rs's MongoCleanup, the server's TestDb);
# this clears older runs and killed ones (SIGKILL and abort skip Drop). Uses META_WHATSAPP_RS_TEST_POSTGRES_URL,
# META_WHATSAPP_RS_TEST_REDIS_URL, META_WHATSAPP_RS_TEST_REDISS_URL (with
# its CA and client files) and META_WHATSAPP_RS_TEST_MONGODB_URL when set (the devcontainer's sidecars),
# otherwise the compose.test.yaml servers. Not during a live run: it would
# delete that run's objects too.
#
# Drop leftover live-test databases, schemas and keys, keeping the servers up
test-live-clean:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "${META_WHATSAPP_RS_TEST_POSTGRES_URL:-}" ]; then
        psql=(psql "$META_WHATSAPP_RS_TEST_POSTGRES_URL")
    else
        psql=(docker compose -f compose.test.yaml exec -T postgres psql -U wa -d wa)
    fi
    "${psql[@]}" -q -X -v ON_ERROR_STOP=1 <<'SQL'
    SET client_min_messages = warning;
    CREATE TEMP VIEW leftover AS
      SELECT 'DATABASE' AS kind, datname::text AS name,
             format('DROP DATABASE %I WITH (FORCE)', datname) AS statement
      FROM pg_database WHERE datname LIKE 'wa\_test\_%'
      UNION ALL
      SELECT 'SCHEMA', nspname::text, format('DROP SCHEMA %I CASCADE', nspname)
      FROM pg_namespace
      WHERE nspname LIKE 'wa\_test\_%' OR nspname LIKE 'wa\_server\_test\_%';
    SELECT count(*) FILTER (WHERE kind = 'DATABASE') AS databases,
           count(*) FILTER (WHERE kind = 'SCHEMA') AS schemas
    FROM leftover \gset
    \echo test-live-clean: dropping :databases databases and :schemas schemas
    SELECT statement FROM leftover ORDER BY kind, name \gexec
    SELECT count(*) AS left FROM leftover \gset
    \echo test-live-clean: :left left
    SQL
    if [ -n "${META_WHATSAPP_RS_TEST_REDIS_URL:-}" ]; then
        redis=(redis-cli -u "$META_WHATSAPP_RS_TEST_REDIS_URL")
    else
        redis=(docker compose -f compose.test.yaml exec -T redis redis-cli)
    fi
    # The TLS Redis: its variables when set, compose.test.yaml's when the
    # plain Redis is too, none otherwise (a devcontainer built before it).
    if [ -n "${META_WHATSAPP_RS_TEST_REDISS_URL:-}" ]; then
        rediss=(redis-cli -u "$META_WHATSAPP_RS_TEST_REDISS_URL" --tls
            --cacert "$META_WHATSAPP_RS_TEST_REDISS_CA"
            --cert "$META_WHATSAPP_RS_TEST_REDISS_CLIENT_CERT"
            --key "$META_WHATSAPP_RS_TEST_REDISS_CLIENT_KEY")
    elif [ -z "${META_WHATSAPP_RS_TEST_REDIS_URL:-}" ]; then
        rediss=(docker compose -f compose.test.yaml exec -T redis-tls redis-cli --tls -p 6380
            --cacert /tls/ca.crt --cert /tls/client.crt --key /tls/client.key)
    else
        rediss=()
    fi
    # One script, so no key name goes through a shell or xargs.
    script="
      local n, cursor = 0, '0'
      repeat
        local page = redis.call('SCAN', cursor, 'MATCH', ARGV[1], 'COUNT', 1000)
        cursor = page[1]
        for _, key in ipairs(page[2]) do n = n + redis.call('DEL', key) end
      until cursor == '0'
      return n"
    deleted=$("${redis[@]}" --no-raw EVAL "$script" 0 'wa-test:*')
    echo "test-live-clean: deleted ${deleted#(integer) } Redis keys"
    if [ "${#rediss[@]}" -gt 0 ]; then
        deleted=$("${rediss[@]}" --no-raw EVAL "$script" 0 'wa-test:*')
        echo "test-live-clean: deleted ${deleted#(integer) } keys on the TLS Redis"
    else
        echo "test-live-clean: no TLS Redis (META_WHATSAPP_RS_TEST_REDISS_URL is not set)"
    fi
    # MongoDB (roadmap S8), as the TLS Redis: its URL when set, through a
    # mongosh on PATH (the devcontainer has one); compose.test.yaml's when the
    # plain Redis's URL is unset too; none otherwise (a devcontainer built
    # before it).
    if [ -n "${META_WHATSAPP_RS_TEST_MONGODB_URL:-}" ]; then
        if ! command -v mongosh >/dev/null; then
            echo "test-live-clean: META_WHATSAPP_RS_TEST_MONGODB_URL is set but mongosh is not on PATH" >&2
            exit 1
        fi
        mongo=(mongosh "$META_WHATSAPP_RS_TEST_MONGODB_URL")
    elif [ -z "${META_WHATSAPP_RS_TEST_REDIS_URL:-}" ]; then
        mongo=(docker compose -f compose.test.yaml exec -T mongodb mongosh)
    else
        mongo=()
    fi
    # Names come from the server and go back to it as values, through no shell.
    js='
      const leftover = () => db.adminCommand({ listDatabases: 1, nameOnly: true,
        filter: { name: { $regex: "^wa_test_" } } }).databases.map((d) => d.name);
      const names = leftover();
      print("test-live-clean: dropping " + names.length + " MongoDB databases");
      for (const name of names) db.getSiblingDB(name).dropDatabase();
      print("test-live-clean: " + leftover().length + " left");'
    if [ "${#mongo[@]}" -gt 0 ]; then
        "${mongo[@]}" --quiet --eval "$js"
    else
        echo "test-live-clean: no MongoDB (META_WHATSAPP_RS_TEST_MONGODB_URL is not set)"
    fi

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
# It leaves the service's crates that turn on adapter features out
# (meta-whatsapp-server and, since roadmap S7, the crates it composes):
# together they turn on the facade's reqwest, memory, postgres and axum,
# and in one --workspace build those features would be on for every
# crate, hiding an ungated link. For the
# same reason the meta-whatsapp-server-core lines check that crate alone,
# all targets: it must build on the facade without those features, with
# and without its `conformance` suites. The library is also checked alone,
# without features and with the suites, and documented with the suites
# (the workspace's doc line covers it without them): with --all-targets
# the crate's dev-dependencies (the facade's memory adapters, tokio's
# macros) would lend the library what it forgot to enable, and an
# intra-doc link of the suites' that names an ungated item would pass
# (roadmap S6). The crates the service's binary composes (roadmap S7) are
# each checked alone, all targets, so none borrows a facade feature from
# another; and the binary is built with `api-axum` and each backend alone:
# a `#[cfg(feature = "store-…")]` gate it forgot fails one of them.
#
# Each feature on its own, so a missing cfg gate cannot hide behind --all-features
features:
    cargo check -p meta-whatsapp-adapters --no-default-features
    cargo check -p meta-whatsapp-adapters --no-default-features --features memory
    cargo check -p meta-whatsapp-adapters --no-default-features --features sinks
    cargo check -p meta-whatsapp-adapters --no-default-features --features reqwest
    cargo check -p meta-whatsapp-adapters --no-default-features --features postgres
    cargo check -p meta-whatsapp-adapters --no-default-features --features sqlite
    cargo check -p meta-whatsapp-adapters --no-default-features --features redis
    cargo check -p meta-whatsapp-adapters --no-default-features --features ffmpeg
    cargo check -p meta-whatsapp-webhooks --no-default-features --all-targets
    cargo check -p meta-whatsapp-webhooks --features axum
    cargo check -p meta-whatsapp-client --no-default-features
    cargo check -p meta-whatsapp-client --features flows-endpoint
    cargo check -p meta-whatsapp-bot --all-targets
    cargo check -p meta-whatsapp-rs --no-default-features
    cargo check -p meta-whatsapp-rs --no-default-features --features reqwest
    cargo check -p meta-whatsapp-rs --no-default-features --features memory
    cargo check -p meta-whatsapp-rs --no-default-features --features sinks
    cargo check -p meta-whatsapp-rs --no-default-features --features postgres
    cargo check -p meta-whatsapp-rs --no-default-features --features sqlite
    cargo check -p meta-whatsapp-rs --no-default-features --features redis
    cargo check -p meta-whatsapp-rs --no-default-features --features axum
    cargo check -p meta-whatsapp-rs --no-default-features --features typst
    cargo check -p meta-whatsapp-rs --no-default-features --features flows-endpoint
    cargo check -p meta-whatsapp-rs --no-default-features --features ffmpeg
    cargo check -p meta-whatsapp-rs --no-default-features --features bot
    cargo check -p meta-whatsapp-rs
    cargo check -p meta-whatsapp-server-core
    cargo check -p meta-whatsapp-server-core --all-targets
    cargo check -p meta-whatsapp-server-core --all-targets --features conformance
    cargo check -p meta-whatsapp-server-core --features conformance
    RUSTDOCFLAGS="-D warnings" cargo doc -p meta-whatsapp-server-core --features conformance --no-deps
    cargo check -p meta-whatsapp-server-store-memory --all-targets
    cargo check -p meta-whatsapp-server-store-postgres --all-targets
    cargo check -p meta-whatsapp-server-http --all-targets
    cargo check -p meta-whatsapp-server-api-axum --all-targets
    cargo check -p meta-whatsapp-server --no-default-features --features api-axum,store-memory
    cargo check -p meta-whatsapp-server --no-default-features --features api-axum,store-postgres
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --exclude meta-whatsapp-server --exclude meta-whatsapp-server-http --exclude meta-whatsapp-server-api-axum --exclude meta-whatsapp-server-store-memory --exclude meta-whatsapp-server-store-postgres --no-default-features --no-deps

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
# One request at a time, 15 s apart, backing off on HTTP 429: a full crawl
# takes over an hour and a half. Flags (`just meta-docs --help`):
# --delay-secs N, --missing-only (retry what .meta-docs/README.md lists as
# unavailable), --force (refetch pages already mirrored), --out DIR.
#
# Mirror Meta's docs into .meta-docs/, one page every 15 s (--missing-only retries what its README lists)
meta-docs *args:
    cargo run -q --manifest-path .xtask/Cargo.toml -- meta-docs {{args}}
