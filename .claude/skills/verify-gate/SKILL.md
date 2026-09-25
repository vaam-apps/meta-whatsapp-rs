---
name: verify-gate
description: "What counts as verified in meta-whatsapp-rs — running `just ci` (lint, check, test, consumer-skill stamps, doc, per-feature builds, cargo-deny, live Postgres/Redis tests), reading its exit code, and the traps (skipped live tests, reconstructed commands, agent self-reports). Use before claiming anything passes, before committing, and when reviewing a sub-agent's result."
metadata:
  internal: true
---

# Verification

```bash
just ci > /tmp/ci.log 2>&1; echo $? > /tmp/ci.exit
cat /tmp/ci.exit   # 0 or it is not verified
```

`ci` = `lint check test skills-check skills-ts doc features deny test-live`.
`skills-check` needs the git history (a shallow clone fails it);
`skills-ts` needs Node of the major version in `tools/skills-ts/.nvmrc`
(CI installs exactly that version) and the npm registry for `npm ci`.

## Traps

- **A green PR does not prove main after a squash.** `skills-check` passes
  on main only if the squash body is `just squash-body <pr>`: without it,
  stamps naming the PR's commits resolve nowhere and main turns red.
- **Skipped ≠ passed.** `live_*` tests print `ok` when their service URL is
  unset. Only `just test-live` sets `META_WHATSAPP_RS_REQUIRE_LIVE=1`, which turns a
  missing service into a failure. Check its output shows the live tests
  *ran* (non-zero count). It runs two crates' live tests
  (`meta-whatsapp-adapters`, `meta-whatsapp-server`): check both counts.
- **`just features`' rustdoc line leaves `meta-whatsapp-server` out** on
  purpose: the service turns on the facade's features, and in one
  `--workspace` build they would hide an ungated doc link.
- **Don't reconstruct the gate.** `cargo test` is not `just test`
  (`--all-features`); `cargo clippy` is not `just lint` (`-D warnings`,
  `--all-targets`).
- **Agent reports are claims.** Before building on a sub-agent's branch:
  `git rev-parse <branch>`, `git log --oneline main..<branch>`, the diff is
  non-empty, and `just ci` passes there.
- **Mutation check** for new logic: break the guard (flip a comparison,
  delete a validation) and confirm a test fails; restore.
- **CI is the evidence** when it disagrees with local: find out why before
  claiming either.
