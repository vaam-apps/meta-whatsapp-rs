//! SHA-256 verification of a streamed download.

use std::fmt;
use std::pin::Pin;
use std::task::{Context, Poll};

use base64::Engine as _;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use sha2::{Digest, Sha256};
use wa_core::error::{TransportError, ValidationError};
use wa_core::transport::ByteStream;
use wa_core::{Error, Result};

use super::MediaInfo;

/// A media download whose body is still streaming. Returned by
/// [`super::Media::download`].
///
/// The bytes are **unverified**. Call [`verified`](Self::verified) to check
/// them against the SHA-256 Meta reported.
pub struct MediaDownload {
    /// What `GET /{media-id}` said about the file.
    pub info: MediaInfo,
    /// Body chunks as they arrive.
    pub body: ByteStream,
}

impl fmt::Debug for MediaDownload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaDownload")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl MediaDownload {
    /// Hash the body while it streams and fail at the end if it does not
    /// match [`MediaInfo::sha256`] (see [`VerifiedBody`] for the error).
    ///
    /// Fails immediately, with a [`ValidationError`] on `sha256`, if the
    /// digest is not SHA-256 in hex (64 characters) or base64 (Meta's docs
    /// show both: hex in history and echo webhooks, base64 in media message
    /// webhooks): without a usable digest nothing can be verified.
    pub fn verified(self) -> Result<VerifiedDownload> {
        let expected = decode_sha256(&self.info.sha256).ok_or_else(|| {
            ValidationError::new("sha256", "is not a hex or base64 SHA-256 digest")
        })?;
        Ok(VerifiedDownload {
            info: self.info,
            body: VerifiedBody {
                inner: self.body,
                hasher: Sha256::new(),
                expected,
                done: false,
            },
        })
    }
}

/// A download being verified; see [`MediaDownload::verified`].
#[derive(Debug)]
pub struct VerifiedDownload {
    /// What `GET /{media-id}` said about the file.
    pub info: MediaInfo,
    /// Body chunks, then an error instead of the end if the hash is wrong.
    pub body: VerifiedBody,
}

/// The fully buffered, verified file. Returned by
/// [`super::Media::download_bytes`].
#[derive(Debug, Clone)]
pub struct DownloadedMedia {
    /// What `GET /{media-id}` said about the file.
    pub info: MediaInfo,
    /// The file.
    pub data: Bytes,
}

/// Upper bound on the buffer reserved up front from the *reported*
/// `file_size`. The report is not trusted for allocation: with a generous
/// `max_bytes`, a wrong or hostile size (a `MediaInfo` can be built from a
/// webhook) would otherwise ask the allocator for that many bytes and abort
/// the process. Past this the buffer grows as bytes actually arrive.
const MAX_PREALLOC: u64 = 16 * 1024 * 1024;

impl VerifiedDownload {
    /// Buffer the whole file, failing if it grows past `max_bytes` or its
    /// hash does not match.
    pub async fn collect(mut self, max_bytes: u64) -> Result<DownloadedMedia> {
        let hint = self
            .info
            .file_size
            .unwrap_or(0)
            .min(max_bytes)
            .min(MAX_PREALLOC);
        let mut buf = Vec::with_capacity(usize::try_from(hint).unwrap_or(0));
        while let Some(chunk) = self.body.next().await {
            let chunk = chunk?;
            let len = u64::try_from(buf.len() + chunk.len()).unwrap_or(u64::MAX);
            if len > max_bytes {
                return Err(too_large(len, max_bytes));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(DownloadedMedia {
            info: self.info,
            data: Bytes::from(buf),
        })
    }
}

pub(super) fn too_large(len: u64, max: u64) -> Error {
    ValidationError::new(
        "file_size",
        format!("media is at least {len} bytes; the limit given was {max} bytes"),
    )
    .into()
}

/// Stream adaptor behind [`VerifiedDownload::body`].
///
/// Yields the inner chunks unchanged while hashing them. At end of stream
/// it yields `Err(Error::Transport(TransportError::Integrity(..)))` instead
/// of ending when the digest differs — a transport failure, not bad input:
/// the bytes were damaged or substituted in flight, and a fresh download
/// (new URL from [`super::Media::url`]) may be intact, which is why
/// [`wa_core::Error::is_retryable`] says `true`. **Treat bytes as untrusted
/// until the stream has ended without error**: write them to a temporary
/// location and only publish them afterwards. Dropping the stream early
/// skips the check.
pub struct VerifiedBody {
    inner: ByteStream,
    hasher: Sha256,
    expected: [u8; 32],
    done: bool,
}

impl fmt::Debug for VerifiedBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedBody")
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl Stream for VerifiedBody {
    type Item = Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Every field is `Unpin` (`ByteStream` is a `Pin<Box<_>>`).
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(chunk))) => {
                this.hasher.update(&chunk);
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => {
                this.done = true;
                Poll::Ready(Some(Err(Error::Transport(e))))
            }
            Poll::Ready(None) => {
                this.done = true;
                let actual = this.hasher.finalize_reset();
                if actual.as_slice() == this.expected {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Err(Error::Transport(TransportError::Integrity(
                        "downloaded media does not match the SHA-256 Meta reported; discard it",
                    )))))
                }
            }
        }
    }
}

/// A 32-byte digest from 64 hex characters or standard base64.
fn decode_sha256(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    let bytes = if s.len() == 64 {
        hex::decode(s).ok()?
    } else {
        base64::engine::general_purpose::STANDARD.decode(s).ok()?
    };
    bytes.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_hex_and_base64_digests() {
        let hex = "3f9d94d399fa61c191bc1d4ca71375a035cd9b9f5b1128e1f0963a415c16b0cc";
        let raw = decode_sha256(hex).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        assert_eq!(decode_sha256(&b64), Some(raw));
        assert_eq!(decode_sha256(&hex.to_uppercase()), Some(raw));
        // The base64 example from webhooks/reference/messages/image.
        assert!(decode_sha256("SfInY0gGKTsJlUWbwxC1k+FAD0FZHvzwfpvO0zX0GUI=").is_some());
        assert_eq!(decode_sha256("PHOTO_HASH"), None);
        assert_eq!(decode_sha256(""), None);
        assert_eq!(decode_sha256(&hex[..62]), None);
    }
}
