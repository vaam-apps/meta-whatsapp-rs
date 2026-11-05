//! Resumable uploads (docs/design/server.md, section 4.2; roadmap M5c1),
//! as services an API adapter calls, with the number's WABA token, over
//! the library's `Media` (Meta's Resumable Upload API, which the
//! WhatsApp pages rely on for a template's `header_handle` and a profile
//! picture's `profile_picture_handle`):
//!
//! | Operation | Graph call |
//! | --- | --- |
//! | [`Uploads::start`] | `POST /{app_id}/uploads?file_name&file_length&file_type` → `{"id": "upload:<session>"}` |
//! | [`Uploads::chunk`] | `POST /upload:<session>` with the `file_offset` header, the bytes as the body → `{"h": "<handle>"}` once Meta has the file |
//! | [`Uploads::status`] | `GET /upload:<session>` → `{"id", "file_offset"}`, none once the service holds the handle |
//!
//! Page: the Graph API guide `docs/graph-api/guides/upload`, outside the
//! WhatsApp pages (the library's `client::media::resumable` module says
//! how it is read). Meta documents the three calls and the five file
//! types ([`FILE_TYPES`]); it documents **no** chunk size, no total size,
//! no session lifetime and no limit on handles. The service's bounds are
//! therefore its own, and say so ([`NewUpload::new`], [`SESSION_TTL`]).
//!
//! **The handle** (`h`) is what the caller keeps: a template's
//! `header_handle` (the service's template creation passes it through)
//! and, from roadmap M5c2, a profile picture's `profile_picture_handle`.
//! It is answered by the chunk that completes the file, and kept in the
//! session's record until [`SESSION_TTL`] after the session began, so
//! that [`Uploads::status`] answers it again after a lost answer. It
//! reaches no log.
//!
//! **A session is the number's, or it does not exist.** The service
//! answers its own `upload_id` (a random 128-bit id, `up_` and 32 hex
//! characters), never Meta's session id, which can carry a signature
//! (`?sig=`) the service keeps out of URLs, answers and logs. The record
//! under it (the library's `JsonStore` over the bundle's `KvStore`,
//! namespace [`NAMESPACE`]) names the tenant, the number, Meta's session,
//! the declared file and, once complete, the handle. A record of another
//! tenant, of another number of the tenant's, an expired one and a missing
//! one are all `404 not_found`, with nothing asked of Meta: one token may
//! reach the sessions of several tenants' numbers (a platform's system
//! user), so only the record decides.
//!
//! **Bounds**, all before any request to Meta: a file type of the five
//! (`422` on `file_type`); a length from 1 byte to its kind's limit in
//! Meta's media table (5 MiB for an image, 16 MiB for a video, 100 MiB
//! for a document: the limits of a template header's media, not
//! the Resumable Upload API's, which states none) and
//! `WA_SERVER_MEDIA_MAX_BYTES` (`413 media_too_large`); a chunk of at
//! least a byte, within the declared file ([`OpenUpload::max_chunk`]:
//! the bytes from its offset to the end, so no chunk, and no sum of
//! chunks, passes the declared length); and a chunk holds one of the
//! tenant's transfer slots ([`Uploads::transfer_slot`], the same pool as
//! an upload, `429 too_many_requests`) while its body is read and sent.
//!
//! **A chunk is not replayed**, the library's rule for a POST that is not
//! idempotent (a timeout may have taken effect): after a `504` or a
//! connection lost, ask [`Uploads::status`] for the offset Meta holds and
//! resume from it. A session that completed without Meta answering a
//! handle is `502 upstream`.
//!
//! The services open on a capability their own [`Authorizer`] made, and
//! admit it first. A failed Graph call is told to the [`Observer`], a
//! `190` marks the WABA's numbers `reconnect_required`, and Meta's
//! `details` are kept. The lines this module logs keep the target
//! `meta_whatsapp_server::api::media`.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::media::MediaKind;
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::ids::{AppId, UploadHandle, UploadSessionId};
use meta_whatsapp_rs::core::store::{Expiry, JsonStore, KvStore};
use serde::{Deserialize, Serialize};

use crate::authz::{Authorizer, OwnedNumber};
use crate::error::{ErrorCode, ServiceError};
use crate::media::filename;
use crate::observe::Observer;
use crate::ratelimit::{Slot, Slots};

/// The target of this module's log lines: the media routes'.
const LOG_TARGET: &str = "meta_whatsapp_server::api::media";

/// The `KvStore` namespace of the sessions' records: a stable identifier
/// once released, pending the owner's confirmation before the first
/// release.
pub const NAMESPACE: &str = "wa.server.upload";

/// How long a session's record is kept, from the session's start: 24
/// hours. Meta documents no lifetime for a session or for a handle; the
/// service's own is a day, which a resumed upload of a file within its
/// size limits needs, and after which the id is `404 not_found`. Pending
/// the owner's confirmation before the first release.
pub const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The longest file name sent to Meta.
const MAX_FILE_NAME_CHARS: usize = 128;

/// The file types the Resumable Upload API takes, as its guide lists them
/// ("Valid values are `application/pdf`, `image/jpeg`, `image/jpg`,
/// `image/png`, and `video/mp4`").
pub const FILE_TYPES: [&str; 5] = [
    "application/pdf",
    "image/jpeg",
    "image/jpg",
    "image/png",
    "video/mp4",
];

/// The most bytes a file of `file_type` may have, or `None` for a type
/// that is none of [`FILE_TYPES`]: its kind's limit in Meta's media table
/// (the library's [`MediaKind::max_bytes`]: 5 MiB for an image, 16 MiB
/// for a video, 100 MiB for a document), and never more than
/// `max_bytes` (`WA_SERVER_MEDIA_MAX_BYTES`). The Resumable Upload API
/// itself documents no limit.
pub fn file_limit(file_type: &str, max_bytes: u64) -> Option<u64> {
    if !FILE_TYPES.contains(&file_type) {
        return None;
    }
    // `image/jpg` is the guide's spelling of `image/jpeg`.
    let media_type = if file_type == "image/jpg" {
        "image/jpeg"
    } else {
        file_type
    };
    MediaKind::for_mime_type(media_type).map(|kind| kind.max_bytes().min(max_bytes))
}

// ─── A session's record ──────────────────────────────────────────────────

/// What the service keeps of a session. No `Debug`: it holds Meta's
/// session id and the handle.
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    tenant: String,
    phone_number_id: String,
    /// Meta's session id (`upload:…`).
    session: String,
    file_name: String,
    file_length: u64,
    file_type: String,
    /// Unix seconds.
    created_at: i64,
    /// The handle, once Meta answered it.
    handle: Option<String>,
}

/// The sessions' records, over the bundle's `KvStore` (a typed store on
/// the port, never a port of its own). Cheap to clone.
#[derive(Clone)]
pub struct UploadSessions {
    store: JsonStore<Record>,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for UploadSessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadSessions").finish_non_exhaustive()
    }
}

impl UploadSessions {
    /// The sessions' records in `kv`, dated by `clock`.
    pub fn new(kv: Arc<dyn KvStore>, clock: Arc<dyn Clock>) -> Self {
        Self {
            store: JsonStore::new(kv, NAMESPACE),
            clock,
        }
    }
}

/// A new upload id: `up_` and 32 hex characters (128 random bits).
fn new_id() -> Result<String, ServiceError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ServiceError::internal())?;
    Ok(format!("up_{}", hex::encode(bytes)))
}

/// Whether `id` has the shape of [`new_id`]'s.
fn is_upload_id(id: &str) -> bool {
    id.strip_prefix("up_")
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}

// ─── The requests and answers ────────────────────────────────────────────

/// A checked request to start a session. Only [`Self::new`] makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUpload {
    file_name: String,
    file_length: u64,
    file_type: String,
}

impl NewUpload {
    /// The file `file_name` (made inert as an upload's is,
    /// [`crate::media::filename`]) of `file_length` bytes and MIME type
    /// `file_type`, checked against [`FILE_TYPES`] and [`file_limit`]
    /// (`max_bytes` is `WA_SERVER_MEDIA_MAX_BYTES`).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `file_name` (blank), `file_type` (not one
    /// of [`FILE_TYPES`]) or `file_length` (zero); `413 media_too_large`
    /// (over [`file_limit`]).
    pub fn new(
        file_name: &str,
        file_length: u64,
        file_type: &str,
        max_bytes: u64,
    ) -> Result<Self, ServiceError> {
        if file_name.trim().is_empty() {
            return Err(ServiceError::invalid("file_name"));
        }
        let Some(limit) = file_limit(file_type, max_bytes) else {
            return Err(ServiceError::invalid("file_type"));
        };
        if file_length == 0 {
            return Err(ServiceError::invalid("file_length"));
        }
        if file_length > limit {
            return Err(ServiceError::new(ErrorCode::MEDIA_TOO_LARGE));
        }
        let cleaned: String = filename(Some(file_name), file_type)
            .chars()
            .take(MAX_FILE_NAME_CHARS)
            .collect();
        Ok(Self {
            file_name: cleaned,
            file_length,
            file_type: file_type.to_owned(),
        })
    }

    /// The file name sent to Meta.
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    /// The declared length.
    pub fn file_length(&self) -> u64 {
        self.file_length
    }

    /// The declared MIME type.
    pub fn file_type(&self) -> &str {
        &self.file_type
    }
}

/// A session's progress: what [`Uploads::start`], [`Uploads::chunk`] and
/// [`Uploads::status`] answer. `Debug` shows no handle.
#[derive(Clone, PartialEq, Eq)]
pub struct Progress {
    /// The service's id for the session.
    pub upload_id: String,
    /// The file name sent to Meta.
    pub file_name: String,
    /// The declared length.
    pub file_length: u64,
    /// The declared MIME type.
    pub file_type: String,
    /// The bytes Meta holds: where the next chunk starts.
    pub file_offset: u64,
    /// The handle, once the file is complete: what a template's
    /// `header_handle` and a profile picture's `profile_picture_handle`
    /// (roadmap M5c2) take.
    pub handle: Option<UploadHandle>,
}

impl Progress {
    /// Whether Meta has the whole file (the handle is answered).
    pub fn complete(&self) -> bool {
        self.handle.is_some()
    }
}

impl fmt::Debug for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Progress")
            .field("file_length", &self.file_length)
            .field("file_offset", &self.file_offset)
            .field("complete", &self.complete())
            .finish_non_exhaustive()
    }
}

/// A session of the number's, read from its record and not yet asked of
/// Meta: [`Uploads::open`] makes one. `Debug` shows no id.
pub struct OpenUpload {
    id: String,
    record: Record,
    version: u64,
}

impl fmt::Debug for OpenUpload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenUpload")
            .field("file_length", &self.record.file_length)
            .finish_non_exhaustive()
    }
}

impl OpenUpload {
    /// The most bytes a chunk starting at `file_offset` may have: up to
    /// the end of the declared file, so that no chunk passes it.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `file_offset` (not before the end of the
    /// declared file); on `upload_id` (the session is complete already: it
    /// takes no more chunks).
    pub fn max_chunk(&self, file_offset: u64) -> Result<u64, ServiceError> {
        if self.record.handle.is_some() {
            return Err(ServiceError::invalid("upload_id"));
        }
        self.record
            .file_length
            .checked_sub(file_offset)
            .filter(|rest| *rest > 0)
            .ok_or_else(|| ServiceError::invalid("file_offset"))
    }

    fn progress(&self, file_offset: u64, handle: Option<UploadHandle>) -> Progress {
        Progress {
            upload_id: self.id.clone(),
            file_name: self.record.file_name.clone(),
            file_length: self.record.file_length,
            file_type: self.record.file_type.clone(),
            file_offset,
            handle,
        }
    }
}

// ─── The operations ──────────────────────────────────────────────────────

/// The resumable upload operations on a number the caller's tenant owns.
/// Cheap: a few references.
pub struct Uploads<'a> {
    authz: &'a Authorizer,
    number: &'a OwnedNumber,
    sessions: &'a UploadSessions,
    app_id: Option<&'a AppId>,
    slots: &'a Slots,
    observer: &'a dyn Observer,
}

impl fmt::Debug for Uploads<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Uploads")
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> Uploads<'a> {
    /// The operations on `number`, once `authz` admits it, over
    /// `sessions`, for the Meta app `app_id` (`WA_APP_ID`; `None`, every
    /// operation that asks Meta for a session is `409
    /// resumable_upload_not_configured`), taking chunk slots of `slots`
    /// (the uploads' pool).
    ///
    /// # Errors
    ///
    /// `number` another [`Authorizer`] made: `403 forbidden`, and nothing
    /// is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        number: &'a OwnedNumber,
        sessions: &'a UploadSessions,
        app_id: Option<&'a AppId>,
        slots: &'a Slots,
    ) -> Result<Self, ServiceError> {
        authz.admit_number(number)?;
        Ok(Self {
            authz,
            number,
            sessions,
            app_id,
            slots,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// A slot of the number's tenant's share of the uploads' pool, for a
    /// chunk: taken before its body is read. Released when dropped.
    ///
    /// # Errors
    ///
    /// `429 too_many_requests` (`Retry-After: 1`).
    pub fn transfer_slot(&self) -> Result<Slot, ServiceError> {
        self.slots
            .try_acquire(self.number.tenant())
            .ok_or_else(|| ServiceError::too_many_requests(1))
    }

    /// The error of a failed Graph call, with Meta's `details`.
    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self.number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api.with_details(error)
    }

    /// The library's media API on the number, with its token.
    fn library(&self) -> meta_whatsapp_rs::client::media::Media {
        self.number
            .client()
            .media(self.number.phone_number_id().clone())
    }

    /// Open a session on Meta for `new`: the service's id for it.
    ///
    /// # Errors
    ///
    /// `409 resumable_upload_not_configured` (no app id), before any
    /// request; Meta's refusal or failure, with its `details` (a timeout
    /// `may_have_been_sent`: a session may exist that no record names, and
    /// expires unused); `502 upstream` (Meta's session id is none of the
    /// `upload:` ids the guide shows); `503 storage_unavailable` (the
    /// record, after Meta opened the session).
    pub async fn start(&self, new: &NewUpload) -> Result<Progress, ServiceError> {
        let Some(app_id) = self.app_id else {
            return Err(ServiceError::new(ErrorCode::RESUMABLE_UPLOAD_NOT_CONFIGURED));
        };
        let session = match self
            .library()
            .start_upload_session(app_id, &new.file_name, new.file_length, &new.file_type)
            .await
        {
            Ok(session) => session,
            Err(error) => return Err(self.failed(&error).await),
        };
        // An id that is not `upload:<id>` would aim the token at another
        // Graph object when used: refused before it is kept.
        if session
            .as_str()
            .split('?')
            .next()
            .and_then(|path| path.strip_prefix("upload:"))
            .is_none_or(str::is_empty)
        {
            tracing::warn!(target: LOG_TARGET, "Meta opened an upload session with an unexpected id");
            return Err(ServiceError::new(ErrorCode::UPSTREAM));
        }
        let id = new_id()?;
        let record = Record {
            tenant: self.number.tenant().as_str().to_owned(),
            phone_number_id: self.number.phone_number_id().as_str().to_owned(),
            session: session.as_str().to_owned(),
            file_name: new.file_name.clone(),
            file_length: new.file_length,
            file_type: new.file_type.clone(),
            created_at: self.sessions.clock.now().unix_timestamp(),
            handle: None,
        };
        let version = self
            .sessions
            .store
            .put_if_absent(&id, &record, Expiry::After(SESSION_TTL))
            .await?
            .ok_or_else(ServiceError::internal)?;
        Ok(OpenUpload {
            id,
            record,
            version,
        }
        .progress(0, None))
    }

    /// The session `id`, if it is this number's.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `upload_id` (not an id the service
    /// issues); `404 not_found` (no such record, an expired one, or one
    /// of another tenant or another number: all the same answer, with
    /// nothing asked of Meta); `503 storage_unavailable`.
    pub async fn open(&self, id: &str) -> Result<OpenUpload, ServiceError> {
        if !is_upload_id(id) {
            return Err(ServiceError::invalid("upload_id"));
        }
        let Some((record, version, _)) = self.sessions.store.get(id).await? else {
            return Err(ServiceError::not_found());
        };
        if record.tenant != self.number.tenant().as_str()
            || record.phone_number_id != self.number.phone_number_id().as_str()
        {
            tracing::info!(
                target: LOG_TARGET,
                "an upload session of another tenant or number: not found, Meta not asked"
            );
            return Err(ServiceError::not_found());
        }
        Ok(OpenUpload {
            id: id.to_owned(),
            record,
            version,
        })
    }

    /// Send `data` as the bytes of the file from `file_offset` on.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `file_offset` or `upload_id`
    /// ([`OpenUpload::max_chunk`]), on `body` (no bytes); `413
    /// media_too_large` (past the declared file); Meta's refusal or
    /// failure, with its `details` (a timeout `may_have_been_sent`: ask
    /// [`Self::status`]); `502 upstream` (the file is complete and Meta
    /// answered no handle).
    pub async fn chunk(
        &self,
        open: &OpenUpload,
        file_offset: u64,
        data: Bytes,
    ) -> Result<Progress, ServiceError> {
        let room = open.max_chunk(file_offset)?;
        let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
        if len == 0 {
            return Err(ServiceError::invalid("body"));
        }
        if len > room {
            return Err(ServiceError::new(ErrorCode::MEDIA_TOO_LARGE));
        }
        let session = UploadSessionId::new(open.record.session.as_str());
        let handle = match self
            .library()
            .upload_chunk(&session, file_offset, data)
            .await
        {
            Ok(handle) => handle,
            Err(error) => return Err(self.failed(&error).await),
        };
        let next = file_offset + len;
        let handle = match handle {
            Some(handle) => Some(handle),
            // The whole file is there and Meta gave no handle: a 2xx of
            // the wrong shape (the library's `resumable_upload` says the
            // same).
            None if next == open.record.file_length => {
                let error = Error::decode(
                    "upload response",
                    <serde_json::Error as serde::de::Error>::missing_field("h"),
                    b"",
                );
                return Err(ServiceError::from_library(&error));
            }
            None => None,
        };
        if let Some(handle) = &handle {
            self.keep_handle(open, handle).await;
        }
        Ok(open.progress(next, handle))
    }

    /// Keep the handle in the record (the session's expiry unchanged), so
    /// that [`Self::status`] answers it again. A failure is logged and not
    /// raised: the caller has the handle in this answer.
    async fn keep_handle(&self, open: &OpenUpload, handle: &UploadHandle) {
        let mut record = open.record.clone();
        record.handle = Some(handle.as_str().to_owned());
        let kept = self
            .sessions
            .store
            .compare_and_swap(&open.id, open.version, Some(&record), Expiry::Keep)
            .await;
        if !matches!(kept, Ok(Some(_))) {
            tracing::warn!(
                target: LOG_TARGET,
                "an upload's handle could not be kept: it is answered once"
            );
        }
    }

    /// Where the session stands: the handle if the service holds it (no
    /// request to Meta), else the offset Meta reports.
    ///
    /// # Errors
    ///
    /// `409 resumable_upload_not_configured` (no app id), before any
    /// request; Meta's refusal or failure, with its `details`.
    pub async fn status(&self, open: &OpenUpload) -> Result<Progress, ServiceError> {
        if let Some(handle) = &open.record.handle {
            return Ok(open.progress(
                open.record.file_length,
                Some(UploadHandle::new(handle.as_str())),
            ));
        }
        if self.app_id.is_none() {
            return Err(ServiceError::new(ErrorCode::RESUMABLE_UPLOAD_NOT_CONFIGURED));
        }
        let session = UploadSessionId::new(open.record.session.as_str());
        match self.library().upload_session_status(&session).await {
            Ok(offset) => Ok(open.progress(offset.min(open.record.file_length), None)),
            Err(error) => Err(self.failed(&error).await),
        }
    }
}
