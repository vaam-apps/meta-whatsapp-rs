//! Media (docs/design/server.md, section 4.2), as services an API adapter
//! calls, with the number's WABA token:
//!
//! | Operation | Graph calls |
//! | --- | --- |
//! | [`Media::upload`] | `POST /{pn}/media` (multipart: `messaging_product`, `type`, `file`) |
//! | [`Media::download`] | `GET /{media_id}?phone_number_id={pn}` (URL, MIME type, SHA-256, size), or `GET /{media_id}` for a media id the service recorded as received on the number (roadmap M2e), then the URL (`lookaside.fbsbx.com`, the only media host the library lets a token reach) |
//! | [`Media::delete`] | `GET /{media_id}?phone_number_id={pn}` (it must be that media), then `DELETE /{media_id}?phone_number_id={pn}` |
//!
//! Pages: `business-phone-numbers/media`, `reference/media/media-api`,
//! `reference/media/media-download-api`,
//! `reference/whatsapp-business-phone-number/media-upload-api`.
//!
//! **Uploads** are an [`Upload`]: its `type` one of Meta's supported MIME
//! types ([`checked_type`]: `422` on `type`), its file no larger than its
//! kind allows (5 MiB for images, 16 MiB for audio and video, 500 KiB for
//! stickers, 100 MiB for documents; [`upload_cap`] for an adapter reading
//! a body: `413 media_too_large`), its file name made inert
//! ([`filename`]), all checked before any request. Reading the request's
//! body (a multipart form) is the adapter's.
//!
//! **Downloads** are verified against the SHA-256 Meta reports, within
//! [`DownloadLimits`] (a size Meta reports over `max_bytes` is `413
//! media_too_large` before the download; a digest Meta did not send
//! usably is `502 integrity`, with nothing downloaded):
//!
//! - whole ([`DownloadBody::Whole`]: at most [`MAX_WHOLE_DOWNLOAD`]), read,
//!   verified, and only then answered: a mismatch is `502 integrity` and
//!   **no byte** of the file is answered;
//! - streamed ([`DownloadBody::Stream`], a [`MediaStream`]: a
//!   `futures::Stream` of the library's `Bytes`, no HTTP framework), the
//!   bytes passed on as they arrive, **one chunk behind**, hashed on the
//!   way: a mismatch (or a file growing past `max_bytes`, or a failure)
//!   ends the stream with a [`StreamStopped`] instead of its last chunk,
//!   which is never yielded, so unverified bytes never arrive as the whole
//!   file. An adapter aborts its answer on that error (the axum one: the
//!   connection), so that they never arrive as a complete body either.
//!
//! Either way the answer carries the digest ([`Downloaded::sha256`]) and
//! the content type to answer ([`Downloaded::content_type`]: Meta's, when
//! it is a plain MIME type).
//!
//! Uploads and whole downloads hold memory, streams a connection: each
//! takes a slot of the number's tenant's share of the adapter's pools
//! ([`MediaSlots`], the core's [`Slots`]), else `429 too_many_requests`. An
//! upload takes its slot ([`Media::transfer_slot`]) before its body is
//! read; a stream holds its slot until it is dropped (an adapter drops it
//! once its answer's body ended).
//!
//! **A media id is the number's, or it does not exist.** It is digits
//! (`422` on `media_id` otherwise, [`crate::request::graph_id`]), and the
//! node Meta answers for it must be media with that id: a deletion looks
//! it up first, since `DELETE /{id}` deletes whatever node an id names.
//! The lookup and the deletion carry `phone_number_id={pn}`
//! (`reference/media/media-api`: Meta then acts only on media of that
//! number), and Meta refusing it is `404 not_found`, without Meta's code
//! or text ([`crate::authz::object_failed`]): another tenant's media id is
//! never read, downloaded or deleted, and never told apart from a missing
//! one.
//!
//! **Received media** (roadmap M2e, `OPEN_QUESTIONS.md` #43). Meta
//! documents the `phone_number_id` check for media uploaded on the number;
//! a media id a customer sent (by webhook) was uploaded on no business
//! number, and Meta does not say whether the check lets it through, or
//! whether it lets another number's through. So before Meta is asked, the
//! id is looked up among the media the service recorded as received
//! ([`Outbox::received_media`]: the outbox rows of `message_received`
//! events, each routed to its tenant by the binding it arrived under):
//!
//! - recorded on **this number, routed to this tenant**, and received from
//!   the number's binding epoch on (the inbox's cut,
//!   [`crate::authz::history_from`]): a download asks Meta for it without
//!   `phone_number_id`, with the number's token, once the number's binding
//!   is read again and still holds (`503 storage_unavailable` otherwise);
//!   the URL Meta answers is fetched through the same allow-list, within
//!   the same limits, verified the same way. A deletion keeps the check
//!   (Meta's `DELETE` is documented for uploaded media);
//! - recorded **anywhere else** (another number, another tenant's row on
//!   this number, an operator-only row, a row from before the cut): `404
//!   not_found`, the answer for a missing id, and **nothing is asked of
//!   Meta**, neither for a download nor for a deletion, whatever Meta's
//!   check would have answered;
//! - **never recorded** (an upload's id, a received id the outbox purged
//!   or received before the service recorded media ids): Meta's check, as
//!   above.
//!
//! One row here is enough, whatever other rows name the id
//! (`Received::among`), and the cut is the inbox's own (received at or
//! after it). A media id is bound to the number by the event that named
//! it: the row's number and tenant are the ones the webhook was routed
//! by (its `metadata.phone_number_id`, bound under the WABA it names), and
//! only a delivery signed with the deployment's app secret is recorded,
//! so no tenant writes a row. The exemption trusts that a media id Meta
//! delivered on a number is that number's customer's file.
//!
//! **Timing** (design D78): a refusal answers without a round trip to
//! Meta, an id never recorded after one, so a caller can tell that an id
//! it already holds was received elsewhere in the deployment (within the
//! outbox's retention). Nothing of the file, its number or its tenant is
//! told, and asking Meta anyway would undo the refusal's point (nothing
//! asked); accepted, not mitigated.
//!
//! To swap (every id through Meta's check again, M1b's rule): answer
//! [`Received::Never`] from [`Media::received`].
//!
//! The services open on a capability their own [`Authorizer`] made, and
//! admit it first ([`Authorizer::admit_number`]). Failed Graph calls are
//! told to the [`Observer`] (the adapter's metrics), a `190` marks the
//! WABA's numbers `reconnect_required`, and Meta's `details` are kept. The
//! lines this module logs keep the target the service's operators filter
//! on, `meta_whatsapp_server::api::media`, where they were logged before
//! roadmap S5d. No URL is logged: Meta's carries the media's handle.

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use base64::Engine as _;
use bytes::Bytes;
use futures::{Stream, StreamExt as _};
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::media::{
    Media as LibraryMedia, MediaInfo, MediaKind, VerifiedBody, validate_upload,
};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::{MediaId, PhoneNumberId};
use meta_whatsapp_rs::core::media::{MediaConverter, MediaInput, TargetFormat};
use time::OffsetDateTime;

use crate::authz::{Authorizer, OwnedNumber, object_failed};
use crate::error::{ErrorCode, ServiceError};
use crate::model::TenantId;
use crate::observe::Observer;
use crate::outbox::{Outbox, ReceivedMedia};
use crate::ratelimit::{Slot, Slots};
use crate::request::typed_graph_id;

/// The target of this module's log lines: the service's own, where they
/// were logged before roadmap S5d.
const LOG_TARGET: &str = "meta_whatsapp_server::api::media";

/// Largest file a download answers whole (and its default `max_bytes`):
/// 16 MiB (docs/design/server.md, section 4.2). Larger ones are streamed.
pub const MAX_WHOLE_DOWNLOAD: u64 = 16 * 1024 * 1024;

/// Longest file name sent to Meta.
const MAX_FILENAME_CHARS: usize = 128;

// ─── Upload ──────────────────────────────────────────────────────────────

/// The extension of each supported MIME type, from the table of
/// `business-phone-numbers/media`, for a file sent without a name (Meta
/// reads the extension).
const EXTENSIONS: &[(&str, &str)] = &[
    ("audio/aac", "aac"),
    ("audio/amr", "amr"),
    ("audio/mpeg", "mp3"),
    ("audio/mp4", "m4a"),
    ("audio/ogg", "ogg"),
    ("text/plain", "txt"),
    ("application/vnd.ms-excel", "xls"),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsx",
    ),
    ("application/msword", "doc"),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "docx",
    ),
    ("application/vnd.ms-powerpoint", "ppt"),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "pptx",
    ),
    ("application/pdf", "pdf"),
    ("image/jpeg", "jpeg"),
    ("image/png", "png"),
    ("image/webp", "webp"),
    ("video/3gpp", "3gp"),
    ("video/mp4", "mp4"),
];

/// The file name sent to Meta: the caller's, reduced to `A-Z a-z 0-9 . _
/// -` (anything else is `_`: the name goes into a multipart header) and
/// 128 characters; `upload.<extension>` when there is none.
pub fn filename(given: Option<&str>, mime_type: &str) -> String {
    let cleaned: String = given
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(MAX_FILENAME_CHARS)
        .collect();
    if cleaned.trim_matches(['.', '_']).is_empty() {
        let essence = mime_type.split(';').next().unwrap_or_default().trim();
        let extension = EXTENSIONS
            .iter()
            .find(|(m, _)| m.eq_ignore_ascii_case(essence))
            .map_or("bin", |(_, e)| e);
        format!("upload.{extension}")
    } else {
        cleaned
    }
}

/// A `type`: one of Meta's supported MIME types, without a control
/// character (it becomes a header of the part Meta receives), trimmed.
///
/// # Errors
///
/// `422 invalid_request` on `type`.
pub fn checked_type(value: &str) -> Result<String, ServiceError> {
    let value = value.trim();
    if !value.bytes().all(|b| b == b' ' || b.is_ascii_graphic()) {
        return Err(ServiceError::invalid("type"));
    }
    MediaKind::for_mime_type(value)
        .map(|_| value.to_owned())
        .ok_or_else(|| ServiceError::invalid("type"))
}

/// The most bytes of an upload's file worth reading: its kind's limit
/// when its `type` is known (and supported) already, and never more than
/// `max_bytes` (the adapter's own cap, the service's
/// `WA_SERVER_MEDIA_MAX_BYTES`). An adapter reading a body stops there:
/// one byte more is `413 media_too_large`.
pub fn upload_cap(mime_type: Option<&str>, max_bytes: u64) -> u64 {
    mime_type
        .and_then(MediaKind::for_mime_type)
        .map_or(max_bytes, |kind| kind.max_bytes().min(max_bytes))
}

/// A checked upload: a supported `type`, a file within its kind's limit,
/// and the file name Meta will receive. Only [`Upload::new`] makes one.
pub struct Upload {
    mime_type: String,
    filename: String,
    data: Bytes,
}

impl fmt::Debug for Upload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Upload")
            .field("mime_type", &self.mime_type)
            .field("filename", &self.filename)
            .field("len", &self.data.len())
            .finish()
    }
}

impl Upload {
    /// The file `data` of MIME type `mime_type` (as [`checked_type`]
    /// reads it), named `given` by the caller (made inert by
    /// [`filename`]), checked against Meta's table of supported types and
    /// sizes (the library's `validate_upload`, which its upload repeats).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `type` (not a supported type); `413
    /// media_too_large` (larger than its kind allows).
    pub fn new(mime_type: &str, given: Option<&str>, data: Bytes) -> Result<Self, ServiceError> {
        let mime_type = checked_type(mime_type)?;
        // The library's own check, the one `upload` repeats: Meta's table.
        if let Err(refused) =
            validate_upload(&mime_type, u64::try_from(data.len()).unwrap_or(u64::MAX))
        {
            return Err(if refused.field == "type" {
                ServiceError::invalid("type")
            } else {
                ServiceError::new(ErrorCode::MEDIA_TOO_LARGE)
            });
        }
        Ok(Self {
            filename: filename(given, &mime_type),
            mime_type,
            data,
        })
    }

    /// Its MIME type.
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    /// The file name sent to Meta.
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// The file.
    pub fn data(&self) -> &Bytes {
        &self.data
    }
}

/// An uploaded file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uploaded {
    /// Its media id (kept by Meta for 30 days): what a message's `id`
    /// sends.
    pub media_id: MediaId,
}

// ─── Conversion ──────────────────────────────────────────────────────────

/// The formats an upload may be converted into, by the name a request
/// gives (`convert`): `ogg_opus` (a voice message), `static_sticker`,
/// `animated_sticker`, `jpeg` and `png`: the library's
/// [`TargetFormat`]s. The names are pending the owner's confirmation
/// before the first release.
pub const CONVERT_TARGETS: [(&str, TargetFormat); 5] = [
    ("ogg_opus", TargetFormat::OggOpus),
    ("static_sticker", TargetFormat::StaticSticker),
    ("animated_sticker", TargetFormat::AnimatedSticker),
    ("jpeg", TargetFormat::Jpeg),
    ("png", TargetFormat::Png),
];

/// The format a request named (`convert`).
///
/// # Errors
///
/// `422 invalid_request` on `convert`.
pub fn convert_target(name: &str) -> Result<TargetFormat, ServiceError> {
    CONVERT_TARGETS
        .iter()
        .find(|(n, _)| *n == name.trim())
        .map(|(_, target)| *target)
        .ok_or_else(|| ServiceError::invalid("convert"))
}

/// The name of `target` in a request, the inverse of [`convert_target`].
pub fn convert_name(target: TargetFormat) -> &'static str {
    CONVERT_TARGETS
        .iter()
        .find(|(_, t)| *t == target)
        .map_or("unknown", |(name, _)| name)
}

/// A file to convert before it is uploaded (roadmap M5c1): its bytes in
/// whatever format the deployment's converter reads (the converter
/// detects it from the bytes; the caller's `type`, if any, is not trusted
/// and not read), the format wanted, and the name the caller gave.
/// Nothing the caller sends becomes a converter option.
pub struct Conversion {
    target: TargetFormat,
    file_name: String,
    data: Bytes,
}

impl fmt::Debug for Conversion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Conversion")
            .field("target", &self.target)
            .field("file_name", &self.file_name)
            .field("len", &self.data.len())
            .finish()
    }
}

impl Conversion {
    /// Convert `data` into `target`, named after `given` (made inert by
    /// [`filename`], its extension replaced by the target's: Meta warns
    /// that a name whose extension does not match the type is a common
    /// cause of `131053`).
    pub fn new(target: TargetFormat, given: Option<&str>, data: Bytes) -> Self {
        let cleaned = filename(given, target.mime_type());
        let stem = cleaned
            .rsplit_once('.')
            .map_or(cleaned.as_str(), |(stem, _)| stem);
        let stem = if stem.trim_matches(['.', '_']).is_empty() {
            "upload"
        } else {
            stem
        };
        Self {
            target,
            file_name: format!("{stem}.{}", target.extension()),
            data,
        }
    }

    /// The format wanted.
    pub fn target(&self) -> TargetFormat {
        self.target
    }

    /// The file name that will be sent to Meta.
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    /// The file to convert.
    pub fn data(&self) -> &Bytes {
        &self.data
    }
}

// ─── Download ────────────────────────────────────────────────────────────

/// A download's limits: the largest file accepted, and whether it is
/// streamed. Only [`Self::parse`] and [`Self::new`] make them, within the
/// caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadLimits {
    max_bytes: u64,
    stream: bool,
}

impl DownloadLimits {
    /// The limits a request gave as text (`?stream=` and `?max_bytes=`):
    /// `stream` is `true` or `false` (absent: `false`), then `max_bytes`
    /// from 1 to the cap, [`MAX_WHOLE_DOWNLOAD`] for a whole file or
    /// `media_max_bytes` (the adapter's cap, the service's
    /// `WA_SERVER_MEDIA_MAX_BYTES`) for a stream (absent: the cap).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `stream`, then on `max_bytes`.
    pub fn parse(
        stream: Option<&str>,
        max_bytes: Option<&str>,
        media_max_bytes: u64,
    ) -> Result<Self, ServiceError> {
        let stream = match stream {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(ServiceError::invalid("stream")),
        };
        let cap = Self::cap(stream, media_max_bytes);
        let max_bytes = match max_bytes {
            None => cap,
            Some(value) => value
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=cap).contains(n))
                .ok_or_else(|| ServiceError::invalid("max_bytes"))?,
        };
        Ok(Self { max_bytes, stream })
    }

    /// The limits as values, checked as [`Self::parse`] checks them.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `max_bytes`: 0, or over the cap.
    pub fn new(max_bytes: u64, stream: bool, media_max_bytes: u64) -> Result<Self, ServiceError> {
        if (1..=Self::cap(stream, media_max_bytes)).contains(&max_bytes) {
            Ok(Self { max_bytes, stream })
        } else {
            Err(ServiceError::invalid("max_bytes"))
        }
    }

    fn cap(stream: bool, media_max_bytes: u64) -> u64 {
        if stream {
            media_max_bytes
        } else {
            MAX_WHOLE_DOWNLOAD
        }
    }

    /// The largest file accepted.
    pub fn max_bytes(self) -> u64 {
        self.max_bytes
    }

    /// Whether it is streamed.
    pub fn stream(self) -> bool {
        self.stream
    }
}

/// A SHA-256 as Meta reports it, hex (64 characters) or base64, as the
/// library reads it.
fn reported_sha256(value: &str) -> Option<[u8; 32]> {
    let value = value.trim();
    let bytes = if value.len() == 64 {
        hex::decode(value).ok()?
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .ok()?
    };
    bytes.try_into().ok()
}

/// The content type to answer: Meta's, when it is a plain MIME type (a
/// `/`, visible ASCII and spaces only), else `application/octet-stream`.
fn content_type(info: &MediaInfo) -> String {
    let valid = info.mime_type.contains('/')
        && info
            .mime_type
            .bytes()
            .all(|b| b.is_ascii_graphic() || b == b' ');
    if valid {
        info.mime_type.clone()
    } else {
        "application/octet-stream".to_owned()
    }
}

/// A verified download, its head and its body.
#[derive(Debug)]
pub struct Downloaded {
    /// The content type to answer: Meta's, when it is a plain MIME type,
    /// else `application/octet-stream`.
    pub content_type: String,
    /// The file's SHA-256, as Meta reported it and the body is checked
    /// against.
    pub sha256: [u8; 32],
    /// The file.
    pub body: DownloadBody,
}

impl Downloaded {
    /// [`Self::sha256`] in hex (lowercase).
    pub fn sha256_hex(&self) -> String {
        hex::encode(self.sha256)
    }
}

/// A download's body.
#[derive(Debug)]
pub enum DownloadBody {
    /// The whole file, read and verified.
    Whole(Bytes),
    /// The file as it arrives, one chunk behind (see [`MediaStream`]).
    Stream(MediaStream),
}

/// Why a [`MediaStream`] stopped before its end: what its adapter aborts
/// its answer on, so the caller never sees a complete body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StreamStopped {
    /// The bytes did not match the SHA-256 Meta reported.
    #[error("the media failed its integrity check")]
    Integrity,
    /// The file grew past its `max_bytes`.
    #[error("the media grew past max_bytes")]
    TooLarge,
    /// The download failed.
    #[error("the media download failed")]
    Failed,
}

/// The verified body of a streamed download, a `futures::Stream` of the
/// library's `Bytes` passed on **one chunk behind**: the last chunk is
/// yielded only once the download ended with the digest matching, so even
/// a caller that ignores the error never holds the whole of a tampered
/// file. Past `max_bytes`, on a mismatch or a failure, it yields a
/// [`StreamStopped`] (logged at `warn`), drops the chunk it held, and
/// ends. It holds one chunk, never the file, and its download's slot until
/// it is dropped.
pub struct MediaStream {
    // Boxed: the hasher's state, beside a whole file's `Bytes`.
    inner: Box<VerifiedBody>,
    held: Option<Bytes>,
    read: u64,
    max_bytes: u64,
    done: bool,
    _slot: Slot,
}

impl fmt::Debug for MediaStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaStream")
            .field("read", &self.read)
            .field("max_bytes", &self.max_bytes)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl MediaStream {
    fn stop(&mut self, why: StreamStopped) -> Result<Bytes, StreamStopped> {
        self.done = true;
        self.held = None;
        tracing::warn!(
            target: LOG_TARGET,
            reason = %why,
            "a streamed media download stopped: connection aborted"
        );
        Err(why)
    }
}

impl Stream for MediaStream {
    type Item = Result<Bytes, StreamStopped>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.done {
                return Poll::Ready(None);
            }
            let chunk = match self.inner.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    // Verified: the digest matched.
                    self.done = true;
                    return Poll::Ready(self.held.take().map(Ok));
                }
                Poll::Ready(Some(Err(Error::Transport(TransportError::Integrity(_))))) => {
                    return Poll::Ready(Some(self.stop(StreamStopped::Integrity)));
                }
                Poll::Ready(Some(Err(_))) => {
                    return Poll::Ready(Some(self.stop(StreamStopped::Failed)));
                }
                Poll::Ready(Some(Ok(chunk))) => chunk,
            };
            self.read = self
                .read
                .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
            if self.read > self.max_bytes {
                return Poll::Ready(Some(self.stop(StreamStopped::TooLarge)));
            }
            if chunk.is_empty() {
                continue;
            }
            if let Some(previous) = self.held.replace(chunk) {
                return Poll::Ready(Some(Ok(previous)));
            }
        }
    }
}

/// Read a verified body whole, at most `max_bytes`: the stream ended
/// without the library's integrity error, so the file is verified.
async fn whole(mut body: VerifiedBody, max_bytes: u64) -> Result<Bytes, ServiceError> {
    let mut data = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|error| ServiceError::from_library(&error))?;
        if u64::try_from(data.len() + chunk.len()).unwrap_or(u64::MAX) > max_bytes {
            return Err(ServiceError::new(ErrorCode::MEDIA_TOO_LARGE));
        }
        data.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(data))
}

// ─── The operations ──────────────────────────────────────────────────────

/// The adapter's pools of transfer slots, per replica (the service's
/// `WA_SERVER_MEDIA_CONCURRENCY` and `WA_SERVER_MEDIA_STREAMS`).
#[derive(Debug, Clone, Copy)]
pub struct MediaSlots<'a> {
    /// Uploads and whole downloads: what holds a file in memory.
    pub transfers: &'a Slots,
    /// Streamed downloads.
    pub streams: &'a Slots,
}

/// The media operations on a number the caller's tenant owns: an
/// [`OwnedNumber`] its [`Authorizer`] made, and admits, with that number's
/// token, taking slots of its tenant's share of [`MediaSlots`], and
/// reading the media ids the bundle's [`Outbox`] recorded as received.
/// Cheap: a few references.
pub struct Media<'a> {
    authz: &'a Authorizer,
    number: &'a OwnedNumber,
    slots: MediaSlots<'a>,
    received: &'a dyn Outbox,
    observer: &'a dyn Observer,
    converter: Option<&'a dyn MediaConverter>,
}

/// Where the service recorded a media id as received, for the caller's
/// number (see the [module](self), "Received media").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Received {
    /// On this number, routed to its tenant, from the binding epoch on:
    /// downloaded without `phone_number_id`.
    Here,
    /// Elsewhere only: not found, nothing asked of Meta.
    Elsewhere,
    /// Never: Meta's `phone_number_id` check.
    Never,
}

impl Received {
    /// Where `rows` (the outbox's rows of one media id,
    /// [`Outbox::received_media`]) place it for `tenant`'s number `pn`
    /// under its binding's cut `from` ([`crate::authz::history_from`]):
    /// [`Self::Here`] when one of them is that number's, routed to that
    /// tenant, and received at or after the cut (the inbox's own test,
    /// `crate::inbox::visible`), whatever the others name; else
    /// [`Self::Elsewhere`] when there is any row; else [`Self::Never`].
    pub(crate) fn among(
        rows: &[ReceivedMedia],
        tenant: &TenantId,
        pn: &PhoneNumberId,
        from: Option<OffsetDateTime>,
    ) -> Self {
        if rows.is_empty() {
            return Self::Never;
        }
        let here = rows.iter().any(|row| {
            row.tenant.as_ref() == Some(tenant)
                && row.phone_number_id.as_ref() == Some(pn)
                && crate::inbox::visible(from, row.received_at)
        });
        if here { Self::Here } else { Self::Elsewhere }
    }
}

impl fmt::Debug for Media<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Media")
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> Media<'a> {
    /// The media operations on `number`, once `authz` admits it
    /// ([`Authorizer::admit_number`]), with slots of `slots`, reading the
    /// media ids `received` (the bundle's outbox, the one the webhook
    /// pipeline writes) recorded as received: a failed call marks through
    /// `authz` ([`OwnedNumber::failed`]). Failed Graph calls are reported
    /// to nobody until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `number` another [`Authorizer`] made: `403 forbidden`, logged at
    /// `warn`, and nothing is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        number: &'a OwnedNumber,
        slots: MediaSlots<'a>,
        received: &'a dyn Outbox,
    ) -> Result<Self, ServiceError> {
        authz.admit_number(number)?;
        Ok(Self {
            authz,
            number,
            slots,
            received,
            observer: &(),
            converter: None,
        })
    }

    /// Convert uploads that ask for it with `converter` (the deployment's,
    /// `WA_SERVER_MEDIA_CONVERTER`): without one, [`Self::convert`] is
    /// `422` on `convert` and no request is made.
    #[must_use]
    pub fn converting_with(mut self, converter: Option<&'a dyn MediaConverter>) -> Self {
        self.converter = converter;
        self
    }

    /// Convert `conversion` into the upload it asks for, checked as any
    /// upload is ([`Upload::new`]): the converted bytes are what
    /// [`Self::upload`] sends. Nothing is sent to Meta, and the converter
    /// runs under its own guards (the library's `FfmpegConverter`:
    /// input, pixel, frame, length, output and time bounds; its
    /// protocols and formats restricted).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `convert` (no converter is configured), or
    /// on `file` (empty, too large for the converter, or not media it
    /// reads); `502 media_conversion_failed` (the converter is missing,
    /// lacks an encoder, timed out (retryable), answered the wrong format
    /// or was killed); `413 media_too_large` (the result passes its
    /// limit). None of them was sent: `may_have_been_sent` is false.
    pub async fn convert(&self, conversion: &Conversion) -> Result<Upload, ServiceError> {
        let Some(converter) = self.converter else {
            return Err(ServiceError::invalid("convert"));
        };
        let output = converter
            .convert(MediaInput::new(conversion.data.clone()), conversion.target)
            .await
            .map_err(|error| match &error {
                // An empty input, or one too large: the caller's file.
                Error::Validation(_) => ServiceError::invalid("file"),
                _ => ServiceError::from_library(&error),
            })?;
        // The port's promise: the format asked for, never another one
        // passed off as it.
        if output.format != conversion.target {
            tracing::warn!(
                target: LOG_TARGET,
                "a media converter answered another format than the one asked for"
            );
            return Err(ServiceError::new(ErrorCode::MEDIA_CONVERSION_FAILED));
        }
        Upload::new(
            conversion.target.mime_type(),
            Some(&conversion.file_name),
            output.data,
        )
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// A slot of the number's tenant's share of the transfers pool, for an
    /// upload: taken before its body is read, so that a body that trickles
    /// in holds one of its own tenant's slots, never another tenant's.
    /// Released when dropped.
    ///
    /// # Errors
    ///
    /// `429 too_many_requests` (`Retry-After: 1`): the tenant's share, or
    /// the pool, is taken.
    pub fn transfer_slot(&self) -> Result<Slot, ServiceError> {
        Self::slot_of(self.slots.transfers, self.number)
    }

    fn slot_of(slots: &Slots, number: &OwnedNumber) -> Result<Slot, ServiceError> {
        slots
            .try_acquire(number.tenant())
            .ok_or_else(|| ServiceError::too_many_requests(1))
    }

    /// The error of a failed Graph call: the number's `failed` (a `190`
    /// marks its WABA's numbers), counted when Meta answered with a Graph
    /// error, without Meta's `details` yet.
    async fn counted(&self, error: &Error) -> ServiceError {
        let api = self.number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    /// [`Self::counted`], with Meta's `details`.
    async fn failed(&self, error: &Error) -> ServiceError {
        self.counted(error).await.with_details(error)
    }

    /// [`Self::counted`] for a call on a media id: Meta refusing it is
    /// `404 not_found` ([`object_failed`]).
    async fn failed_on_object(&self, error: &Error) -> ServiceError {
        object_failed(self.counted(error).await, error)
    }

    /// The library's media API on the number, scoped to it
    /// (`phone_number_id={pn}` on the lookup and the deletion).
    fn scoped(&self) -> LibraryMedia {
        self.unscoped().restrict_to_phone_number()
    }

    /// The library's media API with the number's token, not scoped: for a
    /// media id the service recorded as received on the number only
    /// ([`Received::Here`]).
    fn unscoped(&self) -> LibraryMedia {
        self.number
            .client()
            .media(self.number.phone_number_id().clone())
    }

    /// Where the service recorded `id` as received, for this number: the
    /// outbox's rows of it ([`Outbox::received_media`]), one of them this
    /// number's, routed to its tenant, and received from the binding epoch
    /// on ([`crate::authz::history_from`]); others only; or none. Asks
    /// Meta nothing.
    ///
    /// # Errors
    ///
    /// `503 storage_unavailable`.
    pub async fn received(&self, id: &MediaId) -> Result<Received, ServiceError> {
        let rows = self.received.received_media(id).await?;
        Ok(Received::among(
            &rows,
            self.number.tenant(),
            self.number.phone_number_id(),
            self.number.history_from(),
        ))
    }

    /// The library's media API for a download of `id`: unscoped for a
    /// media id received here, once the number's binding still holds;
    /// scoped for one never recorded; `404 not_found` for one received
    /// elsewhere, nothing asked.
    async fn download_api(&self, id: &MediaId) -> Result<LibraryMedia, ServiceError> {
        match self.received(id).await? {
            Received::Here => {
                // The exemption is the inbox's history: used only while
                // the binding it was read under holds.
                self.number.still_held(self.authz).await?;
                Ok(self.unscoped())
            }
            Received::Elsewhere => Err(Self::received_elsewhere()),
            Received::Never => Ok(self.scoped()),
        }
    }

    /// A media id received elsewhere: `404 not_found`, as a missing one.
    fn received_elsewhere() -> ServiceError {
        tracing::info!(
            target: LOG_TARGET,
            "a media id received on another number, or before the number's binding: \
             not found, Meta not asked"
        );
        ServiceError::not_found()
    }

    /// Upload `upload` with the number's token.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details` (a timeout
    /// `may_have_been_sent`).
    pub async fn upload(&self, upload: &Upload) -> Result<Uploaded, ServiceError> {
        let uploaded = self
            .number
            .client()
            .media(self.number.phone_number_id().clone())
            .upload(upload.data.clone(), &upload.mime_type, &upload.filename)
            .await;
        match uploaded {
            Ok(media_id) => Ok(Uploaded { media_id }),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The media `media_id` of the number, within `limits`, verified
    /// against the SHA-256 Meta reports (see the [module](self)): whole,
    /// or as a [`MediaStream`] holding its slot. A media id the service
    /// recorded as received on the number is asked without
    /// `phone_number_id` ([`Self::received`]).
    ///
    /// # Errors
    ///
    /// In this order: `422 invalid_request` on `media_id` (not a Meta id),
    /// before any request; `429 too_many_requests` (the tenant's share of
    /// the pool `limits` needs is taken), before any request; `404
    /// not_found` for a media id recorded as received elsewhere (another
    /// number, another tenant, before the binding epoch), before any
    /// request; `503 storage_unavailable` (the outbox; for a media id
    /// received here, the number's binding moved since it was opened);
    /// `404 not_found`, without Meta's code or text (Meta refusing the id,
    /// or answering for another node); Meta's failure otherwise, with its
    /// `details`; `413 media_too_large` (the size Meta reports is over
    /// `max_bytes`); `502 integrity` (no usable SHA-256 reported), before
    /// the download; `502 upstream` (Meta's URL refused by the library's
    /// host allow-list: the token is not sent); Meta's failure to serve the
    /// file, with its `details`. A whole file: `413 media_too_large` (past
    /// `max_bytes`), `502 integrity` (the digest does not match), or the
    /// download's failure, with no byte of the file answered.
    pub async fn download(
        &self,
        media_id: &str,
        limits: DownloadLimits,
    ) -> Result<Downloaded, ServiceError> {
        let id: MediaId = typed_graph_id("media_id", media_id)?;
        // A whole file is held in memory, a stream holds two connections:
        // take a slot of the tenant's share first.
        let slot = Self::slot_of(
            if limits.stream {
                self.slots.streams
            } else {
                self.slots.transfers
            },
            self.number,
        )?;
        let media = self.download_api(&id).await?;
        let info = match media.url(&id).await {
            // A media node, the one asked for.
            Ok(info) if info.id.as_str() == media_id => info,
            Ok(_) => return Err(ServiceError::not_found()),
            Err(error) => return Err(self.failed_on_object(&error).await),
        };
        // Meta's reported size, before a byte is downloaded (checked again
        // while reading: the report may be wrong).
        if info.file_size.is_some_and(|size| size > limits.max_bytes) {
            return Err(ServiceError::new(ErrorCode::MEDIA_TOO_LARGE));
        }
        let Some(sha256) = reported_sha256(&info.sha256) else {
            tracing::warn!(
                target: LOG_TARGET,
                "Meta reported no usable SHA-256 for a media file"
            );
            return Err(ServiceError::new(ErrorCode::INTEGRITY));
        };
        let content_type = content_type(&info);
        let download = match media.download_with_info(info).await {
            Ok(download) => download,
            Err(error) => return Err(self.download_failed(&error).await),
        };
        let verified = download
            .verified()
            .map_err(|_| ServiceError::new(ErrorCode::INTEGRITY))?;
        let body = if limits.stream {
            DownloadBody::Stream(MediaStream {
                inner: Box::new(verified.body),
                held: None,
                read: 0,
                max_bytes: limits.max_bytes,
                done: false,
                _slot: slot,
            })
        } else {
            let data = whole(verified.body, limits.max_bytes).await?;
            drop(slot);
            DownloadBody::Whole(data)
        };
        Ok(Downloaded {
            content_type,
            sha256,
            body,
        })
    }

    /// A failed download step: the lookaside URL Meta gave being refused
    /// by the library's host allow-list is Meta's answer gone wrong
    /// (`upstream`), not the caller's input.
    async fn download_failed(&self, error: &Error) -> ServiceError {
        if matches!(error, Error::Validation(_)) {
            tracing::warn!(
                target: LOG_TARGET,
                "Meta's media URL was refused by the library's host allow-list"
            );
            return ServiceError::new(ErrorCode::UPSTREAM);
        }
        self.failed(error).await
    }

    /// Delete the media `media_id` of the number: looked up first, as this
    /// number's media (`DELETE /{id}` deletes whatever node the id names,
    /// a flow, a QR code, that the token reaches), and only that deleted.
    /// Always with `phone_number_id={pn}`, a media id received here
    /// included; one recorded as received elsewhere is not asked of Meta.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `media_id`, before any request; `404
    /// not_found` for a media id recorded as received elsewhere, before any
    /// request; `503 storage_unavailable` (the outbox); `404 not_found`,
    /// without Meta's code or text, and nothing deleted (Meta refusing the
    /// id, or answering for another node); Meta's refusal of the deletion
    /// (`404` as well) or failure, with its `details`.
    pub async fn delete(&self, media_id: &str) -> Result<(), ServiceError> {
        let id: MediaId = typed_graph_id("media_id", media_id)?;
        if self.received(&id).await? == Received::Elsewhere {
            return Err(Self::received_elsewhere());
        }
        let media = self.scoped();
        match media.url(&id).await {
            Ok(info) if info.id == id => {}
            Ok(_) => return Err(ServiceError::not_found()),
            Err(error) => return Err(self.failed_on_object(&error).await),
        }
        match media.delete(&id).await {
            Ok(()) => Ok(()),
            Err(error) => Err(self.failed_on_object(&error).await),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a panic is the report
mod tests {
    use super::*;

    /// Roadmap M2e's rule for the rows of a media id (`Received::among`):
    /// one row of this number, routed to this tenant, received at or
    /// after the cut, is `Here`, whatever other rows name the id; a row
    /// received the instant the cut began is inside it (the inbox's own
    /// test), one an instant before is not; another tenant's row on the
    /// number, the tenant's row on another number and an operator-only row
    /// are `Elsewhere`; no row is `Never`. Decisive: the cut's `>=` made
    /// `>` (the instant of the cut), `any` made `all` (a row here beside
    /// one elsewhere), and each clause of the filter.
    #[test]
    fn a_received_media_id_is_here_from_the_instant_of_the_cut_on() {
        let tenant = TenantId::parse("tenant-a").unwrap();
        let other = TenantId::parse("tenant-b").unwrap();
        let pn = PhoneNumberId::new("106540352242922").unwrap();
        let other_pn = PhoneNumberId::new("106540352242923").unwrap();
        let cut = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        let row =
            |tenant: Option<&TenantId>, pn: &PhoneNumberId, at: OffsetDateTime| ReceivedMedia {
                tenant: tenant.cloned(),
                phone_number_id: Some(pn.clone()),
                received_at: at,
            };
        let instant = time::Duration::nanoseconds(1);
        let among = |rows: &[ReceivedMedia], from| Received::among(rows, &tenant, &pn, from);

        assert_eq!(among(&[], Some(cut)), Received::Never);
        assert_eq!(
            among(&[row(Some(&tenant), &pn, cut)], Some(cut)),
            Received::Here
        );
        assert_eq!(
            among(&[row(Some(&tenant), &pn, cut - instant)], Some(cut)),
            Received::Elsewhere,
            "an instant before the cut"
        );
        assert_eq!(
            among(
                &[row(Some(&tenant), &pn, cut - time::Duration::days(30))],
                None
            ),
            Received::Here,
            "no cut: a number no other tenant held"
        );
        for (rows, why) in [
            (
                vec![row(Some(&other), &pn, cut)],
                "another tenant's row on the number",
            ),
            (
                vec![row(Some(&tenant), &other_pn, cut)],
                "the tenant's row on another number",
            ),
            (
                vec![row(None, &pn, cut)],
                "an operator-only row on the number",
            ),
            (
                vec![ReceivedMedia {
                    phone_number_id: None,
                    ..row(Some(&tenant), &pn, cut)
                }],
                "a row naming no number",
            ),
        ] {
            assert_eq!(among(&rows, Some(cut)), Received::Elsewhere, "{why}");
        }
        // One row here is enough, wherever else the id was recorded.
        let mixed = [
            row(Some(&other), &other_pn, cut + instant),
            row(None, &pn, cut + instant),
            row(Some(&tenant), &pn, cut),
        ];
        assert_eq!(among(&mixed, Some(cut)), Received::Here);
    }

    #[test]
    fn file_names_are_inert_and_have_an_extension() {
        assert_eq!(filename(Some("voucher.png"), "image/png"), "voucher.png");
        assert_eq!(
            filename(Some("a;b\r\nContent-Type: x.png"), "image/png"),
            "a_b__Content-Type__x.png"
        );
        // A quote would end the multipart header's `filename="…"`.
        assert_eq!(
            filename(Some("a\"b\r\nContent-Type: x.png"), "image/png"),
            "a_b__Content-Type__x.png"
        );
        assert_eq!(filename(None, "image/png"), "upload.png");
        assert_eq!(filename(Some(""), "audio/ogg; codecs=opus"), "upload.ogg");
        assert_eq!(filename(Some("é"), "application/pdf"), "upload.pdf");
        assert_eq!(filename(Some(&"x".repeat(300)), "image/png").len(), 128);
    }

    #[test]
    fn meta_digests_are_read_in_hex_or_base64() {
        let hex = "3f9d94d399fa61c191bc1d4ca71375a035cd9b9f5b1128e1f0963a415c16b0cc";
        let raw = reported_sha256(hex).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        assert_eq!(reported_sha256(&b64), Some(raw));
        assert_eq!(reported_sha256("PHOTO_HASH"), None);
        assert_eq!(reported_sha256(""), None);
    }

    #[test]
    fn the_extension_table_is_metas_supported_types() {
        for (mime, _) in EXTENSIONS {
            assert!(MediaKind::for_mime_type(mime).is_some(), "{mime}");
        }
        assert_eq!(EXTENSIONS.len(), 18);
    }

    /// The content type answered is Meta's only when it is a plain MIME
    /// type: a header must not carry Meta's control characters.
    #[test]
    fn the_content_type_is_metas_when_it_is_plain() {
        let info = |mime_type: &str| MediaInfo {
            messaging_product: "whatsapp".to_owned(),
            url: String::new(),
            mime_type: mime_type.to_owned(),
            sha256: String::new(),
            file_size: None,
            id: MediaId::new("1").unwrap(),
        };
        assert_eq!(content_type(&info("image/jpeg")), "image/jpeg");
        assert_eq!(
            content_type(&info("audio/ogg; codecs=opus")),
            "audio/ogg; codecs=opus"
        );
        for odd in ["", "jpeg", "image/jpeg\r\nX: y", "image/\u{e9}", "a/\tb"] {
            assert_eq!(
                content_type(&info(odd)),
                "application/octet-stream",
                "{odd:?}"
            );
        }
    }

    /// The limits' text, in the order a request is checked: `stream`, then
    /// `max_bytes` within its cap (16 MiB whole, the adapter's streamed).
    #[test]
    fn download_limits_are_read_within_their_caps() {
        let max = 100 * 1024 * 1024;
        let limits = DownloadLimits::parse(None, None, max).unwrap();
        assert_eq!(
            (limits.max_bytes(), limits.stream()),
            (MAX_WHOLE_DOWNLOAD, false)
        );
        let limits = DownloadLimits::parse(Some("true"), None, max).unwrap();
        assert_eq!((limits.max_bytes(), limits.stream()), (max, true));
        let limits = DownloadLimits::parse(Some("false"), Some("100"), max).unwrap();
        assert_eq!((limits.max_bytes(), limits.stream()), (100, false));
        for (stream, max_bytes, field) in [
            (Some("yes"), Some("0"), "stream"),
            (None, Some("16777217"), "max_bytes"),
            (None, Some("0"), "max_bytes"),
            (None, Some("x"), "max_bytes"),
            (Some("true"), Some("104857601"), "max_bytes"),
        ] {
            let refused = DownloadLimits::parse(stream, max_bytes, max).unwrap_err();
            assert_eq!(
                (refused.code(), refused.field()),
                ("invalid_request", Some(field))
            );
        }
        assert_eq!(
            DownloadLimits::new(MAX_WHOLE_DOWNLOAD, false, max),
            Ok(DownloadLimits {
                max_bytes: MAX_WHOLE_DOWNLOAD,
                stream: false
            })
        );
        for (max_bytes, stream) in [(0, true), (MAX_WHOLE_DOWNLOAD + 1, false), (max + 1, true)] {
            let refused = DownloadLimits::new(max_bytes, stream, max).unwrap_err();
            assert_eq!(refused.field(), Some("max_bytes"));
        }
    }

    /// Uploads: Meta's supported types and sizes, before any request.
    #[test]
    fn uploads_are_checked_against_metas_table() {
        let upload = Upload::new(" image/png ", Some("a b.png"), Bytes::from_static(b"x")).unwrap();
        assert_eq!(
            (upload.mime_type(), upload.filename(), &upload.data()[..]),
            ("image/png", "a_b.png", &b"x"[..])
        );
        for kind in ["image/gif", "image/png\u{7}", ""] {
            let refused = Upload::new(kind, None, Bytes::from_static(b"x")).unwrap_err();
            assert_eq!(refused.field(), Some("type"), "{kind:?}");
        }
        let image = Bytes::from(vec![0u8; 5 * 1024 * 1024 + 1]);
        let refused = Upload::new("image/png", None, image).unwrap_err();
        assert_eq!((refused.status(), refused.code()), (413, "media_too_large"));
        assert_eq!(upload_cap(Some("image/png"), u64::MAX), 5 * 1024 * 1024);
        assert_eq!(upload_cap(Some("image/png"), 10), 10);
        assert_eq!(upload_cap(Some("image/gif"), 77), 77);
        assert_eq!(upload_cap(None, 77), 77);
    }
}
