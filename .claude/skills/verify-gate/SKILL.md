---
name: verify-gate
description: "What counts as verified in wa-rs — running `just ci` (lint, check, test, consumer-skill stamps, doc, per-feature builds, cargo-deny, live Postgres/Redis tests), reading its exit code, and the traps (skipped live tests, reconstructed commands, agent self-reports). Use before claiming anything passes, before committing, and when reviewing a sub-agent's result."
metadata:
  internal: true
---

# Verification

```bash
just ci > /tmp/ci.log 2>&1; echo $? > /tmp/ci.exit
cat /tmp/ci.exit   # 0 or it is not verified
```

`ci` = `lint check test skills-check doc features deny test-live`. `skills-check`
needs the git history (a shallow clone fails it).

## Traps

- **Skipped ≠ passed.** `live_*` tests print `ok` when their service URL is
  unset. Only `just test-live` sets `WA_RS_REQUIRE_LIVE=1`, which turns a
  missing service into a failure. Check its output shows the live tests
  *ran* (non-zero count).
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
