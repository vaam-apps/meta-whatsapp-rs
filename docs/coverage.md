# Feature coverage

What `wa-rs` covers of Meta's WhatsApp Business Platform, in priority order.
Doc paths are relative to
`https://developers.facebook.com/documentation/business-messaging/whatsapp/`.

Status: **done** = implemented and tested against the docs' request/response
examples; **partial** = the listed subset only; **planned** = not yet; **out
of scope** = deliberately not covered, with the reason.

| # | Feature | Priority | Module | Doc paths | Status |
| --- | --- | --- | --- | --- | --- |
| 1 | Embedded Signup (v4): code exchange, business token, `debug_token`, subscribe app to WABA, register number, encrypted token vault, session binding, launch options, session-info parsing | **most requested** | `wa_client::embedded_signup` | `embedded-signup/*`, `access-tokens` | planned |
| 2 | Webhooks: verification, `X-Hub-Signature-256`, every documented field, BSUID identities, normalized events, dedup, sinks, axum router, SSE | very requested | `wa-webhooks` | `webhooks/*`, `business-scoped-user-ids` | planned |
| 3 | Templates: create/edit/delete/list/get, library, migrate, compare, named & positional params, media/carousel/LTO/coupon/call-permission, send-time components | very requested | `wa_client::templates` | `templates/*` | planned |
| 4 | Authentication templates (copy code, one-tap, zero-tap), previews, bulk upsert, OTP service | very requested | `wa_client::authentication` | `templates/authentication-templates/*` | planned |
| 5 | Catalogs & commerce: commerce settings, catalog ↔ WABA, catalog/SPM/MPM/product-carousel messages & templates, `order` webhooks | requested | `wa_client::commerce`, `messages`, `wa-webhooks` | `catalogs/*` | planned |
| 6 | In-App Signup (opt-in deep links) | requested | `wa_client::signups` | `in-app-signup` | planned |
| 7 | Messages: text, media, location, contacts, address, interactive (buttons, list, CTA URL, location request, flow, media carousel), reactions, stickers, contextual replies, mark read, typing indicators, link previews | core | `wa_client::messages` | `messages/*`, `typing-indicators` | planned |
| 8 | Media: upload, URL, streaming download + SHA-256 check, delete, Resumable Upload (template header handles) | core | `wa_client::media` | `business-phone-numbers/media`, `reference/media/*` | planned |
| 9 | Business-scoped user IDs & usernames: send by BSUID, identities in every webhook | core (mandatory 2026) | `wa_core::recipient`, `wa-webhooks` | `business-scoped-user-ids` | planned |
| 10 | Phone numbers: list/get, request & verify code, register/deregister, two-step PIN, settings, display names, conversational components | core | `wa_client::phone_numbers` | `business-phone-numbers/*`, `display-names` | planned |
| 11 | Business profile | core | `wa_client::business_profile` | `business-profiles` | planned |
| 12 | WABA management: details, subscribed apps & callback override, assigned users, client/owned WABAs | core | `wa_client::waba` | `whatsapp-business-accounts`, `webhooks/override` | planned |
| 13 | Marketing Messages API for WhatsApp (send, TTL, onboarding) | e-commerce | `wa_client::marketing` | `marketing-messages/*` | planned |
| 14 | WhatsApp Flows: management API, send, data-endpoint crypto, business public key | CMS | `wa_client::flows` | `flows/*` | planned |
| 15 | Analytics: messaging, pricing, template, call | marketing | `wa_client::analytics` | `analytics` | planned |
| 16 | QR codes & short links | marketing | `wa_client::qr_codes` | `qr-codes` | planned |
| 17 | Block users | CMS | `wa_client::block_users` | `block-users` | planned |
| 18 | Groups API | CMS | `wa_client::groups` | `groups/*` | planned |
| 19 | Calling API signalling (settings, permissions, connect/accept/reject/terminate) | later | `wa_client::calling` | `calling/*` | planned (WebRTC media out of scope) |
| 20 | Direct Send (beta): `category` on free-form sends | later | `wa_client::messages` | `direct-send/*` | planned |
| 21 | Coexistence (WhatsApp Business app users): onboarding, contacts/history sync, echoes | CMS | `embedded_signup`, `wa-webhooks` | `embedded-signup/onboarding-business-app-users` | planned |
| 22 | Error codes → `ErrorKind` (retry, window, opt-out, throttling) | core | `wa_core::error` | `support/error-codes` | done |
| 23 | Typst documents (invoice, receipt, voucher) for document/image messages | e-commerce | `wa-typst` | — | planned |
| 24 | Pricing objects on status webhooks (per-message pricing) | analytics | `wa-webhooks` | `pricing` | planned |
| 25 | Identity change check (`identity_key_hash`) | security | `wa-webhooks`, `phone_numbers` settings | `identity-change` | planned |
| 26 | Solution partner APIs: credit lines, partner-led business verification, multi-partner solutions, WABA/number migration | partners | — | `solution-providers/*` | planned |
| 27 | Conversation routing / handover (standby, thread control) | later | — | `conversation-routing/*` | planned |
| 28 | Account model evolution (Messaging Accounts, beta) | later | — | `account-model-evolution/*` | planned |
| 29 | CTWA welcome message sequences | later | — | `ctwa/welcome-message-sequences` | planned |
| 30 | Payments (India UPI, Brazil Pix/Boleto) | — | — | `payments/*` | out of scope (market-specific; revisit on demand) |
