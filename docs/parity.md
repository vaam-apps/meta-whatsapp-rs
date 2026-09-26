# Parity: Zaileys, Meta's Cloud API and meta-whatsapp-rs

Verified against `main` at b6fc893 (PR #20) on 2026-09-26; rows 17,
19 and 84–88 again when the bot framework (roadmap B1) landed, against
its code; the service's cells of rows 5, 6, 9, 33, 84, 91, 112 and 113
again when its core was extracted (roadmap S1), against the core's
code. Every cell about us was checked against the code: a cell that
says a thing is done names the symbol that does it, and what the service
(`meta-whatsapp-server`) does is read from its routes, not from its
design. The plan to close the gaps is [roadmap.md](roadmap.md); the Meta
platform categories are in [categories.md](categories.md); the
per-feature list is [coverage.md](coverage.md).

## Where we stand

**39 of the 131 counted rows are done on both sides.** Parity is not
reached.

| 131 counted rows | done | partial | gap | n/a (that side does not carry it) |
| --- | --- | --- | --- | --- |
| Library | 89 | 23 | 18 | 1 |
| Service | 39 | 24 | 67 | 1 |

- The table has 155 rows. 24 of them are not counted: they work only
  over WhatsApp Web ("n/a — unofficial protocol"). The counted rows'
  n/a column is a side that does not carry the capability at all: the
  library for row 115 (packaging), the service for row 63 (a Flow JSON
  builder).
- Each of the library's 41 partial or gap rows is cited by a
  [roadmap](roadmap.md) item. Each of the service's 91 names, in its
  status, the roadmap items that bring it; by family (a row can name
  two): M2 8, M3 11, M4 2, M5 70, S 1 (the modular split), L 1 (L22a,
  tooling), P 2 (payments).
- The largest gaps: the service routes for the modules the design once
  left "on demand" (M5), the inbox, live events and onboarding over HTTP
  (M2, M3), and the rest of the bot framework (section G: its commands,
  middleware, plugins, access lists and Markdown replies exist, B1;
  subcommands and flags, rich replies beyond text, paced broadcast,
  scheduling and auto-delete do not, B1b, B1c, B2–B4).

## What parity means

The owner's definition (2026-09-26):

- **Cloud API only.** Every capability of Meta's Cloud API is a row.
  A Zaileys capability that works only over WhatsApp Web is listed and
  marked "n/a — unofficial protocol"; meta-whatsapp-rs has no WhatsApp
  Web provider and will not get one.
- **Zaileys' framework features are in scope**, although Zaileys runs
  them only over WhatsApp Web: commands, middleware, plugins, broadcast
  with rate limiting, scheduled messages, auto-delete, message stores,
  rich markdown responses, multiple accounts and typed errors. We build
  them on Cloud API webhooks.
- **Done** means every row that is not n/a is done in the library, and
  in the service wherever the service is meant to carry it
  ([roadmap.md § Done criteria](roadmap.md#done-criteria-for-parity)).
- This is the comparison with Zaileys and Meta's Cloud API, not
  CONTRIBUTING.md's "docs and skills parity" (the docs and skills
  agreeing with the code).

## How to read the table

- **Status** is `library / service`, each one of done, partial, gap or
  n/a. A side is **done** when it carries every endpoint and field the
  capability's pages document for the Graph API version we target
  (v25.0), **partial** when it carries some, **gap** when it carries
  none, and **n/a** when that side does not carry the capability. A
  part another row covers counts in that row, and a part outside the
  Cloud API (Meta's ads APIs, say) is named and not counted.
  Something the service will carry counts as a gap (or partial) until
  it ships; the [roadmap](roadmap.md) items that bring it are in
  parentheses. An n/a row says why instead.
- The same grades hold in [coverage.md](coverage.md), whose
  **planned** is a gap here and whose **out of scope** is an n/a, and
  in [categories.md](categories.md), which has no n/a (a category's
  n/a parts are named in its notes).
- **Zaileys (Web)** and **Zaileys (Cloud)**: Yes, No, Partial, or `—`
  when Zaileys does not mention the capability. The wording is ours.
- **Meta Cloud API**: doc paths relative to
  `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
  (append `.md` for Markdown). **(nm)** marks one of the 5 pages Meta
  lists that the local mirror (`.meta-docs/`, `just meta-docs`) cannot
  fetch (`flows/changelog`, `overview`, `pricing/prepaid-billing`, `webhooks/reference/messaging-handovers`, `webhooks/reference/standby`: missing or gated at Meta); a
  claim resting on one rests on the reference pages or on our code's
  fixtures. Every other page cited is in the mirror.
- **Library** and **Service**: code cited by module path and symbol,
  never by line. `client::` is `meta_whatsapp_client::`, `webhooks::` is
  `meta_whatsapp_webhooks::`, `core::` is `meta_whatsapp_core::`,
  `adapters::` is `meta_whatsapp_adapters::`, `typst::` is
  `meta_whatsapp_typst::` (the facade `meta_whatsapp_rs` re-exports each
  under the same name), `inbox::` is `meta_whatsapp_rs::inbox::`,
  `server::` is `meta_whatsapp_server::`, the service's crate, and
  `server_core::` is `meta_whatsapp_server_core::`, its framework-free
  core (the domain, the authorization order, event routing and polling,
  idempotency, the error model as data, and the ports its backends
  implement); an item the service re-exports from its core is cited
  where it is declared, in the core. The bot
  framework's items (`meta_whatsapp_bot::`, re-exported as
  `meta_whatsapp_rs::bot`) are cited by type (`Command::alias`) in a
  cell that names `meta-whatsapp-bot`. A type already qualified in a
  row is not qualified again in it. Routes are
  the service's `/v1` API; "the send union" is its message types
  (`server::api::messages::MessageType`: text, the five media types,
  location, contacts, reaction, template, and interactive `button`,
  `list`, `cta_url`).
- Row numbers are permanent: other documents cite them.

## Sources

- **Zaileys** ([zeative/zaileys](https://github.com/zeative/zaileys), a
  Node library over two providers, Baileys for WhatsApp Web and Meta's
  Cloud API): its [feature matrix](https://zaileys.kejaa.id/feature-matrix)
  and the feature pages in its navigation (getting started, core
  concepts, messaging, bots, chats, groups, data, and its Cloud pages),
  read on 2026-09-26 through summaries. Where its pages disagree (its
  providers page against the matrix, for locations and HTML apps; the
  plugins and middleware pages against the matrix and the Cloud
  overview), the matrix and the Cloud overview win here. Not read: the
  quickstart, installation, client reference, changelog and the recipes
  other than auto-reply. The Zaileys columns were not re-read for this
  revision.
- **Meta**: `.meta-docs/`, crawled on 2026-09-24 and completed on
  2026-09-26. The rows that rested on the pages the first crawl missed
  were checked again against those pages that day: their key facts
  (limits, fields, endpoints) and every claim that something is absent,
  not each sentence.
- **Us**: the code at b6fc893; for rows 17, 19 and 84–88, the bot
  framework's (B1).

## The table

### A. Connection, credentials, accounts

| # | Area | Capability | Zaileys (Web) | Zaileys (Cloud) | Meta Cloud API | Library | Service | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Connection | QR-code login | Yes | No | none: the Cloud API authenticates with access tokens (`access-tokens`) | — | — | n/a — unofficial protocol |
| 2 | Connection | Pairing-code login | Yes | No | none | — | — | n/a — unofficial protocol |
| 3 | Connection | Saved session, logout, a budget of login attempts | Yes | No (the Cloud API is stateless HTTPS) | none | — | — | n/a — unofficial protocol |
| 4 | Connection | Token authentication; checking the token and the number at start | No | Yes (checks the token and the phone number id when it connects) | `access-tokens`, `permissions.md` | `client::ClientBuilder::access_token`; token inspection `client::embedded_signup::EmbeddedSignup::debug_token`; `client::phone_numbers::PhoneNumber::get`. No single "check my credentials" call | incomplete configuration refuses the start (`server::config`); attaching a WABA checks it with Meta: `POST /v1/admin/tenants/{id}/wabas` (`server::api::admin::attach_waba`) | done / done |
| 5 | Connection | Reconnect semantics on the Cloud API: back off when throttled, never replay a send that timed out | Yes (reconnects with growing delays) | Partial (waits after a rate limit) | `throughput.md` (error 130429), `support/error-codes.md` | `client::RetryPolicy` (jittered exponential backoff, `Retry-After` honoured, a send replayed only after a throttle); `core::ErrorKind::is_retryable` | `may_have_been_sent` on `502`/`504`; `Idempotency-Key` replays an answer, never a send (`server_core::idempotency`, over HTTP `server::idempotency`) | done / done |
| 6 | Connection | Token expiry and refresh | — | — | `access-tokens` (business tokens need no re-authentication; no refresh call is documented) | expiry recorded (`client::embedded_signup::StoredBusinessToken::expires_at`); nothing refreshes it (OPEN_QUESTIONS #8, decided: re-onboard, surface the expiry early, L11e) | `409 reconnect_required` (`server_core::authz`, over HTTP `server::auth`); the expiry surfaced before it lapses in M3e | partial / partial (M3e) |
| 7 | Connection | Webhook endpoint: the verify-token handshake, `X-Hub-Signature-256` | No | Yes (`client.webhook()`; an unsigned mode for development) | `webhooks/overview.md`, `webhooks/create-webhook-endpoint.md` | `webhooks::verify::verify_subscription`; `webhooks::SignatureVerifier` (several secrets, fail-closed, no unsigned mode); `webhooks::WebhookHandler::deliver`; the axum `webhooks::router` | `GET` and `POST /webhooks/meta` (`server::api::webhooks::verify`, `receive`): a missing or malformed signature header refused before the body is read, the signature checked against every app secret before parsing, 3 MiB | done / done |
| 8 | Connection | Webhook deduplication | — | — | `webhooks/overview.md` (Meta retries a delivery) | `webhooks::dedup::DedupGuard` (a lease: pending, then done or released) | the library's lease, shared by every replica through the Postgres `KvStore` | done / done |
| 9 | Accounts | Several numbers and accounts, routed by `phone_number_id` | Yes (one client per session) | Yes (one client per phone number id; a router reads the body) | `solution-providers/manage-accounts.md` | `client::Client::with_token`; `client::embedded_signup::TokenVault::get_by_phone_number`; every event carries its business number | tenants, keys and WABA bindings (`server::api::admin`); each webhook event routed to the tenant that owns its number or WABA (`server_core::events::route`, `owner`) | done / done |
| 10 | Accounts | A live event stream for a UI | — | — | none (ours to build) | `webhooks::sse`; `adapters::sink::BroadcastSink` | polling: `GET /v1/events` (`server::api::events::list_events`); SSE and webhooks-out in M2 | done / partial (M2b, M2c) |

### B. Sending messages

| # | Area | Capability | Zaileys (Web) | Zaileys (Cloud) | Meta Cloud API | Library | Service | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 11 | Messaging | Text, with WhatsApp formatting | Yes | Yes | `messages/text-messages`; `reference/whatsapp-business-phone-number/message-api.md` | `client::messages::OutboundMessage::text` | `POST /v1/numbers/{pn}/messages` (`server::api::messages::send_message`), type `text` | done / done |
| 12 | Messaging | Link previews | — | — | `link-previews`, `messages/text-messages` | `OutboundMessage::text_with_preview` | `text.preview_url` | done / done |
| 13 | Messaging | Reply to (quote) a message | Yes | Yes | `messages/contextual-replies` | `OutboundMessage::reply_to` | `reply_to` | done / done |
| 14 | Messaging | @-mentions | Yes | No (dropped) | no mention object in `reference/whatsapp-business-phone-number/message-api.md` or in the groups guides (`groups/groups-messaging`) | — | — | n/a — unofficial protocol |
| 15 | Messaging | Disappearing messages | Yes | No | not supported: `groups` lists them among the types groups do not support, `embedded-signup/onboarding-business-app-users` says coexistence turns them off in one-to-one chats, and `reference/whatsapp-business-phone-number/message-api.md` has no such option | — | — | n/a — unofficial protocol |
| 16 | Messaging | Rich responses in Meta AI's block format | Yes (sent as a forwarded bot message) | No | undocumented format | — | — | n/a — unofficial protocol |
| 17 | Messaging | Rich markdown responses rendered as Cloud messages: formatting, long text split, images as media, suggestions as buttons or lists, product cards as carousels | Yes (through row 16) | No | building blocks only: text (at most 4096 characters, `messages/text-messages`), media, interactive (`reference/whatsapp-business-phone-number/message-api.md`) | formatting and the split, in `meta-whatsapp-bot`: `Ctx::reply_markdown` renders with a `MarkdownRenderer` (the default `Renderer`: bold, italic, strike, code, quotes, lists, links, tables as padded columns or `header: value` lines within `Renderer::table_max_width` and `Renderer::table_max_growth`) and sends messages of at most 4096 UTF-16 code units, cut between blocks. Not yet: images go out as their alt text and URL, not as media, and no suggestions become buttons or lists, nor product cards carousels (B1c) | gap (the bot API, M5k) | partial / gap (M5k) |
| 18 | Messaging | HTML apps in the chat bubble | Partial (Android only, undocumented) | No | undocumented; the official in-chat UI is Flows (rows 60–63) | — | — | n/a — unofficial protocol |
| 19 | Messaging | Emoji reactions | Yes | Yes | `messages/reaction-messages` | `client::messages::Messages::react`; `OutboundMessage::reaction`; in a bot, `Ctx::react` (the received message) | type `reaction` | done / done |
| 20 | Messaging | Edit a sent message | Yes | No | no business-side edit in `reference/whatsapp-business-phone-number/message-api.md`; a user's edit arrives as an unsupported type (`webhooks/reference/messages/edit.md`) | inbound only: `webhooks::fields::MessageContent::Edit` | inbound only, in `message_received` events | n/a — unofficial protocol (sending an edit) |
| 21 | Messaging | Delete (revoke) a sent message | Yes | No | no business-side delete in `reference/whatsapp-business-phone-number/message-api.md`; an inbound revoke for coexistence (`webhooks/reference/messages/revoke.md`) | inbound only: `webhooks::fields::MessageContent::Revoke`; the inbox leaves a tombstone (`core::store::ConversationStore::revoke`) | inbound only, recorded by the service's inbox | n/a — unofficial protocol (sending a revoke) |
| 22 | Messaging | Pin a message | Yes | No | in groups only (`groups/groups-messaging`); an inbound `pin` is an unsupported type (`webhooks/reference/messages/unsupported.md`) | `OutboundMessage::pin`, `unpin`; `client::groups::Groups::pin_message` | not in the send union (`server::api::messages::MessageType`) | done / gap (M5a) |
| 23 | Messaging | Forward a message | Yes | No | no forward call in `reference/whatsapp-business-phone-number/message-api.md`; a media id can be sent again | a media id can be sent again (`OutboundMessage::image_id` and the other `*_id` constructors) | a media `id` can be sent again | n/a — unofficial protocol |
| 24 | Messaging | Location | Yes | Yes | `messages/location-messages` | `OutboundMessage::location` | type `location` | done / done |
| 25 | Messaging | Contact cards | Yes | Yes | `messages/contacts-messages` (up to 257 cards) | `OutboundMessage::contacts` | type `contacts` | done / done |
| 26 | Messaging | Polls | Yes | No | an unsupported inbound type (`webhooks/reference/messages/unsupported.md`) | inbound only: `MessageContent::Unsupported` | inbound only | n/a — unofficial protocol |
| 27 | Messaging | Event invitations | Yes (groups) | No | none in the mirror | — | — | n/a — unofficial protocol |
| 28 | Messaging | Group invite card | Yes | No | an inbound `group_invite` is unsupported (`webhooks/reference/messages/unsupported.md`); an invite link goes out as a utility template from Meta's template library, or as text or a CTA URL (`groups/reference`) | the link: `client::groups::Group::invite_link` (row 79) | — | n/a — unofficial protocol (the card; the link is row 79) |
| 29 | Messaging | Ask a user for their phone number | Yes | No | "request contact info" for users who write by username (`business-scoped-user-ids`) | `client::messages::Interactive::RequestContactInfo` | not in the send union | done / gap (M5a) |
| 30 | Messaging | Post a status | Yes | No | no status endpoint anywhere in the mirror | — | — | n/a — unofficial protocol |
| 31 | Messaging | Mark as read | Yes | Yes | `messages/mark-message-as-read` | `Messages::mark_read` | `POST /v1/numbers/{pn}/messages/{message_id}/read` (`server::api::messages::mark_read`) | done / done |
| 32 | Messaging | Typing indicator | Yes | Partial (only with a read receipt, which is how the Cloud API works) | `typing-indicators.md` | `Messages::mark_read_with_typing_indicator` | `typing_indicator` on the read route | done / done |
| 33 | Messaging | Delivery statuses (sent, delivered, read, played, failed; pricing, conversation, errors) | Yes | Yes (adds the conversation id and the error) | `webhooks/reference/messages/status.md`, `pricing.md` | `webhooks::WebhookEvent::StatusUpdated`; `webhooks::fields::MessageStatus` (`played` included); `webhooks::fields::Pricing` | `status_updated` events on `GET /v1/events` (`server_core::events::TENANT_EVENT_TYPES`) | done / done |
| 34 | Messaging | Opaque callback data on a send | — | — | `biz_opaque_callback_data`, at most 512 characters (Meta's `changelog`), echoed in status webhooks (`webhooks/reference/messages/status.md`); the message reference's schema (`reference/whatsapp-business-phone-number/message-api.md`) does not list it | `OutboundMessage::callback_data` | `callback_data` | done / done |
| 35 | Messaging | Send to a group | Yes (groups) | No | `reference/whatsapp-business-phone-number/message-api.md` (`recipient_type` `group`) | `core::recipient::Recipient::Group` | a `{"group_id"}` recipient | done / done |
| 36 | Messaging | Send by BSUID or parent BSUID | Partial (LID addresses) | No | `business-scoped-user-ids` | `core::recipient::Recipient::User` | a `{"user_id"}` recipient | done / done |
| 37 | Messaging | Direct Send (beta: category, TTL, configuration) | — | — | `direct-send`, `direct-send/api-reference` | `OutboundMessage::category`, `ttl_seconds`, `direct_send_template_name` | refused today, `422 unsupported_message_type` (design §4.3) | done / gap (M5a) |
| 38 | Messaging | Payment messages (order details, order status) | — | — | `payments/payments-in/orderdetailstemplate.md`, `payments/payments-in/orderstatustemplate.md`, `payments/payments-br/orders.md` | only through `client::messages::MessageContent::Raw` | — (a Graph passthrough stays a non-goal: typed routes come with payments) | partial / gap (P2) |

### C. Media

| # | Area | Capability | Zaileys (Web) | Zaileys (Cloud) | Meta Cloud API | Library | Service | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 39 | Media | Image, video, document (by id or link; caption, filename) | Yes | Yes | `messages/image-messages`, `messages/video-messages`, `messages/document-messages` | `client::messages::Image`, `Video`, `Document` | types `image`, `video`, `document` by `id` or `https` `link` | done / done |
| 40 | Media | Stickers | Yes | Yes | `messages/sticker-messages` | `client::messages::Sticker` | type `sticker` | done / done |
| 41 | Media | Audio file | Yes | Yes | `messages/audio-messages` | `client::messages::Audio` | type `audio` | done / done |
| 42 | Media | Voice note | Yes | Partial (arrives as audio) | `messages/audio-messages` (voice messages, Ogg with Opus) | `Audio::voice` | `audio.voice` | done / done |
| 43 | Media | Round video notes, view-once, albums (GIF playback: row 154) | Yes | No, or sent as plain media | an inbound `gif` is unsupported (`webhooks/reference/messages/unsupported.md`); view-once is unsupported in groups and turned off for coexistence (`groups`, `embedded-signup/onboarding-business-app-users`); none of these in `reference/whatsapp-business-phone-number/message-api.md`; the nearest to an album is a media carousel (row 51) | — | — | n/a — unofficial protocol (a GIF as a template header is row 154) |
| 44 | Media | Upload, type and size checked | Yes (URL, file or buffer) | Yes | `reference/whatsapp-business-phone-number/media-upload-api.md` | `client::media::Media::upload` | `POST /v1/numbers/{pn}/media` (`server::api::media::upload_media`) | done / done |
| 45 | Media | Download received media | Yes | Yes (by key) | `reference/media/media-api.md`, `reference/media/media-download-api.md` | `Media::download` (SHA-256 checked; the token goes only to Graph and `lookaside.fbsbx.com`) | `GET /v1/numbers/{pn}/media/{media_id}` (`server::api::media::download_media`); whether Meta serves a *received* media id under the route's `phone_number_id` check is unverified (OPEN_QUESTIONS #43, decided: exempt the ids the service recorded as received, M2e) | done / partial (M2e) |
| 46 | Media | Delete media | — | — | `reference/media/media-api.md` | `Media::delete` | `DELETE /v1/numbers/{pn}/media/{media_id}` (`server::api::media::delete_media`) | done / done |
| 47 | Media | Resumable upload (template header handles, profile pictures) | — | — | Graph's Resumable Upload API, documented outside the WhatsApp docs; the WhatsApp pages that rely on it: `templates/template-media.md` (header handles), `reference/whatsapp-business-profile/whatsapp-business-profile-node-api.md` (pictures) | `Media::resumable_upload` | not exposed | done / gap (M5c1) |
| 48 | Media | Converting media (voice to Opus, stickers from images, resizing) | Yes | Yes (runs locally: ffmpeg, sharp) | none (client side); accepted formats: `messages/audio-messages`, `messages/sticker-messages` | gap: type and size checks only | gap | gap / gap (M5c1) |
| 154 | Media | A GIF (an animated mp4) as a template's media header | Yes (GIF playback, row 43) | No, or sent as plain media (row 43) | Marketing Messages API only: `marketing-messages/features.md` (an animated GIF header), `templates/components.md` (header format `GIF`: an mp4 of at most 3.5 MB); no mirrored page shows the send-time header parameter for a GIF | the definition: `client::templates::HeaderFormat::Gif`; the send-time parameter is not typed (`client::templates::Parameter::Other` would carry it verbatim; unverified); sent through `client::marketing::Marketing::send` | creating the template takes Meta's JSON (`POST /v1/wabas/{waba_id}/templates`); Marketing Messages API sends are not exposed (row 136) | partial / gap (M5h) |

### D. Interactive messages and Flows

| # | Area | Capability | Zaileys (Web) | Zaileys (Cloud) | Meta Cloud API | Library | Service | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 49 | Interactive | Reply buttons (at most 3 on the Cloud API), header, footer | Yes (up to 10) | Partial (3) | `messages/interactive-reply-buttons-messages` (up to three) | `client::messages::ReplyButtons` (1–3 checked) | interactive `button` | done / done |
| 50 | Interactive | A media header on buttons | Yes | No | `reference/whatsapp-business-phone-number/message-api.md` (header: text, image, video, document) | `client::messages::Header` | header `text`, `image`, `video`, `document` | done / done |
| 51 | Interactive | Carousels (interactive media carousel, product carousel, template carousels) | Yes | No | `messages/interactive-media-carousel-messages` (2–10 cards), `catalogs/interactive-product-carousel-messages`, `templates/marketing-templates/media-card-carousel-templates.md` | `client::messages::MediaCarousel`; `client::messages::ProductCarousel`; `client::templates::TemplateMessage::carousel` | template carousels pass through; interactive carousels are not in the send union | done / partial (M5a) |
| 52 | Interactive | URL button (CTA URL) | Yes | Partial (one button) | `messages/interactive-cta-url-messages` | `client::messages::CtaUrl` | interactive `cta_url` | done / done |
| 53 | Interactive | Copy-code and call buttons | Yes | No | copy code: `templates/authentication-templates/copy-code-button-authentication-templates.md`, `templates/marketing-templates/coupon-templates.md`; calls: `calling/call-button-messages-deep-links` | `TemplateMessage::copy_code_button`; `client::templates::Button::phone_number`, `Button::voice_call`; `client::messages::Interactive::VoiceCall` | template buttons pass through; interactive `voice_call` is not in the send union | done / partial (M5a) |
| 54 | Interactive | Lists (at most 10 rows) | Yes | Yes | `messages/interactive-list-messages` (10 sections, 10 rows in all) | `client::messages::ListMessage` | interactive `list` | done / done |
| 55 | Interactive | Button and list taps received | Yes | Yes | `webhooks/reference/messages/interactive.md`, `webhooks/reference/messages/button.md` | `webhooks::fields::InteractiveReply::ButtonReply`, `ListReply`; `webhooks::fields::MessageContent::Button` | in `message_received` events | done / done |
| 56 | Interactive | Location request | Yes (location button) | No | `messages/location-request-messages` | `client::messages::Interactive::LocationRequest` | not in the send union | done / gap (M5a) |
| 57 | Interactive | Address request | Yes (address button) | Yes (`sendAddressRequest`) | `messages/address-messages` | `client::messages::Interactive::Address` | not in the send union | done / gap (M5a) |
| 58 | Interactive | Reminder buttons, bottom sheets, countdown offers | Yes | No | the countdown offer is the limited-time-offer template (`templates/marketing-templates/limited-time-offer-templates.md`: an expiration timer on the offer code), row 99; no reminder buttons or bottom sheets | the countdown: `TemplateMessage::limited_time_offer` (row 99) | the template passes through | n/a — unofficial protocol (reminder buttons, bottom sheets; the countdown offer is row 99) |
| 59 | Interactive | Call permission request | — | — | `calling/user-call-permissions`; `reference/whatsapp-business-phone-number/message-api.md` (`call_permission_request`) | `client::messages::CallPermissionRequest`; replies as `InteractiveReply::CallPermissionReply` | not in the send union; replies arrive in `message_received` events | done / partial (M5a) |
| 60 | Flows | Send a Flow, receive its answer | No | Yes (`flows.send`, a `flow-response` event) | `flows/guides/sendingaflow`, `flows/guides/flowswebhooks` | `OutboundMessage::flow`; `client::messages::FlowMessage`; answers as `InteractiveReply::NfmReply`; `webhooks::WebhookEvent::FlowUpdated` | answers in `message_received` events, `flow_updated` events; sending a Flow is not in the send union | done / partial (M5a) |
| 61 | Flows | Manage Flows (create, upload the JSON, preview, publish, deprecate, delete, assets) | No | Partial (list only) | `flows/guides/flowsapi` | `client::flows::Flows::create`, `list`; `client::flows::Flow::update`, `upload_flow_json`, `publish`, `deprecate`, `delete` | not exposed | done / gap (M5d) |
| 62 | Flows | The data endpoint: decrypt requests, seal responses, check signatures, decrypt uploaded media; the business public key | — | — | `flows/guides/implementingyourflowendpoint`, `flows/guides/whatsapp-business-encryption`; `reference/whatsapp-business-phone-number/business-encryption-api.md` | `client::flows::endpoint::FlowEndpointKey::decrypt_request`; `client::flows::endpoint::ResponseSealer`; `client::flows::BusinessEncryption::set_public_key` | not exposed | done / gap (M5d) |
| 63 | Flows | A typed Flow JSON builder | — | — | `flows/guides/flowjson`, `flows/guides/components` | Flow JSON stays raw JSON (`Flow::upload_flow_json` takes the bytes) | n/a (callers send Flow JSON as JSON) | partial / n/a |

### E. Profile, chats, contacts, calls

| # | Area | Capability | Zaileys (Web) | Zaileys (Cloud) | Meta Cloud API | Library | Service | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 64 | Profile | Own profile (on the Cloud API, the business profile: about, address, description, email, websites, vertical, picture) | Yes (name, about, picture) | Yes (business profile) | `business-profiles`; `reference/whatsapp-business-phone-number/whatsapp-business-profile-api.md`, `reference/whatsapp-business-profile/whatsapp-business-profile-node-api.md` | `client::business_profile::BusinessProfile::get`, `update`; the picture by upload handle (`ProfileUpdate::profile_picture_handle`) | `GET` and `PATCH /v1/numbers/{pn}/profile` (`server::api::numbers::get_profile`, `update_profile`), without the picture | done / partial (M5c2) |
| 65 | Profile | Display name change | — | — | `display-names` | `client::phone_numbers::PhoneNumber::request_display_name_change` | not exposed | done / gap (M5c2) |
| 66 | Profile | Business username: adopt or change it, read it, reserved names, delete it; changes by webhook | — | — | `business-scoped-user-ids` (§ Business usernames: `POST`, `GET` and `DELETE /{Phone-Number-ID}/username`, `username_suggestions`); `reference/whatsapp-business-phone-number/whatsapp-business-account-phone-number-api.md` | the webhook only: `webhooks::WebhookEvent::BusinessUsernameUpdated`; none of the calls | `business_username_updated` events; no setter | partial / partial (M5c3) |
| 67 | Chats | Archive, pin, mute, star, clear chats | Yes | No | none | the inbox tracks unread only (`inbox::Inbox::mark_read`) | — | n/a — unofficial protocol |
| 68 | Contacts | Check that a number is on WhatsApp | Yes | No | none: the `contacts` check belonged to the retired On-Premises API (Meta's `changelog`) | — | — | n/a — unofficial protocol |
| 69 | Contacts | Save, rename, look up contacts | Yes | No | the coexistence contacts sync (`webhooks/reference/smb_app_state_sync.md`) | typed `webhooks::WebhookEvent::AppStateSynced`; not stored (coverage row 21; L5, L8) | `app_state_synced` events; not stored (M3b) | partial / partial (M3b) |
| 70 | Contacts | Privacy settings | Yes | No | none for an account's privacy; the business number's search visibility is row 129 | — | — | n/a — unofficial protocol |
| 71 | Contacts | Block, unblock, list blocked users | Yes | Yes | `block-users`; `reference/whatsapp-business-phone-number/block-api.md` | `client::block_users::BlockUsers::block`, `unblock`, `list` (phone number or BSUID) | not exposed | done / gap (M5g) |
| 72 | Contacts | Presence (online, typing, recording) | Yes | No | typing only (row 32) | — | — | n/a — unofficial protocol |
| 73 | Contacts | LID and username lookups | Yes | No (null) | no lookup endpoint: identities arrive on webhooks, changes as `user_id_update` (`business-scoped-user-ids`) | the BSUID and username on every event; `webhooks::WebhookEvent::UserIdChanged` | `user_id_changed` events | done / done |
| 74 | Calls | Incoming calls (accept, reject, terminate) | Yes | No | `calling/user-initiated-calls`; `reference/whatsapp-business-phone-number/calling-api.md` | `webhooks::WebhookEvent::CallUpdated`; `client::calling::Calling::pre_accept`, `accept`, `reject`, `terminate` | `call_updated` and `call_status_updated` events; the actions are not exposed | done / partial (M5e) |
| 75 | Calls | Rejecting calls automatically, call restrictions | Yes (auto-reject with an allow list) | No | `calling/call-settings` | `client::calling::CallingSettings` (call hours, callback permission, voicemail, restrictions) | not exposed | done / gap (M5e) |
| 151 | Contacts | Meta's contact book (a user's phone number kept with their BSUID across the portfolio): delete an entry | — | — | `business-scoped-user-ids` (§ Contact book: `DELETE /{Phone-Number-ID}/contact_book`) | not wrapped | — | gap / gap (M5c3) |

### F. Groups and channels

| # | Area | Capability | Zaileys (Web) | Zaileys (Cloud) | Meta Cloud API | Library | Service | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 76 | Groups | Create, read, list, update, set the picture, delete | Yes | No | `groups`; `reference/whatsapp-business-phone-number/groups-management-api.md`, `reference/groups/groups-query-api.md` | `client::groups::Groups::create`, `list`; `client::groups::Group::info`, `update`, `set_picture`, `delete` | not exposed (sends to a group work, row 35) | done / gap (M5f) |
| 77 | Groups | Add and remove members | Yes | No | `reference/groups/groups-participants-api.md` | `Group::add_participants`, `remove_participants` (adding kept with a warning, OPEN_QUESTIONS #23) | not exposed | done / gap (M5f) |
| 78 | Groups | Promote and demote admins, leave a group | Yes | No | no promote, demote or leave call in the groups reference or guides (`groups/reference`); a participant leaving arrives as a webhook | — | — | n/a — unofficial protocol |
| 79 | Groups | Invite links, join requests | Yes | No | `reference/groups/groups-invite-link-api.md`, `reference/groups/groups-join-requests-api.md` | `Group::invite_link`, `reset_invite_link`, `delete_invite_link`; `join_requests`, `approve_join_requests`, `reject_join_requests` | not exposed | done / gap (M5f) |
| 80 | Groups | Group events (joins, leaves, settings, lifecycle) | Yes | No | `groups/webhooks`; fields `group_lifecycle_update`, `group_participants_update`, `group_settings_update`, `group_status_update` | `webhooks::WebhookEvent::GroupUpdated` | `group_updated` events | done / done |
| 81 | Groups | Group analytics | — | — | `analytics` | `client::analytics::Analytics::groups` | not exposed | done / gap (M5f) |
| 82 | Groups | Communities | Yes | No | none anywhere in the mirror | — | — | n/a — unofficial protocol |
| 83 | Groups | Channels (newsletters) | Yes | No | none anywhere in the mirror | — | — | n/a — unofficial protocol |

### G. Bots and automation (framework features)

| # | Area | Capability | Zaileys (Web) | Zaileys (Cloud) | Meta Cloud API | Library | Service | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 84 | Bots | Typed message events | Yes | Partial (text, media, location, contacts, reactions, button and list taps) | `webhooks/reference/messages.md` and its pages, one per type | `webhooks::WebhookEvent` (34 variants, `Unknown` and `Unparsed` included); `webhooks::fields::MessageContent`; in a bot, listeners by message type or event kind: `Listen::MessageType`, `Listen::Event` (a kind not in `WebhookEvent::KINDS` fails the build) | every reviewed type on `GET /v1/events` (`server_core::events::TENANT_EVENT_TYPES`, 28 types); the standby, handover and click types in M2d (design D25) | done / partial (M2d) |
| 85 | Bots | Commands: prefixes, arguments and flags, aliases, subcommands, guards (group, private, admin), cooldowns, generated help, error events | Yes | No (parse by hand) | Meta shows a command menu, but dispatch is ours: `business-phone-numbers/conversational-components`; `reference/whatsapp-business-account/conversational-automation-api.md` | in `meta-whatsapp-bot`: prefixes (`PrefixParser`, `BotBuilder::prefixes`; names case-insensitive by default), aliases (`Command::alias`), arguments split on whitespace with quoted strings kept whole (`Args::parse`), image and video captions (`BotBuilder::commands_from_captions`), reply buttons, list rows and quick-reply buttons by payload (`Command::payload`), usage hints and metadata (`Command::usage`, `Command::metadata`), guards (`Command::private_only`, `Command::group_only`, `Command::owner_only`: Meta's group info lists participants by `wa_id` alone, with no role, so the bot's owners stand in for Zaileys' group admins), per-user cooldowns (`Command::cooldown`, `KvCooldowns` on `KvStore`), a generated help (`BotBuilder::help_command`, `CategoryHelp`), an unknown-command hook (`BotBuilder::unknown_command`), error events (`ErrorHandler`, `BotBuilder::errors`), and Meta's command menu (`Bot::sync_command_menu`, over `client::phone_numbers::PhoneNumber::configure_conversational_automation`). Not yet: subcommands and `--flag` arguments (B1b) | gap (the bot API, M5k) | partial / gap (M5k) |
| 86 | Bots | Middleware: ordered, `next()`, short-circuit, wraps the handler | Yes | No | none (ours to build) | `Middleware` in `meta-whatsapp-bot`, run in registration order (`BotBuilder::middleware`, `Registrar::middleware`) with `Next::run`: one that does not call it stops the event, and what follows the call runs after the handler (`Logging` times it); built in: `Logging`, `MarkRead`. Unlike Zaileys', it runs for every event, after the ban and the command match and before the command's guards | gap (M5k) | done / gap (M5k) |
| 87 | Bots | Plugins: setup and unload hooks; loaded from a folder with hot reload | Yes | No | none | compile-time plugins in `meta-whatsapp-bot`: `Plugin::setup` registers commands, middleware and listeners through a `Registrar`, `Plugin::on_unload` runs at `Bot::unload`, `Plugin::category` names the help section; added with `BotBuilder::plugin`. Loading from a folder and hot reload are not offered, by design (D28): dynamic loading of Rust code is neither idiomatic nor safe, so a plugin is a crate | gap (M5k) | done / gap (M5k) |
| 88 | Bots | Sender allow and deny lists (owners, banned users) | Yes | Yes | none (ours; row 71 is Meta's block list) | in `meta-whatsapp-bot`: an `AccessPolicy` (the default `AccessList`: owners and bans by BSUID, `AccessList::owner`, `AccessList::ban`, or by phone number, `AccessList::owner_phone`, `AccessList::ban_phone`), set with `BotBuilder::access`; a banned sender's message stops before the command match and the middleware (`Refusal::Banned`), and owners pass `Command::owner_only` | gap (M5k) | done / gap (M5k) |
| 89 | Bots | Broadcast with pacing (progress, retries) | Yes (5 a second by default) | No (throws) | limits to respect: `throughput.md` (80 messages a second per number by default), the pair limit (131056, `support/error-codes.md`), `templates/marketing-templates/per-user-limits.md`, `messaging-limits` | gap: `RetryPolicy` handles a throttle, nothing paces a batch (`meta-whatsapp-bot`, B2; design D29) | gap (M5k) | gap / gap (M5k) |
| 90 | Bots | Scheduled messages: send at a time, cancel, survive restarts, retry | Yes | No (fails when due) | Meta's WABA campaign schedules, reference only (`reference/whatsapp-business-account/schedules-api.md`; its `audience_id` is explained nowhere in the mirror) | gap: durable jobs, a typed store on `KvStore` (`meta-whatsapp-bot`, B3; design D29); Meta's schedules API not wrapped (L10b) | gap (M5k) | gap / gap (M5k) |
| 91 | Bots | Auto-delete stored messages (by age, a cap per chat) | Yes | No ("not yet" on the Cloud API) | none (local data) | gap: `core::store::ConversationStore` has no retention or erasure (L5 and design D10; `meta-whatsapp-bot`, B4) | gap: the outbox purges after 7 days (`server_core::events::DEFAULT_OUTBOX_RETENTION`); the inbox keeps everything (retention per store: M2a; the bot's auto-delete over HTTP: M5k) | gap / gap (M2a, M5k) |
| 92 | Bots | Throttling our own typing indicators and group operations | Yes | Yes, per the summary of its configuration page (doubtful for group operations, which Zaileys does not offer on the Cloud API; unverified) | none (ours) | gap (`meta-whatsapp-bot`, B2, with pacing) | gap (M5k) | gap / gap (M5k) |

### H. Templates, commerce, numbers

| # | Area | Capability | Zaileys (Web) | Zaileys (Cloud) | Meta Cloud API | Library | Service | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 93 | Templates | Send approved templates (named or positional parameters, media headers, buttons) | No | Yes | `templates/overview.md`, `templates/components.md`; `reference/whatsapp-business-phone-number/message-api.md` | `client::templates::TemplateMessage`; `OutboundMessage::template` | type `template` | done / done |
| 94 | Templates | Create, list, get, delete | No | Yes | `templates/template-management.md`; `reference/whatsapp-business-account/message-template-api.md` | `client::templates::Templates::create`, `list`, `get`, `delete_by_name`, `delete_by_id`, `delete_by_ids` | `/v1/wabas/{waba_id}/templates`: list, get, create, delete (`server::api::templates`) | done / done |
| 95 | Templates | Edit a template | No | No | `reference/whatsapp-business-account/message-template-api.md` (`POST /{TEMPLATE_ID}`) | `Templates::edit` | not exposed | done / gap (M5b) |
| 96 | Templates | Template library, migration, comparison, unpausing | — | — | `templates/template-library.md`, `templates/template-migration.md`, `templates/template-comparison.md`, `templates/template-pausing.md` | `Templates::library`, `create_from_library`, `migrate_from`, `compare`, `unpause` | not exposed | done / gap (M5b) |
| 97 | Templates | Review, quality, category and component events | No | Yes (status only) | `webhooks/reference/message_template_status_update.md`, `webhooks/reference/message_template_quality_update.md`, `webhooks/reference/template_category_update.md`, `webhooks/reference/message_template_components_update.md` | `webhooks::WebhookEvent::TemplateStatusUpdated`, `TemplateQualityUpdated`, `TemplateCategoryUpdated`, `TemplateComponentsUpdated`, `TemplateCategoryMisuseDetected` | the five `template_*` event types | done / done |
| 98 | Templates | Authentication templates (copy code, one-tap, zero-tap, previews, bulk upsert) and an OTP service | No | Partial (copy code sent by hand) | `templates/authentication-templates/authentication-templates.md` and its pages | `client::authentication::Authentication::create`, `previews`, `upsert`; `client::authentication::OtpService::issue`, `verify` | M3c (`/v1/otp/*`, `POST /v1/wabas/{waba_id}/templates/authentication`) | done / gap (M3c) |
| 99 | Templates | Marketing and utility template kinds: coupon, limited-time offer, media-card and product-card carousels, location, call permission, catalog, MPM, SPM, product header | — | — | `templates/marketing-templates.md` and its pages, `templates/utility-templates/utility-templates.md` and its pages, `catalogs/product-card-carousel-template-messages` | `client::templates::TemplateComponent::limited_time_offer`, `carousel`, `header_location`, `header_product`, `call_permission_request`; `client::templates::Button::catalog`, `mpm`, `spm`; `client::templates::Parameter::coupon_code` | create and send take Meta's JSON (unverified per kind: no service test creates each one, M5b); what the library cannot carry is refused (coverage row 33) and counted in its own row: payment buttons (rows 38, 141), `app_deep_link` (row 155), `optimization_spec` (row 137); the pre-v21 one-tap fields are superseded on the Graph API version we target | done / partial (M5b) |
| 100 | Templates | Time to live, the tap-target title override | — | — | `templates/time-to-live.md`, `templates/tap-target-url-title-override.md` | `client::templates::TemplateDefinition::message_send_ttl_seconds`; `TemplateMessage::tap_target` | through Meta's JSON (unverified: no test covers these keys through the service) | done / partial (M5b) |
| 101 | Templates | Archiving and unarchiving templates | — | — | `templates/template-management.md` (§ Archive and unarchive templates: the API archives and unarchives templates in bulk, and sends to `templates/template-archival.md` for the endpoints, which the mirrored page does not show); archival and unarchival arrive as `ARCHIVED` and `UNARCHIVED` in `webhooks/reference/message_template_status_update.md` | not wrapped (the endpoint is not in the mirror); the status events are typed (`webhooks::WebhookEvent::TemplateStatusUpdated`) | the status events only (`template_status_updated`) | gap / gap (M5b) |
| 102 | Commerce | Commerce settings (cart, catalog visibility) | — | — | `catalogs/set-commerce-settings`; `reference/whatsapp-business-phone-number/commerce-settings-api.md` | `client::commerce::Commerce::settings`, `update_settings`, `set_cart_enabled`, `set_catalog_visible` | not exposed | done / gap (M5g) |
| 103 | Commerce | List connected catalogs and their products | Yes | Yes (`commerce.catalogs()`, `commerce.products()`) | `catalogs/upload-inventory` sends integrators to Meta's Catalog API, outside the WhatsApp docs | gap: no catalog or product reads (inventory lives outside the WhatsApp API, coverage row 5) | gap | gap / gap (M5g) |
| 104 | Commerce | Product messages (single, multi, catalog, product carousel) | Yes | Yes | `catalogs/single-product-messages`, `catalogs/multi-product-messages`, `catalogs/catalog-messages` | `OutboundMessage::product`, `product_list`, `catalog` | not in the send union | done / gap (M5a) |
| 105 | Commerce | Order events | No | Yes | `webhooks/reference/messages/order.md` | `webhooks::fields::MessageContent::Order` | in `message_received` events | done / done |
| 106 | Marketing | Click-to-chat QR codes and short links | No | Yes (create, list, delete) | `qr-codes.md`; `reference/whatsapp-business-phone-number/whatsapp-business-qr-code-api.md`, `reference/whatsapp-business-phone-number/whatsapp-business-qr-code-management-api.md` | `client::qr_codes::QrCodes::create`, `update`, `get`, `list`, `delete` | not exposed | done / gap (M5g) |
| 107 | Analytics | Messaging, conversation, pricing, template, call and group analytics | No | Yes (conversations, messages) | `analytics` | `client::analytics::Analytics::messaging`, `conversation`, `pricing`, `calls`, `template`, `template_group`, `groups`; `enable_template_insights` | not exposed | done / gap (M5g) |
| 108 | Numbers | Registration: request and verify a code, register, deregister, list numbers | No | Yes | `reference/whatsapp-business-phone-number/phone-number-registration.md`, `reference/whatsapp-business-phone-number/phone-number-verification-request-code-api.md`, `reference/whatsapp-business-phone-number/verify-code-api.md`, `reference/whatsapp-business-phone-number/phone-number-deregister-api.md`, `reference/whatsapp-business-account/phone-number-management-api.md` | `client::phone_numbers::PhoneNumber::request_code`, `verify_code`, `register`, `deregister`; `client::waba::Waba::phone_numbers` | `GET /v1/numbers`, `GET /v1/numbers/{pn}` (`server::api::numbers::list_numbers`, `get_number`); registration in M3 | done / partial (M3d) |
| 109 | Numbers | Number health: quality rating, throughput, messaging limit tier | No | Yes (`info()`) | `reference/whatsapp-business-phone-number/whatsapp-business-account-phone-number-api.md`; `throughput.md` | `client::phone_numbers::PhoneNumberInfo` (`quality_rating`, `throughput`, `messaging_limit_tier`); `webhooks::WebhookEvent::PhoneNumberQualityUpdated` | `GET /v1/numbers/{pn}` reports the quality rating and throughput, not the messaging limit tier (`server::api::numbers::NUMBER_FIELDS`); `phone_number_quality_updated` events (limit changes included) | done / partial (M5c3) |
| 153 | Templates | Template groups: create, read, update, delete | — | — | Meta's `changelog` lists `GET` and `POST /{WABA_ID}/template_groups`, `GET`, `POST` and `DELETE /{TEMPLATE_GROUP_ID}`; the guide it links, `templates/template-groups`, is not in Meta's page list, so not in the mirror | their analytics only (`Analytics::template_group`); management not wrapped | not exposed | gap / gap (M5b) |
| 155 | Templates | Android app deep links on marketing templates' URL buttons | — | — | `marketing-messages/deep-links.md` (a URL button's `app_deep_link`: the Android deep link, a fallback URL, the Meta app id) | not carried: `client::templates::Button::Url` has no `app_deep_link` | refused when a template is created, `422` on the key (coverage row 33) | gap / gap (M5b) |

### I. Data and production

| # | Area | Capability | Zaileys (Web) | Zaileys (Cloud) | Meta Cloud API | Library | Service | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 110 | Storage | Auth store | Yes (plain JSON in files, SQLite, Postgres, Redis, Convex) | No (a token only) | none (ours) | `client::embedded_signup::TokenVault` (AES-256-GCM, key rotation) on `core::store::KvStore`: `adapters::store::MemoryKvStore`, `PostgresKvStore`, `RedisKvStore` | Postgres and the vault; `POST /v1/admin/vault/rotate` (`server::api::admin::rotate_vault`) | done / done |
| 111 | Storage | Message store: backends, history, chat list, a single-message lookup | Yes (memory, SQLite, Postgres, Redis, Convex) | Yes | none (ours) | `core::store::ConversationStore`: `adapters::store::MemoryConversationStore`, `PostgresConversationStore`; no Redis or SQLite store, no lookup by message id (L5, L8); `inbox::Inbox::conversations`, `history` | the service records its tenants' inbox since M1c (`server::events::ServiceSink`); its read routes come in M2a | partial / partial (M2a) |
| 112 | Storage | Pluggable stores with conformance tests | Yes (custom interfaces) | Yes | none (ours) | ports in `meta-whatsapp-core`; executable suites `adapters::store::conformance`, `conversation_conformance` | the core's ports (`server_core::store::RecordStore`, `IdempotencyRecords`, `LeaderLock`, `Janitor`, `SchemaMigrator`; `server_core::outbox::Outbox`), bundled per database as `server_core::backend::Backend` with the library's `KvStore` and `ConversationStore`, implemented in memory and on Postgres (`server::store::MemoryBackend`, `PgBackend`); their suites are still in the service's tests, not in core (S3), and the service is not yet composed from a bundle alone, so an integrator's backend needs an edit to `serve.rs` (S4) (design D26) | done / partial (S3, S4) |
| 113 | Errors | Typed errors with retry guidance | Yes | Yes | `support/error-codes.md` | `core::Error`, `Error::in_step`; `core::ErrorKind`, `ErrorKind::ALL`, `is_retryable` | the §5 error model as data on `ErrorKind::as_str` (`server_core::error`, over HTTP `server::error`) | done / done |
| 114 | Ops | Logging without secrets or personal data; metrics | Partial (a custom logger) | Partial | none (ours) | `tracing`; secrets kept out of `Debug`; webhook log redaction (`webhooks::redact`) | request logs with the key id, Prometheus `/metrics` (`server::api::ops::metrics`) | done / done |
| 115 | Ops | Runtimes and packaging | Yes (Node, Bun, Deno, Termux) | Yes | none | Rust with tokio; the crates are not published yet (publishing to crates.io is the owner's: roadmap § Owner touchpoints) | a binary today; the Docker image and the TypeScript client in M4 | n/a / gap (M4) |
| 116 | Ops | Server sizing guidance | Yes (for WhatsApp Web) | — | `support/load-testing.md` | — | — | n/a — unofficial protocol (sizing for a WhatsApp Web session) |
| 117 | Tooling | Agent skills, LLM-readable docs, a docs MCP server, a project doctor | Yes | Yes | none | consumer skills under `skills/` (`npx skills add vaam-apps/meta-whatsapp-rs`); no `llms.txt`, MCP server or doctor (L22a, L22b) | the `meta-whatsapp-rs-server*` skills; no `doctor` (L22a) | partial / partial (L22a) |
| 118 | Documents | Invoices, receipts, vouchers rendered to PDF or PNG | — | — | none (ours) | `typst::Renderer`; `typst::Template::invoice`, `receipt`, `voucher` | `POST /v1/numbers/{pn}/documents` in M4 | done / gap (M4) |
| 119 | Inbox | CMS inbox: store events, check the 24-hour window before a reply | — | — | `pricing.md` (the customer service window) | `inbox::InboxSink`; `inbox::Inbox::reply`; `core::store::CustomerServiceWindow` | recording since M1c; the inbox routes and the window check over HTTP in M2a | done / partial (M2a) |

### J. Cloud API capabilities Zaileys does not offer

| # | Area | Capability | Zaileys (Web) | Zaileys (Cloud) | Meta Cloud API | Library | Service | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 120 | Onboarding | Embedded Signup v4: launch options, session events, code exchange, token checks, the encrypted vault, an approval gate, resume | — | — | `embedded-signup/overview`, `embedded-signup/implementation`, `access-tokens` | `client::embedded_signup::EmbeddedSignup`, `onboard_with_approval`, `resume`; `client::embedded_signup::LaunchOptions`; `client::embedded_signup::SignupSessions` | M3 (`/v1/signup/*`) | done / gap (M3a) |
| 121 | Onboarding | Hosted Embedded Signup, app-only install | — | — | `embedded-signup/hosted-es`, `embedded-signup/app-only-install` | the `app_only_install` launch feature (`client::embedded_signup::FeatureName::AppOnlyInstall`, refused with coexistence); hosted Embedded Signup not integrated: it starts from a `PARTNER_ADDED` webhook and gets its business token from `system_user_access_tokens`, not from a code (OPEN_QUESTIONS #11, decided: integrate, L11a) | — | partial / gap (M3f) |
| 122 | Onboarding | Several WABAs in one signup | — | — | `embedded-signup/overview` | only the claimed WABA is onboarded (OPEN_QUESTIONS #5, decided: opt-in onboarding of every granted WABA, L11b) | inherits it (M3f) | partial / gap (M3f) |
| 123 | Onboarding | Pre-verified numbers: pools, adding, codes, sharing, partners | — | — | `embedded-signup/pre-verified-numbers`; `reference/business/whatsapp-business-pre-verified-phone-numbers-api.md`, `reference/business/whatsapp-business-pre-verified-phone-number-sharing-api.md`, `reference/business/add-phone-numbers-api.md`, `reference/whatsapp-business-pre-verified-phone-number/whatsapp-business-pre-verified-phone-number-api.md` | only the launch option (`LaunchOptions::pre_verified_phone_ids`) and `client::waba::NewPhoneNumber::preverified_id`; the endpoints are not wrapped (L11c) | — | partial / gap (M3f) |
| 124 | Onboarding | Coexistence (WhatsApp Business app users): onboarding, contacts and history sync, echoes | — | — | `embedded-signup/onboarding-business-app-users`; `webhooks/reference/history.md`, `webhooks/reference/smb_message_echoes.md`, `webhooks/reference/smb_app_state_sync.md` | `LaunchOptions::coexistence`; `client::phone_numbers::PhoneNumber::sync_smb_app_data`; `webhooks::WebhookEvent::HistorySynced`, `MessageEchoed`; recorded by the inbox (coverage row 21) | echoes and history recorded and delivered as events since M1c; onboarding and the automatic sync (D7) in M3b | done / partial (M3b) |
| 125 | Onboarding | In-App Signup (opt-in deep links, promo codes) | — | — | `in-app-signup` | `client::signups::Signups::create`, `list`; `client::signups::deep_link` | not exposed; accepting Meta's terms on a business's first signup comes in M5i as an explicit, audited operator action, off by default (OPEN_QUESTIONS #26, decided: the deployer's act) | done / gap (M5i) |
| 126 | WABA | Details, subscribed apps and the callback override, assigned users, client and owned WABAs, customer bases | — | Partial (lists numbers) | `whatsapp-business-accounts.md`, `webhooks/override.md`; `reference/whatsapp-business-account/whatsapp-business-account-api.md`, `reference/whatsapp-business-account/subscribed-apps-api.md`, `reference/whatsapp-business-account/assigned-users-management-api.md`, `reference/business/client-whatsapp-business-accounts-api.md`, `reference/business/owned-whatsapp-business-accounts.md` | `client::waba::Waba`; `client::waba::Business` | `GET /v1/wabas`, disconnect (`server::api::numbers::list_wabas`, `disconnect_waba`); admin attach and unbind | done / partial (M5c4) |
| 127 | WABA | Create a WABA, its activities, system users and their tokens for client businesses, a WABA's solutions | — | — | `reference/business/whatsapp-business-accounts-api.md` (`POST`), `solution-providers/partner-initiated-waba-creation.md`, `reference/whatsapp-business-account/whatsapp-business-account-activities-api.md`, `reference/whatsapp-business-account/whatsapp-business-account-solutions-list-api.md`, `solution-providers/manage-system-users.md`, `system_user_access_tokens` (`marketing-messages/onboard-business-customers.md`, `embedded-signup/hosted-es`) | not wrapped | — | gap / gap (M5c4) |
| 128 | Numbers | Two-step PIN, data localization, identity-key check, per-number webhook override | — | — | `business-phone-numbers/two-step-verification`, `no-storage.md`, `local-storage`, `identity-change`; `reference/whatsapp-business-phone-number/settings-api.md` | `PhoneNumber::set_two_step_pin`, `enable_local_storage`, `set_identity_key_check`, `set_webhook_override` | the PIN in M3d; the rest not exposed (M5c3) | done / gap (M3d, M5c3) |
| 129 | Numbers | Search visibility, security notifications, notifying users of a number change | — | — | `reference/whatsapp-business-phone-number/whatsapp-business-account-phone-number-api.md` (`search_visibility`, `show_security_notifications`, `notify_user_change_number`) | not wrapped | — | gap / gap (M5c3) |
| 130 | Numbers | Official Business Account: request and status | — | — | `official-business-accounts.md`; `reference/whatsapp-business-phone-number/whatsapp-business-account-official-business-account-status-api.md` | only the flag `PhoneNumberInfo::is_official_business_account` | — | partial / gap (M5c3) |
| 131 | Numbers | Business compliance information (India) | — | — | `reference/whatsapp-business-phone-number/business-compliance-information-api.md` | not wrapped | — | gap / gap (M5c3) |
| 132 | Numbers | Health status of a number, a WABA or a business | — | — | `support/health-status.md` | typed on templates only (`client::templates::TemplateInfo`'s `health_status`) | — | partial / gap (M5c3) |
| 133 | Bots | Conversational components (welcome message, ice breakers, commands); bot details | — | — | `business-phone-numbers/conversational-components`; `reference/whatsapp-business-account/conversational-automation-api.md`, `reference/whatsapp-business-bot/bot-details-api.md` | `PhoneNumber::conversational_automation`, `configure_conversational_automation`; `GET /{WABA-Bot-ID}` not wrapped | not exposed | partial / gap (M5c3) |
| 134 | Calling | Calling settings (hours, SIP, voicemail, icons), permissions, business-initiated calls | — | — | `calling`, `calling/call-settings`, `calling/business-initiated-calls`; `reference/whatsapp-business-phone-number/calling-api.md` | `client::calling::Calling::settings`, `update_settings`, `permissions`, `connect`; `client::calling::CallingSettings` (signalling only: WebRTC media is the integrator's stack) | not exposed | done / gap (M5e) |
| 135 | Calling | Call recording and transcription | — | — | `calling/call-recording`, `calling/call-transcription` | the webhooks are typed (`call_recording_available`, `call_transcription_available` in `webhooks::fields`) and the files download through `Media::download` by the media id they carry; the per-call `recording` and `transcription` objects on connect and accept are not carried by `client::calling::ConnectCall` or `Calling::accept` | the call events only | partial / gap (M5e) |
| 136 | Marketing | Marketing Messages API: send (product policy, activity sharing, bid multiplier), onboarding, the Cloud API marketing switch, partner onboarding to MM Lite | — | — | `marketing-messages/overview.md`, `marketing-messages/send-marketing-messages.md`, `marketing-messages/onboarding.md`; `reference/whatsapp-business-phone-number/marketing-messages-api-for-whatsapp.md`, `reference/business/whatsapp-business-partner-onboarding-to-mm-lite-api.md` | `client::marketing::Marketing::send`; `client::marketing::MarketingAccount::onboarding_status`, `set_cloud_api_marketing_disabled`; `client::marketing::MarketingBusiness::request_onboarding` | not exposed | done / gap (M5h) |
| 137 | Marketing | Max price (agreement, partner allow list, duplicating a template at another price), reach estimates, click tracking (metrics and conversion measurement are Meta's ads APIs: not part of this row) | — | — | `marketing-messages/pricing/enroll-max-price.md`, `marketing-messages/pricing/duplicate-templates.md`, `marketing-messages/pricing.md` (`GET /{WABA_ID}/reachestimate`), `marketing-messages/view-metrics.md`, `marketing-messages/measure-conversion.md`, `marketing-messages/track-click-events.md` | max price and reach estimates not wrapped (L13). Click tracking opt-out: `client::analytics::Analytics::set_button_click_tracking`; clicks as `webhooks::WebhookEvent::UserActionReported`. Enrolling in max price signs Meta's beta agreement, a legal act: wrapped as an explicit call, and signing it is the deployer's (OPEN_QUESTIONS #45); the metrics and conversion measurement come from Meta's ads APIs (insights on ad objects, events for ads), outside the Cloud API | clicks are operator-only until M2d (D25); max price in M5h, its agreement an explicit, audited operator action | partial / gap (M2d, M5h) |
| 138 | Marketing | Click-to-WhatsApp: the referral on inbound messages, automatic events, welcome message sequences | — | — | `ctwa/welcome-message-sequences`; `embedded-signup/automatic-events-api` | `webhooks::fields::Referral`; `webhooks::WebhookEvent::AutomaticEventDetected`; the sequences (`/{WABA_ID}/welcome_message_sequences`) not wrapped (coverage row 31); the sequences come with L13; reporting automatic events to Meta's Conversions API (optional) is outside the Cloud API, and not part of this row | the referral in `message_received` events, `automatic_event_detected` events | partial / partial (M5h) |
| 139 | Routing | Conversation routing: thread control (pass, take, release), standby, conversation context | — | — | `conversation-routing/thread-control`, `conversation-routing/overview`; `webhooks/reference/messaging-handovers` (nm), `webhooks/reference/standby` (nm) | typed webhooks `webhooks::WebhookEvent::ThreadControlChanged`, `StandbyObserved`; the API is not wrapped; the inbox ignores ownership (OPEN_QUESTIONS #44, decided: window events and ownership, L7); the thread control API in L15 | the two types are operator-only until M2d (D25); the API in M5l | partial / gap (M2d, M5l) |
| 140 | Accounts | Account model evolution (Messaging Accounts, beta) | — | — | `account-model-evolution`; `reference/whatsapp-account-number/whatsapp-account-number-api.md` | not wrapped (coverage row 30) | — | gap / gap (M5c4) |
| 141 | Payments | Payments in India (UPI, payment links, order templates, onboarding) and Brazil (Pix, Boleto, payment links, one-click) | — | — | `payments/payments-in/overview.md`, `payments/payments-br/overview.md` and their pages | `webhooks::WebhookEvent::PaymentConfigurationUpdated` only; messages through `MessageContent::Raw` (in scope since 2026-09-26: P1) | `payment_configuration_updated` events only | partial / partial (P2) |
| 142 | Partners | Credit lines: share, attach, verify, revoke | — | — | `solution-providers/share-and-revoke-credit-lines.md`; `reference/whatsapp-business-account/extended-credits-api.md` | `client::credit_lines::CreditLines::list`, `share_and_attach`, `revoke`, `allocation_status` | M3a (Solution Partner mode) | done / gap (M3a) |
| 143 | Partners | Partner-led business verification | — | — | `solution-providers/partner-led-business-verification.md` | `client::business_verification::BusinessVerification::submit`, `submissions`, `status` | not exposed | done / gap (M5j) |
| 144 | Partners | Multi-Partner Solutions: create, accept or reject, deactivation, the solution token, an app's solutions and client businesses | — | — | `solution-providers/multi-partner-solutions.md`, `solution-providers/multi-partner-solution-embedded-creation.md`; `reference/whatsapp-business-solution/solution-details-api.md` and the pages beside it, `reference/application/application-solutions-api.md`, `reference/application/application-connected-client-businesses.md` | `webhooks::WebhookEvent::PartnerSolutionUpdated`; `LaunchOptions::solution_id`; the endpoints are not wrapped (coverage row 28) | `partner_solution_updated` is operator-only (no WABA to route it by) | partial / gap (M5j) |
| 145 | Partners | Moving numbers and WABAs between partners or solutions | — | — | `solution-providers/support/migrating-phone-numbers-among-solution-partners-programmatically.md` and the other migration pages; `reference/whatsapp-business-account-migration-intent/migration-intent-details-api.md` | the number migration flag `client::waba::NewPhoneNumber::migrate_phone_number` with the code and register calls; `set_solution_migration_intent` and migration-intent reads not wrapped | — | partial / gap (M5j) |
| 146 | Partners | A user's assigned WABAs | — | — | `reference/user/assigned-whatsapp-business-accounts-api.md` | not wrapped | — | gap / gap (M5j) |
| 147 | Billing | Billing currency or payment method migration | — | — | `pricing/change-billing-currency.md` (`set_payment_method_migration_intent`) | not wrapped | — | gap / gap (M5j) |
| 148 | History | Message history events (the delivery events of one history entry) | — | — | `reference/message-history/whatsapp-business-message-history-events-api.md` (where its id comes from is not in the mirror) | not wrapped | — | gap / gap (M5c4) |
| 149 | Policy | Account, policy and security events (alerts, violations, reviews, capability, name, quality, security) | — | — | `webhooks/reference/account_alerts.md`, `webhooks/reference/account_update.md`, `webhooks/reference/account_review_update.md`, `webhooks/reference/business_capability_update.md`, `webhooks/reference/phone_number_name_update.md`, `webhooks/reference/security.md`; `policy-enforcement.md` | `webhooks::WebhookEvent::AccountAlert`, `AccountReviewUpdated`, `AccountUpdated`, `AccountSettingsUpdated`, `BusinessCapabilityUpdated`, `PhoneNumberNameUpdated`, `SecurityUpdated` | all seven are tenant event types | done / done |
| 150 | Policy | Marketing opt-out (user preferences) | — | — | `webhooks/reference/user_preferences.md` | `webhooks::WebhookEvent::UserPreferenceChanged`; `core::ErrorKind::MarketingOptedOut` (131050) | `user_preference_changed` events; `409 marketing_opted_out` | done / done |
| 152 | Accounts | Parent BSUID accounts: the portfolios that share parent BSUIDs | — | — | `business-scoped-user-ids` (§ Get parent BSUID account, served from `api.facebook.com`) | not wrapped; its host is outside the client's credential host allow list (`client::GraphRequest`), so wrapping it widens that list, a change for the security review (L10a) | — | gap / gap (M5c4) |

## Plan to parity

[roadmap.md](roadmap.md) cites every partial and gap row from a PR-sized
item, each naming its crate, what it comes after and its decisive test:

- **The service's modular split** (S1–S18 and U1–U4, design D26): a framework-free
  core with ports, CrateStack as the default API and store once the
  owner accepts design D20 (a), the axum API (permanent, the swap
  target) and the sqlx store kept, MongoDB later. Row 112, and the
  ground every service milestone below builds on.
- **The bot framework**, a new library crate `meta-whatsapp-bot`:
  commands, middleware, compile-time plugins and markdown replies (B1,
  done; rows 17, 85–88), subcommands and flags (B1b; row 85), rich
  replies beyond text (B1c; row 17), then paced broadcast and durable
  scheduling (B2, B3; rows 89, 90, 92; D29), then retention and
  auto-delete after the `ConversationStore` port change (B4, after L5;
  row 91).
- **Library gap batches** (L4–L25), by crate and topic: every library
  partial or gap row not in the bot framework or payments.
- **Service milestones**: M2a–M2e (inbox, SSE, webhooks-out, D25),
  M3a–M3f (Embedded Signup, OTP, coexistence), M4 (packaging), M5a–M5l
  (routes for the modules parity requires, the bot and broadcast APIs
  included).
- **Payments** (India, Brazil; P1, P2): in scope, last.

## How to keep this true

- A pull request that changes a capability updates its row here in the
  same PR, with the symbol that now does it, and its category in
  [categories.md](categories.md) and its row in [coverage.md](coverage.md)
  when their status moves. A row says done only when the code shows it;
  when in doubt it says unverified.
- A new capability gets the next free row number, in the section it
  belongs to (numbers never move), and the counts at the top are
  recounted.
- When a page marked (nm) reaches the mirror, read it: drop the mark,
  and correct the row if the page says otherwise.
- The PR description says what happened to this table, as it does for
  the docs and skills (CONTRIBUTING.md § Docs and skills parity).
