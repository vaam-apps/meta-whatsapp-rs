@AGENTS.md

## Claude Code specifics

- Project skills: `.claude/skills/` — `meta-docs` (look up the real spec),
  `add-graph-endpoint`, `add-webhook-field`, `error-tree`, `verify-gate`,
  `typst-templates`. Load the matching one before starting that kind of
  work.
- Project agents: `.claude/agents/` — `meta-docs-researcher` (spec
  extraction), `wa-implementer` (draft a module), `wa-sabotage-reviewer`
  (adversarial review-and-fix, mutation-driven), `wa-security-reviewer`
  (secrets, signatures, crypto, tokens), `wa-conventions-reviewer` (public
  API, blast radius, docs/skills parity).
- Pipeline for non-trivial work: implement → sabotage review with a
  distinct lens → remediate → re-run the original failure → `just ci` on the
  final head, exit code read from a file.
- The devcontainer (`.devcontainer/`) is a default-deny sandbox with the
  toolchain, `just`, `cargo-deny`, `typst`, and Postgres/Redis sidecars.
