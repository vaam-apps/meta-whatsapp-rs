# wa-rs

WhatsApp Business Platform for Rust — a typed client for Meta's Cloud API
and Business Management API, webhooks, pluggable storage/transport/sink
adapters, and Typst-rendered documents.

Built for three jobs:

- **Marketing** for an e-commerce store: templates, the Marketing Messages
  API, In-App Signup opt-ins, catalogs and product messages.
- **In-app chat** in a CMS: each merchant onboards their own number with
  **Embedded Signup**; customer messages arrive by webhook, are stored per
  conversation and streamed live to the merchant's inbox.
- **Authentication**: WhatsApp OTP through authentication templates.

> Status: early. See [docs/coverage.md](docs/coverage.md) for what is
> implemented, and [docs/architecture.md](docs/architecture.md) for the
> design.

## Crates

| Crate | Role |
| --- | --- |
| `wa-rs` | Facade: depend on this. Feature flags pick adapters. |
| `wa-core` | Error tree, ids, config, ports (`HttpTransport`, `KvStore`, `ConversationStore`, `EventSink`, `Clock`). |
| `wa-client` | Graph API client, one module per endpoint family. |
| `wa-webhooks` | Signature/verify-token checks, typed payloads, normalized events, dedup, axum router + SSE. |
| `wa-adapters` | reqwest transport; memory, Postgres, Redis stores; channel/broadcast/fan-out sinks. |
| `wa-typst` | Typst → PDF/PNG (invoices, receipts, vouchers) for document and image messages. |

## Development

```bash
just            # list recipes
just ci         # the gate CI runs
just meta-docs  # mirror Meta's docs locally (gitignored) for grep
```

Open the repo in the dev container (`.devcontainer/`) for a sandboxed
Claude Code environment: pinned toolchain, default-deny egress firewall,
Postgres and Redis sidecars.

Agents: see [AGENTS.md](AGENTS.md). Claude Code project skills and agents
live in `.claude/`.

## Naming

`wa-rs` is already taken on crates.io by an unrelated project, so the
workspace is `publish = false` until crate names are chosen.

## License

MIT
