# Outbound message types

> Verified against wa-rs 7940d15 (2026-09-24): `crates/wa-client/src/messages/*`.

All in `wa_rs::client::messages`. `recipient` is `impl Into<Recipient>`
(a `Recipient`, `UserId` or `GroupId` — not a `&str`).

## `OutboundMessage` shortcuts

| Constructor | Sends |
| --- | --- |
| `text(to, body)` / `text_with_preview(to, body)` | text (4096 chars; 1024 with a utility/authentication Direct Send category) |
| `image_id(to, id)` / `image_link(to, url)` | image (caption: `new(to, Image::new(src).caption(..))`) |
| `video_id` / `video_link` | video |
| `audio_id` / `audio_link` | audio (voice note: `Audio::new(src).voice()`) |
| `document_id(to, id, filename)` / `document_link(to, url, filename)` | document |
| `sticker_id` / `sticker_link` | sticker (WebP) |
| `location(to, lat, lon)` | location pin (`Location::new(..).name(..).address(..)`) |
| `contacts(to, cards)` | contact cards (`Contact`, up to 257) |
| `reaction(to, message_id, emoji)` | reaction (`""` removes it; cannot be a contextual reply) |
| `template(to, TemplateMessage)` | template |
| `reply_buttons(to, body, [ReplyButton; 1..=3])` | reply buttons |
| `list(to, body, button, sections)` | list (`ListSection`, `ListRow`) |
| `cta_url(to, body, display_text, url)` | CTA URL button |
| `location_request(to, body)` | ask for the user's location |
| `flow(to, body, FlowParameters)` | WhatsApp Flow |
| `product(to, catalog_id, product_retailer_id)` | single product |
| `product_list(to, header, body, catalog_id, sections)` | multi-product (`ProductSection`) |
| `catalog(to, body)` | catalog message |
| `pin(group, message_id, days)` / `unpin(group, message_id)` | group pin (groups only) |
| `new(to, content)` | anything below |

Builders on any message: `reply_to(message_id)`, `callback_data(s)`,
`category(DirectSendCategory::{Utility, Authentication, Service})`,
`ttl_seconds(n)` (Direct Send only), `direct_send_template_name(name)`.
Direct Send is a Meta beta; authentication-category Direct Send needs a phone
number recipient.

## Content types for `new`

| Type | Builder |
| --- | --- |
| `Text` | `Text::new(body).preview_url(true)` |
| `Image`, `Video`, `Document`, `Audio`, `Sticker` | `Image::new(source)` etc., where `source` is `MediaSource::id(id)` or `MediaSource::link(url)` (a `MediaId` converts directly); then `.caption(..)` (image, video, document), `.filename(..)` (document), `.voice()` (audio) |
| `ReplyButtons` | `ReplyButtons::new(body, [ReplyButton::new(id, title)]).header(Header::…).footer(..)` |
| `ListMessage` | `ListMessage::new(body, button, sections).header(text).footer(..)` |
| `CtaUrl` | `CtaUrl::new(body, display_text, url).header(Header::…).footer(..)` |
| `LocationRequest` | `LocationRequest::new(body)` |
| `FlowMessage` | `FlowMessage::new(body, FlowParameters::new(FlowRef::Id(..) \| FlowRef::Name(..), cta).token(..).navigate(screen, data).draft()/.data_exchange())` |
| `SingleProduct`, `ProductList`, `CatalogMessage`, `ProductCarousel` | catalog messages |
| `MediaCarousel` | `MediaCarousel::new(body, [MediaCard::new(CardHeader::…, CardAction::…).body(..)])` |
| `VoiceCall`, `CallPermissionRequest` | Calling API buttons |
| `AddressMessage` | address request (India only) |
| `RequestContactInfo` | ask for contact info |
| `TemplateMessage` | see `wa-rs-messaging` / `wa-rs-templates-otp` |
| `MessageContent::Raw { message_type, body }` | escape hatch for types not modelled (e.g. payments) |

`Header`: `text`, `image_id`, `image_link`, `video_id`, `video_link`,
`document_id`, `document_link`.

## `SendResponse`

`message_id()` → `Option<&MessageId>`; `contacts[]` (`input`, `wa_id`,
`user_id`, `parent_user_id`); `messages[]` (`id`, `group_id`,
`message_status`: `Accepted`, `HeldForQualityAssessment`, `Paused`,
`Unknown`). The MM API returns the same types.
