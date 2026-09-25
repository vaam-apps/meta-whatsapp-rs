//! Non-interactive message contents: text, media, location, reaction, pin.

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use meta_whatsapp_core::ids::MessageId;

use super::validate::{self, Check};

/// `text` object (`messages/text-messages`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Text {
    /// Body text; URLs in it are hyperlinked by the client.
    pub body: String,
    /// Ask the client to render a preview of the first URL in `body`.
    /// Direct Send ignores it (`direct-send/supported-message-types`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<bool>,
}

impl Text {
    /// Text without a link preview preference.
    pub fn new(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            preview_url: None,
        }
    }

    /// Set `preview_url`.
    #[must_use]
    pub fn preview_url(mut self, preview: bool) -> Self {
        self.preview_url = Some(preview);
        self
    }

    pub(crate) fn validate(&self, max_body: usize) -> Check {
        // messages/text-messages: body required, maximum 4096 characters
        // (1024 under Direct Send, direct-send/supported-features-and-limits;
        // the caller passes the applicable maximum).
        validate::text("text.body", &self.body, max_body)
    }
}

/// Where a media asset comes from; shared with templates (see
/// [`crate::common`]).
pub use crate::common::MediaSource;

/// Caption limit shared by image, video and document messages
/// (`messages/image-messages`, `messages/video-messages`,
/// `messages/document-messages`: "Maximum 1024 characters").
const CAPTION_MAX: usize = 1024;

/// `image` object (`messages/image-messages`). JPEG or PNG, 5 MB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Image {
    /// The asset.
    #[serde(flatten)]
    pub source: MediaSource,
    /// Caption.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl Image {
    /// Image from `source`, without caption.
    pub fn new(source: impl Into<MediaSource>) -> Self {
        Self {
            source: source.into(),
            caption: None,
        }
    }

    /// Set the caption.
    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    pub(crate) fn validate(&self) -> Check {
        self.source.validate("image")?;
        validate::opt_text("image.caption", self.caption.as_deref(), CAPTION_MAX)
    }
}

/// `video` object (`messages/video-messages`). MP4 or 3GPP (H.264 + AAC),
/// 16 MB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Video {
    /// The asset.
    #[serde(flatten)]
    pub source: MediaSource,
    /// Caption.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl Video {
    /// Video from `source`, without caption.
    pub fn new(source: impl Into<MediaSource>) -> Self {
        Self {
            source: source.into(),
            caption: None,
        }
    }

    /// Set the caption.
    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    pub(crate) fn validate(&self) -> Check {
        self.source.validate("video")?;
        validate::opt_text("video.caption", self.caption.as_deref(), CAPTION_MAX)
    }
}

/// `audio` object (`messages/audio-messages`). Audio messages take no
/// caption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Audio {
    /// The asset.
    #[serde(flatten)]
    pub source: MediaSource,
    /// `true` sends a voice message (requires an Ogg/OPUS file); omitted or
    /// `false` sends a basic audio message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<bool>,
}

impl Audio {
    /// Basic audio message.
    pub fn new(source: impl Into<MediaSource>) -> Self {
        Self {
            source: source.into(),
            voice: None,
        }
    }

    /// Mark as a voice message (`"voice": true`).
    #[must_use]
    pub fn voice(mut self) -> Self {
        self.voice = Some(true);
        self
    }

    pub(crate) fn validate(&self) -> Check {
        self.source.validate("audio")
    }
}

/// `document` object (`messages/document-messages`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Document {
    /// The asset.
    #[serde(flatten)]
    pub source: MediaSource,
    /// Caption.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// File name with extension; the client picks the icon from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

impl Document {
    /// Document from `source`, without caption or file name.
    pub fn new(source: impl Into<MediaSource>) -> Self {
        Self {
            source: source.into(),
            caption: None,
            filename: None,
        }
    }

    /// Set the caption.
    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    /// Set the file name shown to the user.
    #[must_use]
    pub fn filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    pub(crate) fn validate(&self) -> Check {
        self.source.validate("document")?;
        validate::opt_text("document.caption", self.caption.as_deref(), CAPTION_MAX)
    }
}

/// `sticker` object (`messages/sticker-messages`). WebP only: 500 KB
/// animated, 100 KB static.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Sticker {
    /// The asset.
    #[serde(flatten)]
    pub source: MediaSource,
}

impl Sticker {
    /// Sticker from `source`.
    pub fn new(source: impl Into<MediaSource>) -> Self {
        Self {
            source: source.into(),
        }
    }

    pub(crate) fn validate(&self) -> Check {
        self.source.validate("sticker")
    }
}

/// `location` object (`messages/location-messages`).
///
/// Coordinates are `f64` here but serialize as JSON strings: the docs type
/// them as strings and every example quotes them.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    /// Latitude in decimal degrees.
    pub latitude: f64,
    /// Longitude in decimal degrees.
    pub longitude: f64,
    /// Place name.
    pub name: Option<String>,
    /// Street address.
    pub address: Option<String>,
}

impl Location {
    /// A point without name or address.
    pub fn new(latitude: f64, longitude: f64) -> Self {
        Self {
            latitude,
            longitude,
            name: None,
            address: None,
        }
    }

    /// Set the place name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the address.
    #[must_use]
    pub fn address(mut self, address: impl Into<String>) -> Self {
        self.address = Some(address.into());
        self
    }

    pub(crate) fn validate(&self) -> Check {
        // "Decimal degrees" (messages/location-messages) only has meaning in
        // these ranges. The range test also rejects NaN and infinities,
        // which would otherwise serialize as the strings "NaN"/"inf".
        if !(-90.0..=90.0).contains(&self.latitude) {
            return Err(meta_whatsapp_core::error::ValidationError::new(
                "location.latitude",
                "must be a finite number of decimal degrees in -90..=90",
            ));
        }
        if !(-180.0..=180.0).contains(&self.longitude) {
            return Err(meta_whatsapp_core::error::ValidationError::new(
                "location.longitude",
                "must be a finite number of decimal degrees in -180..=180",
            ));
        }
        Ok(())
    }
}

impl Serialize for Location {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        // `f64`'s Display is the shortest string that round-trips, so the
        // docs' `37.44216251868683` comes out unchanged.
        map.serialize_entry("latitude", &self.latitude.to_string())?;
        map.serialize_entry("longitude", &self.longitude.to_string())?;
        if let Some(name) = &self.name {
            map.serialize_entry("name", name)?;
        }
        if let Some(address) = &self.address {
            map.serialize_entry("address", address)?;
        }
        map.end()
    }
}

/// `reaction` object (`messages/reaction-messages`).
///
/// Meta only emits a `sent` status for reactions (no delivered/read), and a
/// reaction cannot be sent as a contextual reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reaction {
    /// The user message to react to (at most 30 days old, not itself a
    /// reaction; otherwise Meta reports `131009` by webhook).
    pub message_id: MessageId,
    /// The emoji, as the character itself or its escape sequence.
    ///
    /// An empty string removes an earlier reaction. The current send docs
    /// do not describe removal; this follows the Cloud API's established
    /// behaviour and is therefore not validated either way.
    pub emoji: String,
}

impl Reaction {
    /// React to `message_id` with `emoji`.
    pub fn new(message_id: impl Into<MessageId>, emoji: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            emoji: emoji.into(),
        }
    }

    pub(crate) fn validate(&self) -> Check {
        validate::non_empty("reaction.message_id", self.message_id.as_str())
    }
}

/// `pin` object: pin or unpin a message in a group (`groups/groups-messaging`).
/// Only group admins can pin; at most 3 messages are pinned at a time and
/// the oldest is unpinned automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    /// Pin `message_id` for `expiration_days` (1–30).
    Pin {
        /// Message to pin.
        message_id: MessageId,
        /// Pin duration in days.
        expiration_days: u32,
    },
    /// Unpin `message_id`.
    Unpin {
        /// Message to unpin.
        message_id: MessageId,
    },
}

impl Pin {
    fn message_id(&self) -> &MessageId {
        match self {
            Self::Pin { message_id, .. } | Self::Unpin { message_id } => message_id,
        }
    }

    pub(crate) fn validate(&self) -> Check {
        validate::non_empty("pin.message_id", self.message_id().as_str())?;
        if let Self::Pin {
            expiration_days, ..
        } = self
        {
            // groups/groups-messaging: "Pin duration in days. Can be 1 to 30
            // days."
            if !(1..=30).contains(expiration_days) {
                return Err(meta_whatsapp_core::error::ValidationError::new(
                    "pin.expiration_days",
                    format!("is {expiration_days}; the documented range is 1..=30"),
                ));
            }
        }
        Ok(())
    }
}

impl Serialize for Pin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        match self {
            Self::Pin {
                message_id,
                expiration_days,
            } => {
                map.serialize_entry("type", "pin")?;
                map.serialize_entry("message_id", message_id)?;
                // The parameter table types this as Integer; the syntax
                // block quotes its placeholder. We follow the table.
                map.serialize_entry("expiration_days", expiration_days)?;
            }
            Self::Unpin { message_id } => {
                map.serialize_entry("type", "unpin")?;
                map.serialize_entry("message_id", message_id)?;
            }
        }
        map.end()
    }
}
