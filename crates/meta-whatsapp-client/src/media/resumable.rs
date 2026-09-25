//! Resumable Upload API: the handle (`h`) used as `header_handle` in
//! template examples and as `profile_picture_handle`.
//!
//! The WhatsApp docs (`templates/template-media`, `templates/components`)
//! only link to the Graph API guide
//! `https://developers.facebook.com/docs/graph-api/guides/upload`, which is
//! outside the WhatsApp mirror. Shapes below come from that guide (read
//! 2026-09-24):
//!
//! 1. `POST /{app_id}/uploads?file_name&file_length&file_type` →
//!    `{"id": "upload:<UPLOAD_SESSION_ID>"}`.
//! 2. `POST /upload:<UPLOAD_SESSION_ID>` with headers
//!    `Authorization: OAuth <token>` and `file_offset: <n>`, raw bytes as
//!    body → `{"h": "<handle>"}`. The guide: "You must include the access
//!    token in the header or the call will fail."
//! 3. `GET /upload:<UPLOAD_SESSION_ID>` with the same auth →
//!    `{"id": …, "file_offset": <n>}` to resume an interrupted upload.
//!
//! Deliberate deviation: the guide's step 1 `curl` puts `access_token` in
//! the query string. We send `Authorization: OAuth` on every step instead
//! (Graph accepts a header token on any endpoint), so the token never
//! appears in a URL — proxies and transports log URLs.
//!
//! Session ids go into the URL the way the guide's `curl` puts them there:
//! pasted verbatim, so anything after a `?` (ids seen in the wild carry a
//! `?sig=…` suffix) is a query string, not part of the path. We split on
//! the first `?` explicitly: the part before it becomes **one** path
//! segment (a `/` in it is percent-encoded and cannot reach another
//! object), the part after it becomes query parameters.

use bytes::Bytes;
use http::Method;
use serde::Deserialize;
use meta_whatsapp_core::error::{ConfigError, ValidationError};
use meta_whatsapp_core::ids::{AppId, UploadHandle, UploadSessionId};
use meta_whatsapp_core::secret::AccessToken;
use meta_whatsapp_core::{Error, Result};

use super::Media;
use crate::GraphRequest;
use crate::messages::validate;

/// File types the guide lists as valid `file_type` values.
const RESUMABLE_TYPES: &[&str] = &[
    "application/pdf",
    "image/jpeg",
    "image/jpg",
    "image/png",
    "video/mp4",
];

#[derive(Deserialize)]
struct SessionCreated {
    id: UploadSessionId,
}

#[derive(Deserialize)]
struct ChunkUploaded {
    #[serde(default)]
    h: Option<UploadHandle>,
}

#[derive(Deserialize)]
struct SessionStatus {
    #[serde(deserialize_with = "super::de_u64")]
    file_offset: u64,
}

impl Media {
    fn upload_token(&self) -> Result<AccessToken> {
        self.client.token().cloned().ok_or_else(|| {
            ConfigError::new(
                "the Resumable Upload API needs an access token (Authorization: OAuth)",
            )
            .into()
        })
    }

    /// A request to `/{session}`, split on the first `?` as the module docs
    /// describe.
    fn session_request(&self, method: Method, session: &UploadSessionId) -> Result<GraphRequest> {
        let (path, query) = match session.as_str().split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (session.as_str(), None),
        };
        // The guide's ids are `upload:<UPLOAD_SESSION_ID>`. Anything else
        // would aim the token at some other Graph object (`GET /{waba_id}`),
        // whose body would then come back inside a decode error.
        if path.strip_prefix("upload:").is_none_or(str::is_empty) {
            return Err(ValidationError::new(
                "upload_session_id",
                "must be an `upload:<id>` session id as returned by `start_upload_session`",
            )
            .into());
        }
        let mut req = self.client.request_at(method, &[path]);
        if let Some(q) = query {
            for (k, v) in url::form_urlencoded::parse(q.as_bytes()) {
                req = req.query(&k, v);
            }
        }
        Ok(req.oauth(&self.upload_token()?))
    }

    /// Step 1: open an upload session for a `file_length`-byte file.
    ///
    /// `file_type` must be one of `application/pdf`, `image/jpeg`,
    /// `image/jpg`, `image/png`, `video/mp4`.
    pub async fn start_upload_session(
        &self,
        app_id: &AppId,
        file_name: &str,
        file_length: u64,
        file_type: &str,
    ) -> Result<UploadSessionId> {
        validate::non_empty("file_name", file_name)?;
        if !RESUMABLE_TYPES.contains(&file_type) {
            return Err(ValidationError::new(
                "file_type",
                format!("`{file_type}` is not accepted by the Resumable Upload API"),
            )
            .into());
        }
        let created: SessionCreated = self
            .client
            .post_at(&[app_id.as_str(), "uploads"])
            .query("file_name", file_name)
            .query("file_length", file_length)
            .query("file_type", file_type)
            .oauth(&self.upload_token()?)
            .context("upload session response")
            .send()
            .await?;
        Ok(created.id)
    }

    /// Step 2: upload `data` starting at byte `file_offset`.
    ///
    /// Returns the file handle once Meta has the whole file, `None` if the
    /// response carries no handle. Not replayed on transient errors: ask
    /// [`upload_session_status`](Self::upload_session_status) where to
    /// resume instead.
    pub async fn upload_chunk(
        &self,
        session: &UploadSessionId,
        file_offset: u64,
        data: impl Into<Bytes>,
    ) -> Result<Option<UploadHandle>> {
        let uploaded: ChunkUploaded = self
            .session_request(Method::POST, session)?
            .header("file_offset", file_offset.to_string())
            // The guide uses `curl --data-binary` without a content type.
            .bytes("application/octet-stream", data)
            .context("upload response")
            .send()
            .await?;
        Ok(uploaded.h)
    }

    /// Step 3: the offset to resume an interrupted upload from.
    pub async fn upload_session_status(&self, session: &UploadSessionId) -> Result<u64> {
        let status: SessionStatus = self
            .session_request(Method::GET, session)?
            .context("upload session status response")
            .send()
            .await?;
        Ok(status.file_offset)
    }

    /// Steps 1 and 2 for a file already in memory: returns its handle.
    ///
    /// Failures are wrapped with [`Error::in_step`]: `"start_upload_session"`
    /// or `"upload_file"`.
    pub async fn resumable_upload(
        &self,
        app_id: &AppId,
        file_name: &str,
        file_type: &str,
        data: impl Into<Bytes>,
    ) -> Result<UploadHandle> {
        let data = data.into();
        let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
        let session = self
            .start_upload_session(app_id, file_name, len, file_type)
            .await
            .map_err(|e| e.in_step("start_upload_session"))?;
        match self
            .upload_chunk(&session, 0, data)
            .await
            .map_err(|e| e.in_step("upload_file"))?
        {
            Some(handle) => Ok(handle),
            None => Err(Error::decode(
                "upload response",
                <serde_json::Error as serde::de::Error>::missing_field("h"),
                b"",
            )
            .in_step("upload_file")),
        }
    }
}
