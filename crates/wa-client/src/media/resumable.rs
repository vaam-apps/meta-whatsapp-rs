//! Resumable Upload API: the handle (`h`) used as `header_handle` in
//! template examples and as `profile_picture_handle`.
//!
//! The WhatsApp docs (`templates/template-media`, `templates/components`)
//! only link to the Graph API guide
//! `https://developers.facebook.com/docs/graph-api/guides/upload`, which is
//! outside the WhatsApp mirror. Shapes below come from that guide:
//!
//! 1. `POST /{app_id}/uploads?file_name&file_length&file_type` →
//!    `{"id": "upload:<session>"}`.
//! 2. `POST /upload:<session>` with headers `Authorization: OAuth <token>`
//!    and `file_offset: <n>`, raw bytes as body → `{"h": "<handle>"}`.
//! 3. `GET /upload:<session>` with the same auth →
//!    `{"id": …, "file_offset": <n>}` to resume an interrupted upload.
//!
//! Deliberate deviation: the guide's step 1 puts `access_token` in the query
//! string. We send it as `Authorization: OAuth` on every step instead, so
//! the token never appears in a URL (URLs get logged).

use std::fmt;

use bytes::Bytes;
use http::Method;
use serde::Deserialize;
use wa_core::error::{ConfigError, ValidationError};
use wa_core::ids::AppId;
use wa_core::secret::AccessToken;
use wa_core::{Error, Result};

use super::Media;
use crate::GraphRequest;
use crate::messages::validate;

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap a raw value.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the raw value.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Take the raw value.
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }
    };
}

string_id!(
    /// Upload session id as returned by step 1, `upload:` prefix included.
    UploadSessionId
);
string_id!(
    /// Uploaded file handle (`h`), e.g. `4::aW...`. Pass it as
    /// `header_handle` when creating a template with a media header.
    UploadHandle
);

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

    /// A request to `/{session}`. The session id is used exactly as Meta
    /// returned it, as the guide's `curl` does: anything after a `?` in it
    /// becomes query parameters rather than being escaped into the path.
    fn session_request(&self, method: Method, session: &UploadSessionId) -> Result<GraphRequest> {
        validate::non_empty("upload_session_id", session.as_str())?;
        let (path, query) = match session.as_str().split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (session.as_str(), None),
        };
        let mut req = self.client.request(method, path);
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
        validate::path_id("app_id", app_id.as_str())?;
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
            .post(&format!("{app_id}/uploads"))
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
