//! Upload, retrieve URL, download (streaming, SHA-256 verified), delete
//! media; Resumable Upload API for template sample media (`header_handle`).
//!
//! Docs: `business-phone-numbers/media`, `reference/media/*`,
//! `reference/whatsapp-business-phone-number/media-upload-api`,
//! `templates/template-media`; the Resumable Upload API itself is the Graph
//! guide `docs/graph-api/guides/upload` (see [`Media::start_upload_session`];
//! its ids are [`wa_core::ids::UploadSessionId`] and
//! [`wa_core::ids::UploadHandle`]).
//!
//! Media ids are sent as single path segments (`GET /{media-id}`), so an id
//! containing `/` is percent-encoded and cannot address another object.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! # Lifetimes
//!
//! Uploaded media ids live 30 days; ids from webhooks 7 days. A media URL
//! from [`Media::url`] expires after 5 minutes and needs the access token,
//! which this client only attaches to the Graph endpoint and
//! `https://lookaside.fbsbx.com`, where Meta's media URLs point.
//!
//! # Upload and send
//!
//! ```no_run
//! # async fn demo(client: wa_client::Client, png: Vec<u8>) -> wa_core::Result<()> {
//! use wa_client::messages::{Image, OutboundMessage};
//! use wa_core::recipient::Recipient;
//!
//! let media_id = client
//!     .media("106540352242922")
//!     .upload(png, "image/png", "voucher.png") // type and size checked first
//!     .await?;
//! let msg = OutboundMessage::new(
//!     Recipient::phone("+16505551234"),
//!     Image::new(media_id).caption("Your voucher"),
//! );
//! client.messages("106540352242922").send(&msg).await?;
//! # Ok(()) }
//! ```
//!
//! # Streaming download with verification
//!
//! ```no_run
//! # async fn demo(client: wa_client::Client) -> anyhow::Result<()> {
//! use futures::StreamExt;
//! use wa_core::ids::MediaId;
//!
//! let media = client.media("106540352242922");
//! let download = media.download(&MediaId::new("1037543291543636")).await?;
//! let mut verified = download.verified()?; // fails fast on a malformed digest
//! let mut file = Vec::new(); // stand-in for a temporary file
//! while let Some(chunk) = verified.body.next().await {
//!     // The last item is an error if the SHA-256 does not match, so keep
//!     // the bytes out of reach until the loop ends without one.
//!     file.extend_from_slice(&chunk?);
//! }
//! // Only now is `file` trustworthy.
//! # Ok(()) }
//! ```
//!
//! For small files, [`Media::download_bytes`] does the same with a size cap.

mod kinds;
mod resumable;
mod verify;

#[cfg(test)]
mod tests;

pub use kinds::{MediaKind, validate_upload};
pub use verify::{DownloadedMedia, MediaDownload, VerifiedBody, VerifiedDownload};

use bytes::Bytes;
use http::Method;
use serde::{Deserialize, Deserializer};
use url::Url;
use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::{MediaId, PhoneNumberId};
use wa_core::transport::Multipart;

use crate::Client;

/// Entry point, see [`Client::media`].
#[derive(Debug, Clone)]
pub struct Media {
    client: Client,
    phone_number_id: PhoneNumberId,
    restrict: bool,
}

impl Client {
    /// [`Media`] API for `phone_number_id`.
    pub fn media(&self, phone_number_id: impl Into<PhoneNumberId>) -> Media {
        Media {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
            restrict: false,
        }
    }
}

/// `GET /{media-id}` response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MediaInfo {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: String,
    /// Download URL; valid for 5 minutes and only with the access token.
    pub url: String,
    /// MIME type.
    #[serde(default)]
    pub mime_type: String,
    /// SHA-256 of the file (hex or base64, see [`MediaDownload::verified`]).
    #[serde(default)]
    pub sha256: String,
    /// Size in bytes. The docs type it as a string; both forms are read.
    #[serde(default, deserialize_with = "de_opt_u64")]
    pub file_size: Option<u64>,
    /// The media id.
    pub id: MediaId,
}

impl Media {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Send `phone_number_id=<this number>` with [`url`](Self::url),
    /// [`download`](Self::download) and [`delete`](Self::delete), so Meta
    /// refuses media that was not uploaded on this number.
    ///
    /// Useful as a tenant guard in multi-merchant setups. The docs do not
    /// say how this applies to media received by webhook (which was not
    /// "uploaded" on any business number), so it is off by default.
    #[must_use]
    pub fn restrict_to_phone_number(mut self) -> Self {
        self.restrict = true;
        self
    }

    fn scope(&self) -> Option<&PhoneNumberId> {
        self.restrict.then_some(&self.phone_number_id)
    }

    /// Upload a file (`POST /{phone_number_id}/media`, multipart fields
    /// `messaging_product`, `type`, `file`). Returns its media id.
    ///
    /// `mime_type` and the size are checked against Meta's supported-types
    /// table first ([`validate_upload`]). Not replayed on transient errors
    /// (a replay would create a second media object).
    pub async fn upload(
        &self,
        data: impl Into<Bytes>,
        mime_type: &str,
        filename: &str,
    ) -> Result<MediaId> {
        #[derive(Deserialize)]
        struct Uploaded {
            id: MediaId,
        }
        let data = data.into();
        validate_upload(mime_type, u64::try_from(data.len()).unwrap_or(u64::MAX))?;
        let form = Multipart::new()
            .text("messaging_product", "whatsapp")
            // The parameter table requires `type`; the curl example instead
            // sets the file part's content type. We send both.
            .text("type", mime_type)
            .file("file", filename, mime_type, data);
        let uploaded: Uploaded = self
            .client
            .post_at(&[self.phone_number_id.as_str(), "media"])
            .multipart(form)
            .context("media upload response")
            .send()
            .await?;
        Ok(uploaded.id)
    }

    /// Metadata and a short-lived download URL (`GET /{media-id}`).
    pub async fn url(&self, media_id: &MediaId) -> Result<MediaInfo> {
        self.client
            .get_at(&[media_id.as_str()])
            .query_opt("phone_number_id", self.scope())
            .context("media URL response")
            .send()
            .await
    }

    /// Look the media up and start streaming it. The body is unverified;
    /// see [`MediaDownload::verified`].
    pub async fn download(&self, media_id: &MediaId) -> Result<MediaDownload> {
        let info = self.url(media_id).await?;
        self.download_with_info(info).await
    }

    /// Stream media whose URL you already have (from [`Media::url`], or a
    /// media webhook that carried `url` and `sha256`).
    ///
    /// The token is only sent to the Graph endpoint or
    /// `https://lookaside.fbsbx.com`; any other URL fails with a validation
    /// error (field `url`) before a byte is sent. Not retried: fetch a fresh
    /// URL and call again.
    pub async fn download_with_info(&self, info: MediaInfo) -> Result<MediaDownload> {
        let url = Url::parse(&info.url)
            .map_err(|e| ValidationError::new("url", format!("not an absolute URL: {e}")))?;
        let resp = self
            .client
            .request_url(Method::GET, url)
            .context("media download")
            .send_streaming()
            .await?;
        Ok(MediaDownload {
            info,
            body: resp.body,
        })
    }

    /// Download, verify and buffer a file of at most `max_bytes`.
    ///
    /// Refuses before downloading when Meta reports a larger `file_size`,
    /// and stops reading as soon as the body exceeds `max_bytes` (in case
    /// the reported size was wrong).
    pub async fn download_bytes(
        &self,
        media_id: &MediaId,
        max_bytes: u64,
    ) -> Result<DownloadedMedia> {
        let info = self.url(media_id).await?;
        if let Some(size) = info.file_size
            && size > max_bytes
        {
            return Err(verify::too_large(size, max_bytes));
        }
        self.download_with_info(info)
            .await?
            .verified()?
            .collect(max_bytes)
            .await
    }

    /// Delete uploaded media (`DELETE /{media-id}`).
    pub async fn delete(&self, media_id: &MediaId) -> Result<()> {
        self.client
            .delete_at(&[media_id.as_str()])
            .query_opt("phone_number_id", self.scope())
            .context("media delete response")
            .send_success()
            .await
    }
}

/// Accepts `12` or `"12"`: the docs quote numbers that Meta may send bare.
pub(crate) fn de_opt_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Num {
        Int(u64),
        Str(String),
    }
    match Option::<Num>::deserialize(d)? {
        None => Ok(None),
        Some(Num::Int(n)) => Ok(Some(n)),
        Some(Num::Str(s)) => s.trim().parse().map(Some).map_err(serde::de::Error::custom),
    }
}

/// Required variant of [`de_opt_u64`].
pub(crate) fn de_u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    de_opt_u64(d)?.ok_or_else(|| serde::de::Error::custom("expected a number"))
}
