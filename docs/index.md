---
layout: home
title: meta-whatsapp-rs
titleTemplate: WhatsApp Business Platform for Rust

hero:
  name: meta-whatsapp-rs
  text: WhatsApp Business Platform for Rust
  tagline: A typed client for Meta's Cloud API and Business Management API, webhooks, pluggable storage, transport and sink adapters, and Typst-rendered documents.
  image:
    src: /logo.svg
    alt: meta-whatsapp-rs
  actions:
    - theme: brand
      text: Get started
      link: /getting-started
    - theme: alt
      text: Integrator guides
      link: /guides/README
    - theme: alt
      text: View on GitHub
      link: https://github.com/vaam-apps/meta-whatsapp-rs

features:
  - title: Typed Graph API client
    details: One module per Graph endpoint family, every request through one place that handles auth, retries and error decoding. Errors are branched on ErrorKind, never on their text.
    link: /guides/getting-started
    linkText: First message
  - title: Webhooks, verified and deduplicated
    details: X-Hub-Signature-256 and verify-token checks, typed payloads, normalized events, a dedup guard for Meta's retries, and an axum router with a live SSE stream.
    link: /guides/webhooks
    linkText: Webhooks guide
  - title: Embedded Signup and a CMS inbox
    details: Each merchant connects their own number. Tokens are stored encrypted in a vault, messages are recorded per conversation, and replies are refused outside the 24-hour window.
    link: /guides/embedded-signup
    linkText: Embedded Signup guide
  - title: OTP, documents and bots
    details: WhatsApp OTP codes stored as keyed hashes and rate-limited per number, Typst invoices, receipts and vouchers rendered to PDF or PNG, and a bot framework with paced broadcasts.
    link: /guides/otp-login
    linkText: OTP login guide
---

## Built for three jobs

- **Marketing** for an e-commerce store: templates, the Marketing Messages
  API, In-App Signup opt-ins, catalogs and product messages.
- **In-app chat** in a CMS: each merchant onboards their own number with
  **Embedded Signup**; customer messages arrive by webhook, are stored per
  conversation and streamed live to the merchant's inbox.
- **Authentication**: WhatsApp OTP through authentication templates.

Apps that are not written in Rust use it through **meta-whatsapp-server**, an
HTTP service built on the library and deployed next to them, with a `/v1` REST
API described by a committed OpenAPI document. See
[Run the service](guides/server.md).

## Where to read next

- [Getting started](getting-started.md): add the dependency, pick the feature
  flags, find the crates and run the examples.
- [Integrator guides](guides/README.md): one guide per job, from the Meta app
  setup to production.
- [Feature coverage](coverage.md) says what is implemented, and
  [the architecture](architecture.md) is the design spec.
