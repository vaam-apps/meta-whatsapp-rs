---
name: wa-rs-media
description: "WhatsApp media with wa-rs - uploading files (supported MIME types and size limits checked first), sending the media id, downloading media a customer sent with SHA-256 verification (download_bytes with a size cap, or a verified stream), media id and URL lifetimes, the host allowlist that keeps the access token on Meta's media host, and Resumable Upload handles for template header examples. Load when uploading or downloading images, documents, audio, video or stickers, handling media from webhooks, or creating a template with a media header."
---

# wa-rs-media

> **Verified against wa-rs 4eb93c9bd63812221e75ad0920b6e2cb98ea0dd6 (2026-09-24).** On another revision, trust the code over this page.

Reference code: [examples/media.rs](examples/media.rs), compiled and
tested by wa-rs's own gate.

## When to use

Uploading a file to send it, downloading what a customer sent, or getting
the header handle a template definition needs. Module
`wa_rs::client::media`, reached with `client.media(phone_number_id)`.

## Upload, then send the id

```rust
let media = client.media(phone_number_id.clone());
let id = media.upload(png, "image/png", "voucher.png").await?; // type and size checked first
let image = Image::new(id.clone()).caption("Your voucher");
client
    .messages(phone_number_id)
    .send(&OutboundMessage::new(to, image))
    .await?;
Ok(id) // lives 30 days
```

Meta's table, checked before anything is sent (`validate_upload`,
`MediaKind`): images JPEG/PNG 5 MB; documents (PDF, Office, plain text)
100 MB; audio (AAC, AMR, MP3, MP4, Ogg/Opus) and video (MP4, 3GPP) 16 MB;
WebP only as a sticker (500 KB). Anything else is `Error::Validation` on
`type`. An upload is not replayed on transient errors: a replay would
create a second object.

## Download what a customer sent

Media webhooks carry an id (`MediaContent::id`), valid 7 days:

```rust
let media = client.media(phone_number_id);
media.download_bytes(inbound_media, 5 * 1024 * 1024).await // refused over 5 MiB
```

Large files, streamed; the bytes are trustworthy only once the stream has
ended without an error:

```rust
let download = client
    .media(phone_number_id)
    .download(inbound_media)
    .await?;
let mut verified = download.verified()?; // fails fast on a malformed digest
let mut file = Vec::new(); // stand-in for a temporary file
while let Some(chunk) = verified.body.next().await {
    file.extend_from_slice(&chunk?); // a mismatch: Error::Transport(Integrity)
}
Ok(file) // only now is it the file Meta described
```

(`futures::StreamExt` for `next`.) A hash mismatch is
`TransportError::Integrity`, retryable: fetch a fresh URL and download
again. `download_bytes` refuses before downloading when Meta reports a
larger `file_size`, and stops reading past the cap in case it lied.

## Template header examples: a handle, not a media id

```rust
let media = client.media(phone_number_id);
media
    .resumable_upload(app_id, "header.png", "image/png", png)
    .await
```

The returned `UploadHandle` goes into
`TemplateComponent::header_image(handle.as_str())` when **creating** a
template (`wa-rs-templates`). At **send** time the header takes a normal
media id: `Parameter::image_id(media_id)` (`wa-rs-send-templates`). The
Resumable Upload API needs the client's token (sent as `OAuth`, never in a
URL) and the app id; accepted types are PDF, JPEG, PNG and MP4. For big
files: `start_upload_session`, `upload_chunk`, `upload_session_status`.

## Pitfalls

- **Lifetimes**: uploaded ids 30 days, webhook media ids 7 days, a media
  URL from `Media::url` 5 minutes. Keep your own copy of anything needed
  longer.
- The token is only sent to the configured Graph endpoint and to
  `https://lookaside.fbsbx.com` on the default port (where Meta's media
  URLs point). A download URL anywhere else, `*.whatsapp.net` included, is
  refused with `Error::Validation` on `url` before a byte is sent — do not
  "fix" that by fetching it yourself with the token.
- Never publish downloaded bytes before the verified stream ended: the
  last item is the hash check. Dropping the stream early skips it.
- Several merchants on one app: `media.restrict_to_phone_number()` makes
  Meta refuse media not uploaded on that number (off by default: its
  effect on webhook media is undocumented).

## What wa-rs does not do

- No storage of media: the bytes are yours to keep.
- No virus scanning, transcoding or image resizing; unsupported types are
  refused rather than converted.

## Related skills

`wa-rs-send-messages` (media messages), `wa-rs-templates` (header
handles), `wa-rs-documents` (render a PDF, then upload), `wa-rs-webhook-events`
(`MediaContent` in inbound messages), `wa-rs-cms-inbox`.
