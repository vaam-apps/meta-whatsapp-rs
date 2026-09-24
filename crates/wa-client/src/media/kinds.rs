//! Supported media types and size limits (`business-phone-numbers/media`,
//! "Supported media types"; the same tables appear on each
//! `messages/*-messages` page).

use wa_core::error::ValidationError;

/// Meta writes sizes as "5 MB" and "500 KB" without saying decimal or
/// binary. We read them as binary (MiB/KiB), the larger reading, so we never
/// reject a file Meta would accept; Meta remains the final judge.
const KB: u64 = 1024;
const MB: u64 = 1024 * KB;

/// What kind of message a MIME type can be sent as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MediaKind {
    /// AAC, AMR, MP3, MP4 audio, Ogg (OPUS only). 16 MB.
    Audio,
    /// Plain text, Excel, Word, `PowerPoint`, PDF. 100 MB.
    Document,
    /// JPEG, PNG (8-bit RGB/RGBA). 5 MB.
    Image,
    /// WebP. 500 KB animated, 100 KB static.
    Sticker,
    /// 3GPP, MP4 (H.264 + AAC). 16 MB.
    Video,
}

/// The documented MIME types. `image/webp` is a sticker: "WebP images can
/// only be sent in sticker messages".
const SUPPORTED: &[(&str, MediaKind)] = &[
    ("audio/aac", MediaKind::Audio),
    ("audio/amr", MediaKind::Audio),
    ("audio/mpeg", MediaKind::Audio),
    ("audio/mp4", MediaKind::Audio),
    ("audio/ogg", MediaKind::Audio),
    ("text/plain", MediaKind::Document),
    ("application/vnd.ms-excel", MediaKind::Document),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        MediaKind::Document,
    ),
    ("application/msword", MediaKind::Document),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        MediaKind::Document,
    ),
    ("application/vnd.ms-powerpoint", MediaKind::Document),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        MediaKind::Document,
    ),
    ("application/pdf", MediaKind::Document),
    ("image/jpeg", MediaKind::Image),
    ("image/png", MediaKind::Image),
    ("image/webp", MediaKind::Sticker),
    ("video/3gpp", MediaKind::Video),
    ("video/mp4", MediaKind::Video),
];

impl MediaKind {
    /// The kind a MIME type belongs to, if Meta documents it. Parameters
    /// (`audio/ogg; codecs=opus`) and case are ignored.
    pub fn for_mime_type(mime_type: &str) -> Option<Self> {
        let essence = mime_type.split(';').next().unwrap_or_default().trim();
        SUPPORTED
            .iter()
            .find(|(m, _)| m.eq_ignore_ascii_case(essence))
            .map(|(_, k)| *k)
    }

    /// Documented maximum size in bytes. For stickers this is the animated
    /// limit (500 KB): static stickers are capped at 100 KB, but the two
    /// share a MIME type and cannot be told apart without decoding.
    pub fn max_bytes(self) -> u64 {
        match self {
            Self::Audio | Self::Video => 16 * MB,
            Self::Document => 100 * MB,
            Self::Image => 5 * MB,
            Self::Sticker => 500 * KB,
        }
    }
}

/// Check `mime_type` and `len` against the documented table.
///
/// Note: `messages/document-messages` says other document types "may be
/// sent via the API" but are unsupported; this check follows the upload
/// page and rejects them. Use [`crate::Client::post`] with a
/// [`wa_core::transport::Multipart`] form if you must upload one anyway.
pub fn validate_upload(mime_type: &str, len: u64) -> Result<MediaKind, ValidationError> {
    let kind = MediaKind::for_mime_type(mime_type).ok_or_else(|| {
        ValidationError::new(
            "type",
            format!("`{mime_type}` is not a supported WhatsApp media type"),
        )
    })?;
    let max = kind.max_bytes();
    if len > max {
        return Err(ValidationError::new(
            "file",
            format!("is {len} bytes; the maximum for {kind:?} media is {max} bytes"),
        ));
    }
    Ok(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_type_maps_to_its_kind() {
        assert_eq!(
            MediaKind::for_mime_type("image/jpeg"),
            Some(MediaKind::Image)
        );
        assert_eq!(
            MediaKind::for_mime_type("IMAGE/PNG"),
            Some(MediaKind::Image)
        );
        assert_eq!(
            MediaKind::for_mime_type("audio/ogg; codecs=opus"),
            Some(MediaKind::Audio)
        );
        assert_eq!(
            MediaKind::for_mime_type("image/webp"),
            Some(MediaKind::Sticker)
        );
        assert_eq!(
            MediaKind::for_mime_type("application/pdf"),
            Some(MediaKind::Document)
        );
        assert_eq!(
            MediaKind::for_mime_type("video/3gpp"),
            Some(MediaKind::Video)
        );
        assert_eq!(
            SUPPORTED.len(),
            18,
            "5 audio + 8 document + 2 image + 1 sticker + 2 video"
        );
        assert_eq!(MediaKind::for_mime_type("image/gif"), None);
        assert_eq!(MediaKind::for_mime_type("image/jpg"), None);
        assert_eq!(MediaKind::for_mime_type(""), None);
    }

    #[test]
    fn size_limits_are_inclusive() {
        for (mime, max) in [
            ("image/png", 5 * MB),
            ("video/mp4", 16 * MB),
            ("audio/mpeg", 16 * MB),
            ("application/pdf", 100 * MB),
            ("image/webp", 500 * KB),
        ] {
            assert!(validate_upload(mime, max).is_ok(), "{mime} at limit");
            let err = validate_upload(mime, max + 1).unwrap_err();
            assert_eq!(err.field, "file", "{mime} over limit");
        }
        assert_eq!(validate_upload("image/gif", 1).unwrap_err().field, "type");
    }
}
