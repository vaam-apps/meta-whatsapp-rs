//! The media conversion port: [`MediaConverter`].
//!
//! Meta converts nothing. Its supported-types table
//! (`business-phone-numbers/media`) takes Ogg audio only with the Opus
//! codec and mono input, and a **voice message** must be exactly that:
//! `messages/audio-messages` says voice messages "require .ogg files
//! encoded with the OPUS codec", and that the play icon shows only for a
//! file of 512 KB or less (a download icon otherwise). A recording made
//! by a browser or a phone (WAV, `WebM`, M4A, MP3) has to be converted
//! first, on the integrator's side.
//!
//! Like the other ports, this is a trait and does nothing itself: no
//! codec is linked anywhere. `meta-whatsapp-adapters` implements it
//! behind its `ffmpeg` feature (off by default), with an `ffmpeg` the
//! integrator installs; a conversion service of your own plugs in the
//! same way. `meta-whatsapp-client` re-exports it from its `media` module,
//! where `Media::upload_converted` uploads the result.

use std::fmt;

use async_trait::async_trait;
use bytes::Bytes;

use crate::Result;

/// The most Meta takes for an audio file: "16 MB" in its table
/// (`business-phone-numbers/media`), read as 16 MiB, the larger reading,
/// as `meta-whatsapp-client`'s upload check reads it (its tests keep the
/// two equal).
pub const AUDIO_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// The format a [`MediaConverter`] produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TargetFormat {
    /// Ogg container, Opus codec, one channel: what Meta's table accepts
    /// as `audio/ogg` ("OPUS codecs only; base audio/ogg not supported;
    /// mono input only"), and what a voice message must be
    /// (`messages/audio-messages`): send it as an audio message with
    /// `voice` set.
    OggOpus,
}

impl TargetFormat {
    /// The MIME type to upload it with: the one Meta's table names,
    /// `audio/ogg`.
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::OggOpus => "audio/ogg",
        }
    }

    /// The file name extension Meta's table gives it (`ogg`), without
    /// the dot. Meta's media page warns that a file name extension that
    /// does not match the type is a common cause of error `131053`.
    pub fn extension(self) -> &'static str {
        match self {
            Self::OggOpus => "ogg",
        }
    }

    /// The largest result an upload accepts ([`AUDIO_MAX_BYTES`] for
    /// audio). A converter refuses a result above it rather than return
    /// one the upload would refuse.
    pub fn max_bytes(self) -> u64 {
        match self {
            Self::OggOpus => AUDIO_MAX_BYTES,
        }
    }
}

/// The media to convert: the file's bytes, in whatever format the
/// converter can read. A converter detects the format from the bytes
/// and refuses one it does not read; nothing the caller sends becomes a
/// converter option.
///
/// `Debug` shows the length only: media is personal data.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MediaInput {
    /// The file.
    pub data: Bytes,
}

impl MediaInput {
    /// Media to convert.
    pub fn new(data: impl Into<Bytes>) -> Self {
        Self { data: data.into() }
    }
}

impl fmt::Debug for MediaInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaInput")
            .field("len", &self.data.len())
            .finish()
    }
}

/// What a [`MediaConverter`] returns: the file in [`format`](Self::format).
///
/// `Debug` shows the length and the format only.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConvertedMedia {
    /// The converted file.
    pub data: Bytes,
    /// Its format: upload it as [`TargetFormat::mime_type`].
    pub format: TargetFormat,
}

impl ConvertedMedia {
    /// A converted file (for implementations of [`MediaConverter`]).
    pub fn new(data: impl Into<Bytes>, format: TargetFormat) -> Self {
        Self {
            data: data.into(),
            format,
        }
    }
}

impl fmt::Debug for ConvertedMedia {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConvertedMedia")
            .field("len", &self.data.len())
            .field("format", &self.format)
            .finish()
    }
}

/// Converts media into a [`TargetFormat`] before an upload.
///
/// Implementations do the work outside this crate (a process, a
/// service); their own library types never cross this trait. What they
/// promise:
///
/// - **The result is the format asked for**, at most
///   [`TargetFormat::max_bytes`], or an error: never another format
///   passed off as it.
/// - **The caller's bytes are data, never instructions**: no part of the
///   input chooses a program, an option, a file or a URL the converter
///   reads.
/// - **Errors are typed.** An empty input, or one too large (before or
///   after conversion), is [`Error::Validation`](crate::Error::Validation)
///   on field `input`; everything else is
///   [`Error::Conversion`](crate::Error::Conversion), a
///   [`ConversionError`](crate::error::ConversionError) (a missing
///   program or encoder, a timeout, input it cannot read, output of the
///   wrong format, a process killed). Neither ever panics, and an error
///   message never carries the converter's own diagnostics, which can
///   name files on its host.
/// - **The work is bounded** in input size and run time, and a converter
///   that runs past its time is stopped.
///
/// Conversion sends nothing to Meta: its errors are never
/// [`may_have_been_sent`](crate::Error::may_have_been_sent).
#[async_trait]
pub trait MediaConverter: Send + Sync + fmt::Debug + 'static {
    /// Convert `input` into `target`.
    async fn convert(&self, input: MediaInput, target: TargetFormat) -> Result<ConvertedMedia>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ogg_opus_is_audio_ogg_up_to_sixteen_mebibytes() {
        let t = TargetFormat::OggOpus;
        assert_eq!(t.mime_type(), "audio/ogg");
        assert_eq!(t.extension(), "ogg");
        assert_eq!(t.max_bytes(), 16 * 1024 * 1024);
        assert_eq!(AUDIO_MAX_BYTES, 16_777_216);
    }

    #[test]
    fn debug_shows_lengths_not_bytes() {
        let input = MediaInput::new(&b"RIFF secret recording"[..]);
        assert_eq!(format!("{input:?}"), "MediaInput { len: 21 }");
        let out = ConvertedMedia::new(&b"OggS secret"[..], TargetFormat::OggOpus);
        assert_eq!(
            format!("{out:?}"),
            "ConvertedMedia { len: 11, format: OggOpus }"
        );
    }

    #[test]
    fn the_trait_is_object_safe() {
        #[derive(Debug)]
        struct Echo;
        #[async_trait]
        impl MediaConverter for Echo {
            async fn convert(
                &self,
                input: MediaInput,
                target: TargetFormat,
            ) -> Result<ConvertedMedia> {
                Ok(ConvertedMedia::new(input.data, target))
            }
        }
        let c: std::sync::Arc<dyn MediaConverter> = std::sync::Arc::new(Echo);
        let out = futures::executor::block_on(
            c.convert(MediaInput::new(vec![1u8]), TargetFormat::OggOpus),
        )
        .unwrap();
        assert_eq!(&out.data[..], &[1u8]);
    }
}
