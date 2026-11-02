//! Media conversion over an `ffmpeg` the integrator installs (feature
//! `ffmpeg`): [`FfmpegConverter`], an implementation of core's
//! [`MediaConverter`] port.
//!
//! Nothing is linked or bundled. The converter starts the `ffmpeg`
//! binary as a child process, feeds it the input on stdin and reads the
//! result from stdout; no file is written. Install `ffmpeg` built with
//! `libopus` (voice) and `libwebp` (stickers, `libwebp` and
//! `libwebp_anim`) on the host (Debian and Ubuntu: `apt-get install
//! ffmpeg`, whose build has them), or point
//! [`FfmpegConverter::with_program`] at one, and call
//! [`FfmpegConverter::check`] (or [`check_for`](FfmpegConverter::check_for)
//! with the formats you convert to) at startup to fail early. The
//! service's public image carries no `ffmpeg`: a deployment that wants
//! conversion adds it, under the licence of the build it chooses.
//!
//! It makes five formats: [`TargetFormat::OggOpus`] from a recording
//! (below), [`TargetFormat::StaticSticker`], [`TargetFormat::Jpeg`]
//! and [`TargetFormat::Png`] from a PNG or a JPEG ([Images](#images)),
//! and [`TargetFormat::AnimatedSticker`] from a GIF
//! ([Animations](#animations)).
//!
//! # What it runs
//!
//! For [`TargetFormat::OggOpus`], always these arguments, none of them
//! taken from the caller:
//!
//! ```text
//! ffmpeg -hide_banner -nostdin -nostats -loglevel error
//!        -max_alloc 16777216 -abort_on empty_output
//!        -protocol_whitelist pipe
//!        -format_whitelist wav,mp3,ogg,flac,aac,mov,amr,matroska
//!        -i pipe:0
//!        -map 0:a:0 -map_metadata -1 -map_chapters -1
//!        -ac 1 -ar 48000 -c:a libopus -b:a 32k -application voip
//!        -f ogg pipe:1
//! ```
//!
//! - **The input is data, never a location.** It arrives on stdin, and
//!   `-protocol_whitelist pipe` stops a demuxer from opening anything
//!   else on its behalf (a playlist or a concat script naming a local
//!   file or an `http://` URL), so a crafted file cannot read the host's
//!   files or reach the network. Of the formats listed below, only MP4's
//!   data references (`dref`, an alias naming another file) open anything,
//!   and ffmpeg's `mov` reader follows none unless told to
//!   (`enable_drefs` and `use_absolute_path`, both off, and options the
//!   other readers would refuse, so they cannot be set here):
//!   `tests/ffmpeg.rs` in the facade feeds it such a file and checks that
//!   the file it names never reaches the result.
//! - **The environment is not passed on**: the process gets `PATH` alone
//!   (`FFREPORT` would write ffmpeg's full log, file names in it, to the
//!   working directory, which nothing else uses). A build that needs more
//!   (`LD_LIBRARY_PATH`) goes behind a wrapper script set with
//!   [`FfmpegConverter::with_program`], which must `exec` ffmpeg: a timeout
//!   kills the process started, not its children.
//! - **Only the audio formats a recording comes in are read**
//!   (`-format_whitelist`: WAV, MP3, Ogg, FLAC, AAC, MP4/M4A/3GP, AMR,
//!   Matroska and `WebM`). Anything else is refused before decoding.
//! - **The result is what Meta's table takes**: the first audio stream,
//!   mono (Meta: "mono input only"), Opus in Ogg; tags and chapters are
//!   dropped (they can carry personal data). At 32 kb/s a voice note
//!   stays under the 512 KB that keeps WhatsApp's play icon
//!   (`messages/audio-messages`) for about two minutes. The output is
//!   checked to be mono Ogg Opus before it is returned, and an input that
//!   decodes to no audio fails (`-abort_on empty_output`) rather than
//!   come back as an empty voice note.
//! - **A pipe cannot seek back**, so an MP4 or M4A whose index (`moov`)
//!   comes after its media data, as many recorders (and ffmpeg, by
//!   default) write it, could not be read from one. The converter moves
//!   the index first before it starts ffmpeg, as `-movflags +faststart`
//!   would (in memory, its chunk offsets shifted; anything it does not
//!   understand, such as a compressed index, is passed on as it is).
//!
//! # Images
//!
//! A sticker, a JPEG or a PNG is made from a **PNG or a JPEG** only, the
//! formats Meta's image table names. The input's first bytes are read
//! before `ffmpeg` starts (a PNG's signature, a JPEG's start of image),
//! and anything else (a GIF, a WebP, an SVG, a TIFF, a video) is
//! [`ConversionError::InputRejected`] without starting it; `ffmpeg` then
//! detects the format again itself and may read nothing but a single PNG
//! or JPEG (`-format_whitelist png_pipe,jpeg_pipe`): a JPEG-LS image or
//! an animated PNG, which begin as a JPEG and a PNG do, are refused
//! there. Before `ffmpeg` sees it, the image is walked, and what `ffmpeg`
//! is given of it is a memory bound, not a nicety: `ffmpeg` 6.1 keeps
//! some of an image's metadata many times over, each copy under the
//! allocation bound but not all of them together (measured against
//! ffmpeg 6.1.1):
//!
//! - **A PNG**, chunk by chunk: one whose chunks stop before its end
//!   chunk (a truncated file) is [`ConversionError::InputRejected`];
//!   `ffmpeg` gets it without its text and EXIF chunks (`tEXt`, `zTXt`,
//!   `iTXt`, `eXIf`), with its first colour profile chunk (`iCCP`) only
//!   if that is at most 16 KiB and no other, and without anything after
//!   its end chunk. `ffmpeg` inflates every `zTXt` chunk and keeps the
//!   text until the frame is decoded (four chunks of 100 KB, each
//!   inflating to 100 MiB, took 1.6 GB), and inflates a profile whole
//!   (one of 200 KB inflating to 200 MiB took 660 MB; a real one is a
//!   few kilobytes).
//! - **A JPEG**, segment by segment: one cut short before its end of
//!   image, with a segment whose length runs past the end (or is under
//!   two bytes), with a table, frame or scan header whose length is not
//!   its content (libjpeg refuses one too), or with a second start of
//!   image, is [`ConversionError::InputRejected`]. `ffmpeg` gets only
//!   what it reads whole: the quantization and Huffman tables, the frame
//!   header (baseline, extended, progressive or lossless; JPEG-LS's, for
//!   the format whitelist to refuse), the restart interval, the scans and
//!   their entropy-coded data, the first JFIF and Adobe segments
//!   rewritten to their fixed fields (no thumbnail), the first colour
//!   profile segment (`APP2`, `ICC_PROFILE`) when it holds a whole profile
//!   in one chunk (1 of 1) of at most 16 KiB, in place of the first EXIF
//!   segment a minimal EXIF of its Orientation alone (1 to 8; no EXIF
//!   without a valid one), and the end of image. Every other `APP1`
//!   (EXIF, XMP), every other `APP2` (so a profile split over several
//!   chunks is dropped whole, and that image loses its colour profile),
//!   `APP13`, comments, the other application segments, the markers `ffmpeg` does not decode,
//!   bytes between segments and anything after the end of image are
//!   dropped. `ffmpeg` copies an EXIF value once for every IFD entry
//!   pointing at it, over every `APP1` it reads (a 67 KB JPEG of one
//!   crafted segment took 916 MB, a 658 KB one of ten 8.4 GB), and moves
//!   on by the bytes it read rather than by a segment's length, so a
//!   segment it reads only in part could hide another from the walk:
//!   each segment it is given is one it reads whole.
//!
//! Each attempt runs these arguments, the filter and the encoder from a
//! fixed table:
//!
//! ```text
//! ffmpeg -hide_banner -nostdin -nostats -loglevel error
//!        -max_alloc 268435456 -abort_on empty_output
//!        -protocol_whitelist pipe -format_whitelist png_pipe,jpeg_pipe
//!        -max_pixels 33554432
//!        -i pipe:0
//!        -map 0:v:0 -map_metadata -1 -frames:v 1
//!        -vf <filter> <encoder> -f image2pipe pipe:1
//! ```
//!
//! - **Bounded in pixels**: an image of more than 32 Mi pixels
//!   (`-max_pixels`; 8192 × 4096, more than a 24-megapixel camera's) is
//!   refused before its frame is allocated, and no single allocation may
//!   exceed 256 MiB: one frame at that bound in 16-bit RGBA, less
//!   `ffmpeg`'s padding, so a 16-bit RGBA PNG of exactly 32 Mi pixels is
//!   refused and one a row smaller is not. A larger photo (a
//!   48-megapixel one) has to be scaled down first.
//! - **The first frame, nothing else**: no metadata (EXIF can carry a
//!   place and a camera owner's name). A JPEG's EXIF orientation is
//!   applied to the pixels before it is dropped (ffmpeg 6.1 rotates a
//!   frame by the minimal EXIF above), so a photo taken upright stays
//!   upright. The colour profile (ICC) is not metadata to `ffmpeg`: a
//!   JPEG or a PNG result keeps the input's, so that its colours stay
//!   what they were (a WebP result has none), when it is the one profile
//!   the walk above keeps, at most 16 KiB for a JPEG and a PNG alike (a
//!   JPEG's in a single chunk). An image whose profile is larger, or a
//!   JPEG's in several chunks, comes out without one, and a wide-gamut
//!   photo (Display P3) is then shown as sRGB.
//! - **Under the limit by a ladder**, since a size cannot be asked of an
//!   encoder: the attempts below run in turn until one's result is within
//!   [`TargetFormat::max_bytes`] (an attempt past it is killed as soon as
//!   it passes); the aspect ratio is kept and an image is never enlarged.
//!   If every attempt is over, the conversion is
//!   [`Error::Validation`] on `input` (with these ladders a real image
//!   never gets there: the last attempt of each is far under its limit).
//!   Meta gives no sticker dimensions, so none is imposed: a sticker that
//!   fits at its own size keeps it. A large photo spends most of a
//!   sticker's time on the first attempt, at its own size (about 18 s
//!   for a 24-megapixel one on a 4-core machine, whose WebP is then far
//!   over the limit): keep the timeout at its default, or above it on a
//!   slower host. An image more than 256 times as long as it is wide
//!   (or wide as it is long) can fail at a later, smaller attempt (a
//!   side scaled to nothing) as [`ConversionError::InputRejected`].
//!
//! | Format | Encoder | Attempts (longest side, quality) |
//! | --- | --- | --- |
//! | [`TargetFormat::StaticSticker`] (100 000 bytes) | `libwebp`, lossy, transparency kept | its own (up to 16383, WebP's limit) at 80; 1024 at 80, then 50; 512 at 75, then 50; 256 at 50 |
//! | [`TargetFormat::Jpeg`] (5 000 000 bytes) | `mjpeg`, 8-bit 4:2:0 | its own at `-q:v` 2, 5, then 10; 2048 at 5; 1024 at 10 |
//! | [`TargetFormat::Png`] (5 000 000 bytes) | `png`, 8-bit RGB or RGBA (a 16-bit, grey or palette PNG becomes one) | its own; 2048; 1024 |
//!
//! The output is checked before it is returned: a WebP file whose RIFF
//! size is its length and whose image is not animated; a JPEG of 8 bits
//! and three components; a PNG of 8-bit RGB or RGBA, one image whose
//! chunks end with its end chunk and the file.
//!
//! # Animations
//!
//! An animated sticker is made from a **GIF** only: of the animations
//! ffmpeg 6.1 might read, an animated PNG cannot be read from a pipe (its
//! reader seeks) and an animated WebP cannot be decoded at all (its WebP
//! decoder skips the frames). The input's first bytes are read before
//! `ffmpeg` starts (`GIF87a` or `GIF89a`: anything else, a PNG, a JPEG or
//! a WebP among them, is [`ConversionError::InputRejected`] without
//! starting it), and `ffmpeg`'s own detection may land on nothing but
//! `gif` (`-format_whitelist gif`). The GIF is walked block by block
//! first, and refused ([`ConversionError::InputRejected`], before
//! `ffmpeg` starts) when it is cut short of its trailer, has a block GIF
//! does not define, or is out of bounds:
//!
//! - **pixels**: a canvas of more than 2 Mi pixels (2 097 152: 2048 ×
//!   1024, or 1920 × 1080), which `-max_pixels` refuses again;
//! - **frames**: fewer than two (no animation) or more than 500
//!   (`-frames:v 500` stops `ffmpeg` at as many again);
//! - **frames times pixels**: more than 100 frames of a canvas of
//!   512 × 512 or larger, each frame counted for its canvas's pixels up
//!   to 512 × 512 (25 Mi, 26 214 400, in all: 400 frames of 256 × 256),
//!   since an attempt costs its frames times the pixels it encodes and
//!   every attempt decodes every frame again;
//! - **time**: longer than 10 s (WhatsApp's own rule for an animated
//!   sticker), each frame counted for its delay, or a tenth of a second
//!   when it gives none or one under two hundredths (as browsers and
//!   ffmpeg do): a lower frame rate repeats a slow frame to fill its
//!   time, so the length bounds what the later attempts encode.
//!
//! `ffmpeg` gets the GIF without its comments, plain text and
//! application extensions (a loop count, XMP, an ICC profile) and without
//! anything after its trailer: only its header, colour tables, images
//! and, before each image, the last graphic control extension given for
//! it (its delay, transparency and disposal). None of them is compressed,
//! so unlike a PNG's text none can inflate, but `ffmpeg` 6.1 holds them
//! all in memory with the first frame (60 MB of comments took it 162 MB,
//! where the same GIF without them takes 51 MB; 64 MB of graphic control
//! extensions in a row, 169 MB), and they carry personal data. Each
//! attempt runs these arguments, the filter and the encoder from a fixed
//! table:
//!
//! ```text
//! ffmpeg -hide_banner -nostdin -nostats -loglevel error
//!        -max_alloc 268435456 -abort_on empty_output
//!        -protocol_whitelist pipe -format_whitelist gif
//!        -max_pixels 2097152
//!        -i pipe:0
//!        -map 0:v:0 -map_metadata -1 -fps_mode vfr -frames:v 500
//!        -vf <filter> <encoder> -f image2pipe pipe:1
//! ```
//!
//! Each frame keeps its own time (`-fps_mode vfr`) until an attempt
//! lowers the frame rate. `libwebp_anim` hands over the whole file at the
//! end, so an attempt over the limit is found then; a single allocation
//! may be 256 MiB, for that one packet (26 MB for 100 frames of
//! 512 × 512 that compress badly; a smaller bound fails the attempt as
//! out of memory instead of trying the next). The result loops forever
//! (libwebp's default; a GIF's own loop count is not kept).
//!
//! | Attempt | Largest side | Frame rate | Quality (`libwebp_anim`, lossy, level 4) |
//! | --- | --- | --- | --- |
//! | 1 | 512 | the GIF's own | 75 |
//! | 2 | 512 | 15 a second | 50 |
//! | 3 | 384 | 12 | 40 |
//! | 4 | 256 | 10 | 40 |
//! | 5 | 192 | 8 | 30 |
//! | 6 | 128 | 5 | 25 |
//!
//! The aspect ratio is kept, an animation is never enlarged and its
//! transparency is kept. Meta gives no dimensions, frame rate or length
//! for an animated sticker: 512 pixels is where the ladder starts, so
//! that an attempt's cost (its frames times its pixels) stays within the
//! timeout. Frames that are all alike come out of `libwebp` as a still
//! image, which is [`ConversionError::InputRejected`] (make a
//! [`TargetFormat::StaticSticker`] of a PNG or a JPEG instead). The output
//! is checked before it is returned: a WebP file whose RIFF size is its
//! length, its chunks ending with it, the first an extended header
//! (`VP8X`) with the animation flag, an `ANIM` chunk before the frames, at
//! least two `ANMF` chunks (one is a still image labelled animated), and
//! no image outside them. Converting an animation is slower than an
//! image, each attempt decoding and encoding every frame again (on a
//! 4-core machine, against ffmpeg 6.1.1): the worst measured within the
//! bounds, 100 frames of 1024 × 1024 noise with noisy transparency, took
//! 44 s for the six attempts, none of them within the limit, and 136 MB
//! at the first; 100 frames of 2048 × 1024 noise 36 s; a 5 s, 480 × 360
//! GIF of 100 busy frames 3.7 s, two attempts. Before the frame-pixel
//! bound, 500 frames of 512 × 512 noise took 59 s over 10 s of play, and
//! 66 s over 30 s.
//! Keep the timeout at its default, or above it on a slower host.
//!
//! # Bounds and errors
//!
//! | Bound or failure | Default | Error |
//! | --- | --- | --- |
//! | an empty input, or one over [`with_max_input_bytes`](FfmpegConverter::with_max_input_bytes) | 64 MiB | [`Error::Validation`] on `input`, before `ffmpeg` starts |
//! | a result over the format's limit ([`TargetFormat::max_bytes`]: 16 MiB for audio) | — | [`Error::Validation`] on `input`; `ffmpeg` is killed (for an image or an animation, the next attempt runs, and only the last one's overflow is this error) |
//! | `ffmpeg` missing or not executable | — | [`ConversionError::BinaryMissing`] |
//! | no `libopus`, `libwebp`, `libwebp_anim`, `mjpeg` or `png` encoder ([`FfmpegConverter::check_for`]) | — | [`ConversionError::EncoderMissing`] |
//! | an image whose first bytes are not a PNG's or a JPEG's; a PNG whose chunks stop before its end chunk; a JPEG that stops before its end of image, whose segment's length runs past the end or is not its header's content, or with a second start of image | — | [`ConversionError::InputRejected`], before `ffmpeg` starts |
//! | an image's metadata (a PNG's text and EXIF chunks, a JPEG's application segments and comments, any colour profile but one of at most 16 KiB, a JPEG's in one chunk; [Images](#images)) | — | not passed to `ffmpeg`: a JPEG's EXIF becomes its Orientation alone |
//! | an animation whose first bytes are not a GIF's; a GIF cut short of its trailer, with a block GIF does not define, of a canvas over 2 Mi pixels, of fewer than 2 frames or more than 500, of more than 100 frames of 512 × 512 or more (frames times pixels), or longer than 10 s | — | [`ConversionError::InputRejected`], before `ffmpeg` starts |
//! | an animation whose frames are all alike (`libwebp` makes a still image of it) | — | [`ConversionError::InputRejected`] |
//! | `ffmpeg` fails (input it cannot read, a format off the list, an image over 32 Mi pixels) | — | [`ConversionError::InputRejected`] |
//! | the output is not the format asked for (a binary that is not the `ffmpeg` it should be) | — | [`ConversionError::OutputInvalid`] |
//! | one allocation in `ffmpeg` over 16 MiB for audio (`-max_alloc`: a header declaring an absurd sample rate), 256 MiB for an image or an animation | — | [`ConversionError::InputRejected`]; on an `ffmpeg` whose allocations stay under the bound, the result cap above stops it instead |
//! | a conversion over [`with_timeout`](FfmpegConverter::with_timeout), every attempt of an image's or an animation's ladder together | 60 s | [`ConversionError::Timeout`], the one retryable; `ffmpeg` is killed |
//! | `ffmpeg` killed by a signal, or a pipe to it broken | — | [`ConversionError::Killed`] |
//!
//! Every [`ConversionError`] is [`Error::Conversion`]: never
//! `may_have_been_sent` (a conversion sends nothing to Meta).
//!
//! `ffmpeg`'s diagnostics (stderr) are read and discarded, never put in
//! an error or a log: they name files and URLs. An error says what
//! failed (the exit status, a bound) and nothing of the input. Dropping
//! a conversion in flight kills its process. Conversions are not
//! queued: bound how many run at once yourself (each one is a process
//! using about a CPU core, for up to the timeout, and memory this crate
//! does not cap beyond `-max_alloc`'s per allocation, and an image at the
//! pixel bound takes several frames of it, an animation some 170 MB at
//! the worst measured (padding in one frame's data): a cgroup, or `ulimit -v` in a wrapper script,
//! bounds it). Run it on a tokio runtime with I/O enabled (what
//! `#[tokio::main]` builds): child processes need tokio's I/O driver.
//!
//! ```no_run
//! # async fn demo(wav: Vec<u8>, png: Vec<u8>) -> meta_whatsapp_core::Result<()> {
//! use std::time::Duration;
//! use meta_whatsapp_adapters::media::FfmpegConverter;
//! use meta_whatsapp_core::media::{MediaConverter, MediaInput, TargetFormat};
//!
//! // A sticker from a large photo takes a while: more than the default.
//! let converter = FfmpegConverter::new().with_timeout(Duration::from_secs(90));
//! // At startup: a missing ffmpeg, libopus or libwebp fails here.
//! converter
//!     .check_for(&[TargetFormat::OggOpus, TargetFormat::StaticSticker])
//!     .await?;
//! let note = converter
//!     .convert(MediaInput::new(wav), TargetFormat::OggOpus)
//!     .await?;
//! assert_eq!(note.format.mime_type(), "audio/ogg");
//! let sticker = converter
//!     .convert(MediaInput::new(png), TargetFormat::StaticSticker)
//!     .await?;
//! assert_eq!(sticker.format.mime_type(), "image/webp");
//! assert!(sticker.data.len() <= 100_000);
//! # Ok(()) }
//! ```

mod animation;
mod image;
mod mp4;
#[cfg(test)]
mod tests;

use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;
use meta_whatsapp_core::error::{ConversionError, ValidationError};
use meta_whatsapp_core::media::{ConvertedMedia, MediaConverter, MediaInput, TargetFormat};
use meta_whatsapp_core::{Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// The demuxers `ffmpeg` may use on an input: the audio formats
/// recordings come in. `mov` reads MP4, M4A and 3GP; `matroska` reads
/// `WebM` (a browser's `MediaRecorder`).
const INPUT_FORMATS: &str = "wav,mp3,ogg,flac,aac,mov,amr,matroska";

/// The largest single allocation `ffmpeg` may make (`-max_alloc`), in
/// bytes: 16 MiB.
const MAX_ALLOC: &str = "16777216";

/// The arguments for [`TargetFormat::OggOpus`], in order. Every one is a
/// literal: nothing of the caller's reaches the command line.
const OGG_OPUS_ARGS: &[&str] = &[
    "-hide_banner",
    "-nostdin",
    "-nostats",
    "-loglevel",
    "error",
    // No single allocation over 16 MiB: a crafted header (a WAV declaring
    // one sample a second, which becomes 48 000 at the output rate)
    // otherwise makes each small input frame a buffer of hundreds of
    // megabytes. A recording at any real rate stays far below it.
    "-max_alloc",
    MAX_ALLOC,
    // An input that decodes to no audio (no samples, or samples ffmpeg
    // cannot reach from a pipe) is an error, not an empty voice note.
    "-abort_on",
    "empty_output",
    // Input options: read stdin only, and only these formats.
    "-protocol_whitelist",
    "pipe",
    "-format_whitelist",
    INPUT_FORMATS,
    "-i",
    "pipe:0",
    // The first audio stream, without tags or chapters.
    "-map",
    "0:a:0",
    "-map_metadata",
    "-1",
    "-map_chapters",
    "-1",
    // Meta: Ogg with Opus only, mono only.
    "-ac",
    "1",
    "-ar",
    "48000",
    "-c:a",
    "libopus",
    "-b:a",
    "32k",
    "-application",
    "voip",
    "-f",
    "ogg",
    "pipe:1",
];

/// The only variables of this process's environment the child gets:
/// `PATH`, to find a bare program name (and, on Windows, `SystemRoot`,
/// without which a process cannot start there).
const INHERITED_ENV: &[&str] = if cfg!(windows) {
    &["PATH", "SystemRoot"]
} else {
    &["PATH"]
};

/// `ffmpeg -encoders`, for [`FfmpegConverter::check`].
const ENCODERS_ARGS: &[&str] = &[
    "-hide_banner",
    "-nostdin",
    "-loglevel",
    "error",
    "-encoders",
];

/// The most of `ffmpeg -encoders` read (a full build lists some 30 KB).
const ENCODERS_CAP: u64 = 1024 * 1024;

/// The arguments that make `target` from audio, or `None` for a format
/// this converter does not make from audio (an image's are
/// [`image::ladder`]).
fn arguments(target: TargetFormat) -> Option<&'static [&'static str]> {
    match target {
        TargetFormat::OggOpus => Some(OGG_OPUS_ARGS),
        _ => None,
    }
}

/// The encoder a format needs, as `ffmpeg -encoders` lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Encoder {
    /// The first letter of its flags: `A` audio, `V` video (and images).
    kind: char,
    /// Its name.
    name: &'static str,
    /// What it makes, in words, for errors.
    makes: &'static str,
}

/// The encoder `target` needs, or `None` for a format this converter
/// does not make.
fn encoder(target: TargetFormat) -> Option<Encoder> {
    let (kind, name, makes) = match target {
        TargetFormat::OggOpus => ('A', "libopus", "mono Ogg Opus"),
        TargetFormat::StaticSticker => ('V', "libwebp", "a static WebP sticker"),
        TargetFormat::AnimatedSticker => ('V', "libwebp_anim", "an animated WebP sticker"),
        TargetFormat::Jpeg => ('V', "mjpeg", "an 8-bit colour JPEG"),
        TargetFormat::Png => ('V', "png", "an 8-bit RGB or RGBA PNG"),
        _ => return None,
    };
    Some(Encoder { kind, name, makes })
}

/// A format this converter does not make: a [`ValidationError`] on
/// `target`.
fn unsupported(target: TargetFormat) -> Error {
    ValidationError::new(
        "target",
        format!("{target:?} is not a format FfmpegConverter produces"),
    )
    .into()
}

/// A [`MediaConverter`] that runs an installed `ffmpeg` (see the
/// [module docs](self)).
#[derive(Debug, Clone)]
pub struct FfmpegConverter {
    program: PathBuf,
    timeout: Duration,
    max_input_bytes: u64,
}

impl Default for FfmpegConverter {
    fn default() -> Self {
        Self::new()
    }
}

/// How one run of the program ended.
enum Run {
    /// It exited (or was stopped by a signal) with this output.
    Done(Vec<u8>, ExitStatus),
    /// Its output passed the bound; it was killed.
    Overflow,
}

/// Why a run stopped before the program exited.
enum Stop {
    /// Its output passed the bound.
    Overflow,
    /// A pipe to it, or waiting for it, failed.
    Fail(Error),
}

impl FfmpegConverter {
    /// How long one conversion may take by default.
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

    /// The largest input taken by default: 64 MiB (about six minutes of
    /// CD-quality WAV).
    pub const DEFAULT_MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

    /// A converter running `ffmpeg` from `PATH`, with the default bounds.
    pub fn new() -> Self {
        Self {
            program: PathBuf::from("ffmpeg"),
            timeout: Self::DEFAULT_TIMEOUT,
            max_input_bytes: Self::DEFAULT_MAX_INPUT_BYTES,
        }
    }

    /// Run this binary instead of `ffmpeg` from `PATH` (an absolute path
    /// is best: `PATH` is the process's environment). It is your
    /// configuration: never take it from a request.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// The longest one conversion may take, writing the input and reading
    /// the result included, and for an image or an animation every attempt
    /// of its ladder together; past it the process is killed
    /// ([`DEFAULT_TIMEOUT`](Self::DEFAULT_TIMEOUT) unless set).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The largest input taken, in bytes; a larger one is refused
    /// before `ffmpeg` starts
    /// ([`DEFAULT_MAX_INPUT_BYTES`](Self::DEFAULT_MAX_INPUT_BYTES) unless
    /// set).
    #[must_use]
    pub fn with_max_input_bytes(mut self, max: u64) -> Self {
        self.max_input_bytes = max;
        self
    }

    /// The binary this converter runs.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Check that the binary starts and has the `libopus` encoder, with
    /// `ffmpeg -encoders`: [`check_for`](Self::check_for) with
    /// [`TargetFormat::OggOpus`]. Call it (or `check_for` with the formats
    /// you convert to) at startup, so that a missing or unsuitable
    /// `ffmpeg` is found before the first conversion.
    ///
    /// # Errors
    ///
    /// As [`check_for`](Self::check_for).
    pub async fn check(&self) -> Result<()> {
        self.check_for(&[TargetFormat::OggOpus]).await
    }

    /// Check that the binary starts and has the encoder each of `targets`
    /// needs (`libopus` for [`TargetFormat::OggOpus`], `libwebp` for
    /// [`TargetFormat::StaticSticker`], `libwebp_anim` for
    /// [`TargetFormat::AnimatedSticker`], `mjpeg` for [`TargetFormat::Jpeg`],
    /// `png` for [`TargetFormat::Png`]), with `ffmpeg -encoders`. Decoders
    /// are not checked: every `ffmpeg` build but a stripped-down one reads
    /// the input formats.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] on `target` for a format this converter does
    /// not make, before anything starts;
    /// [`ConversionError::BinaryMissing`] when the binary cannot be
    /// started, [`ConversionError::OutputInvalid`] when it does not list
    /// its encoders as ffmpeg does, [`ConversionError::EncoderMissing`]
    /// when it lists no encoder a target needs,
    /// [`ConversionError::Timeout`] past the timeout.
    pub async fn check_for(&self, targets: &[TargetFormat]) -> Result<()> {
        let mut needed = Vec::with_capacity(targets.len());
        for &target in targets {
            needed.push(encoder(target).ok_or_else(|| unsupported(target))?);
        }
        let not_ffmpeg = || {
            ConversionError::OutputInvalid(format!(
                "`{}` listed more than {ENCODERS_CAP} bytes of encoders: is it ffmpeg?",
                self.program.display()
            ))
        };
        let (out, status) = match self
            .run(ENCODERS_ARGS, None, ENCODERS_CAP, self.timeout)
            .await?
        {
            Run::Done(out, status) => (out, status),
            Run::Overflow => return Err(not_ffmpeg().into()),
        };
        if !status.success() {
            return Err(ConversionError::OutputInvalid(format!(
                "`{}` did not list its encoders ({}): is it ffmpeg?",
                self.program.display(),
                describe(status)
            ))
            .into());
        }
        let listing = String::from_utf8_lossy(&out);
        for wanted in needed {
            let listed = listing.lines().any(|line| {
                let mut words = line.split_whitespace();
                matches!(
                    (words.next(), words.next()),
                    (Some(flags), Some(name)) if name == wanted.name && flags.starts_with(wanted.kind)
                )
            });
            if !listed {
                return Err(ConversionError::EncoderMissing(format!(
                    "`{}` has no {} encoder: {} needs an ffmpeg with it",
                    self.program.display(),
                    wanted.name,
                    wanted.makes
                ))
                .into());
            }
        }
        Ok(())
    }

    /// Start the program with `args`, feed it `input` (stdin is closed
    /// when there is none), read at most `max_out` bytes of its stdout
    /// ([`Run::Overflow`] past them) and discard its stderr, all within
    /// `within` (a [`ConversionError::Timeout`] of the configured timeout
    /// past it). On a timeout, an overflow or an error the process is
    /// killed.
    async fn run(
        &self,
        args: &[&str],
        input: Option<Bytes>,
        max_out: u64,
        within: Duration,
    ) -> Result<Run> {
        let mut command = Command::new(&self.program);
        // Nothing of this process's environment but `PATH`: ffmpeg reads
        // some (`FFREPORT` writes its full log, file names included, to a
        // file in the working directory; `AV_LOG_FORCE_*`, proxies).
        command.env_clear();
        for name in INHERITED_ENV {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        let mut child = command
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| spawn_error(&self.program, &e))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let work = async {
            let feed = async move {
                if let (Some(mut pipe), Some(data)) = (stdin, input) {
                    // ffmpeg stops reading when it gives up on the input
                    // and exits: its exit status says why, not the broken
                    // pipe this write then gets.
                    let _ = pipe.write_all(&data).await;
                    let _ = pipe.shutdown().await;
                }
                Ok::<(), Stop>(())
            };
            let collect = async move {
                match stdout {
                    Some(out) => read_bounded(out, max_out).await,
                    None => Ok(Vec::new()),
                }
            };
            // Read to the end so that ffmpeg never blocks on a full pipe,
            // and keep none of it: diagnostics name files and URLs.
            let drain = async move {
                if let Some(mut err) = stderr {
                    let _ = tokio::io::copy(&mut err, &mut tokio::io::sink()).await;
                }
                Ok::<(), Stop>(())
            };
            let ((), out, ()) = futures::try_join!(feed, collect, drain)?;
            let status = child
                .wait()
                .await
                .map_err(|e| Stop::Fail(process_error("wait for", &e)))?;
            Ok::<_, Stop>((out, status))
        };
        let outcome = tokio::time::timeout(within, work).await;
        match outcome {
            Ok(Ok((out, status))) => Ok(Run::Done(out, status)),
            Ok(Err(stop)) => {
                let _ = child.kill().await;
                match stop {
                    Stop::Overflow => Ok(Run::Overflow),
                    Stop::Fail(e) => Err(e),
                }
            }
            Err(_elapsed) => {
                let _ = child.kill().await;
                tracing::warn!(
                    timeout_ms = u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
                    "ffmpeg ran past its timeout and was killed"
                );
                Err(ConversionError::Timeout(self.timeout).into())
            }
        }
    }

    /// A recording into `target` (Ogg Opus), in one run of `args`.
    async fn convert_audio(
        &self,
        data: Bytes,
        target: TargetFormat,
        args: &'static [&'static str],
    ) -> Result<ConvertedMedia> {
        let max = target.max_bytes();
        // An MP4 whose index follows its media data cannot be read from a
        // pipe: put it first (`mp4`), leaving anything else as it is.
        let data = mp4::index_first(&data).map_or(data, Bytes::from);
        let (out, status) = match self.run(args, Some(data), max, self.timeout).await? {
            Run::Done(out, status) => (out, status),
            Run::Overflow => return Err(too_large(target, None)),
        };
        if !status.success() {
            return Err(failed(target, status));
        }
        self.checked(target, out)
    }

    /// An image (or, for [`TargetFormat::AnimatedSticker`], an animation)
    /// into `target`, trying each rung of `ladder` in turn until one's
    /// result is within the format's limit, all within the timeout.
    async fn convert_image(
        &self,
        data: Bytes,
        target: TargetFormat,
        ladder: &'static [image::Rung],
    ) -> Result<ConvertedMedia> {
        let animated = target == TargetFormat::AnimatedSticker;
        let (accepted, from) = if animated {
            (animation::accepted(&data), "a GIF's, the animations")
        } else {
            (image::accepted(&data), "a PNG's or a JPEG's, the images")
        };
        if !accepted {
            return Err(ConversionError::InputRejected(format!(
                "its first bytes are not {from} FfmpegConverter makes {target:?} from"
            ))
            .into());
        }
        // A PNG without its text, EXIF and large colour profile; a JPEG
        // with only the segments ffmpeg reads whole, its EXIF reduced to
        // the orientation; nothing after either's end. A GIF without its
        // comments and application extensions, nothing after its trailer,
        // and within the animation's bounds.
        let prepared = if animated {
            animation::prepared(&data)
        } else {
            image::prepared(&data)
        };
        let data = prepared.map_err(|why| {
            ConversionError::InputRejected(format!("{why}: not converted to {target:?}"))
        })?;
        let max = target.max_bytes();
        // `None` for a timeout too long to add to now: no deadline sooner.
        let deadline = Instant::now().checked_add(self.timeout);
        for rung in ladder {
            let left = deadline.map_or(self.timeout, |d| {
                d.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                return Err(ConversionError::Timeout(self.timeout).into());
            }
            let args = if animated {
                animation::arguments(rung)
            } else {
                image::arguments(rung)
            };
            match self.run(&args, Some(data.clone()), max, left).await? {
                // Too large: the next rung is smaller.
                Run::Overflow => {}
                Run::Done(out, status) => {
                    if !status.success() {
                        return Err(failed(target, status));
                    }
                    // libwebp makes a still image of frames that are all
                    // alike: the input's doing, not the program's.
                    if animated && check_output(TargetFormat::StaticSticker, &out).is_ok() {
                        return Err(ConversionError::InputRejected(format!(
                            "its frames are all alike, and libwebp made a still image of them: not converted to {target:?}"
                        ))
                        .into());
                    }
                    return self.checked(target, out);
                }
            }
        }
        Err(too_large(target, Some(ladder.len())))
    }

    /// `out` as `target`, once its header shows it is one.
    fn checked(&self, target: TargetFormat, out: Vec<u8>) -> Result<ConvertedMedia> {
        check_output(target, &out).map_err(|why| {
            let (makes, with) =
                encoder(target).map_or(("the format asked for", "ffmpeg"), |e| (e.makes, e.name));
            ConversionError::OutputInvalid(format!(
                "`{}` returned {why}, not {makes}: is it ffmpeg with {with}?",
                self.program.display()
            ))
        })?;
        Ok(ConvertedMedia::new(out, target))
    }
}

#[async_trait]
impl MediaConverter for FfmpegConverter {
    async fn convert(&self, input: MediaInput, target: TargetFormat) -> Result<ConvertedMedia> {
        let audio = arguments(target);
        let ladder = image::ladder(target).or_else(|| animation::ladder(target));
        if audio.is_none() && ladder.is_none() {
            return Err(unsupported(target));
        }
        let len = u64::try_from(input.data.len()).unwrap_or(u64::MAX);
        if len == 0 {
            return Err(ValidationError::new("input", "is empty").into());
        }
        if len > self.max_input_bytes {
            return Err(ValidationError::new(
                "input",
                format!(
                    "is {len} bytes; this converter takes at most {} bytes",
                    self.max_input_bytes
                ),
            )
            .into());
        }
        match (audio, ladder) {
            (Some(args), _) => self.convert_audio(input.data, target, args).await,
            (None, Some(ladder)) => self.convert_image(input.data, target, ladder).await,
            (None, None) => Err(unsupported(target)),
        }
    }
}

/// A result over `target`'s limit: after one run (`attempts` `None`) or
/// after every attempt of an image's ladder.
fn too_large(target: TargetFormat, attempts: Option<usize>) -> Error {
    let max = target.max_bytes();
    let message = match attempts {
        None => format!(
            "converted to {target:?}, it exceeds {max} bytes, the most an upload of it takes"
        ),
        Some(n) => format!(
            "converted to {target:?} at each of the {n} sizes and qualities FfmpegConverter tries, it still exceeds {max} bytes"
        ),
    };
    ValidationError::new("input", message).into()
}

/// `ffmpeg` exited without converting: the input (status code) or a
/// signal (none).
fn failed(target: TargetFormat, status: ExitStatus) -> Error {
    match status.code() {
        Some(_) => ConversionError::InputRejected(format!(
            "ffmpeg could not convert it to {target:?} ({}); its diagnostics are withheld",
            describe(status)
        ))
        .into(),
        None => ConversionError::Killed(format!("ffmpeg was {}", describe(status))).into(),
    }
}

/// Read `reader` to its end, or stop with [`Stop::Overflow`] once it has
/// given more than `max` bytes.
async fn read_bounded<R: AsyncRead + Unpin>(
    reader: R,
    max: u64,
) -> std::result::Result<Vec<u8>, Stop> {
    let mut out = Vec::new();
    reader
        .take(max.saturating_add(1))
        .read_to_end(&mut out)
        .await
        .map_err(|e| Stop::Fail(process_error("read the output of", &e)))?;
    if u64::try_from(out.len()).unwrap_or(u64::MAX) > max {
        return Err(Stop::Overflow);
    }
    Ok(out)
}

/// A failure to start the program. Missing or not executable is the
/// deployment's to fix; anything else (out of processes, say) stopped the
/// conversion before it began.
fn spawn_error(program: &Path, e: &io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::NotFound => ConversionError::BinaryMissing(format!(
            "ffmpeg not found: `{}` could not be started; install ffmpeg, or point FfmpegConverter::with_program at it",
            program.display()
        ))
        .into(),
        io::ErrorKind::PermissionDenied => ConversionError::BinaryMissing(format!(
            "ffmpeg could not be started: `{}` is not executable",
            program.display()
        ))
        .into(),
        _ => ConversionError::Killed(format!("could not start ffmpeg: {e}")).into(),
    }
}

/// A pipe to the process, or waiting for it, failed: the conversion
/// stopped. An I/O error's text names no file of the input.
fn process_error(what: &str, e: &io::Error) -> Error {
    ConversionError::Killed(format!("could not {what} ffmpeg: {e}")).into()
}

/// An exit status in words, without the program's output.
fn describe(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit status {code}"),
        None => "killed by a signal".to_owned(),
    }
}

/// Check that `out` is `target`: for Ogg Opus, an Ogg stream whose first
/// page opens it and carries an `OpusHead` for one channel (RFC 7845
/// § 5.1); for an image, its header ([`image::check`]); for an animated
/// sticker, its chunks ([`animation::check`]). Returns what it found
/// instead, in words.
fn check_output(target: TargetFormat, out: &[u8]) -> std::result::Result<(), &'static str> {
    match target {
        TargetFormat::OggOpus => {
            let head = first_ogg_packet(out).ok_or("no Ogg stream")?;
            if !head.starts_with(b"OpusHead") {
                return Err("an Ogg stream that is not Opus");
            }
            match head.get(9) {
                Some(1) => Ok(()),
                Some(_) => Err("Opus with more than one channel"),
                None => Err("a truncated Opus header"),
            }
        }
        TargetFormat::AnimatedSticker => animation::check(out),
        _ => image::check(target, out),
    }
}

/// The first packet of an Ogg stream: the payload of its first page, which
/// must begin the stream (RFC 3533 § 6). `None` when `out` is not one.
fn first_ogg_packet(out: &[u8]) -> Option<&[u8]> {
    const HEADER: usize = 27;
    if out.get(..4)? != b"OggS" || *out.get(4)? != 0 || out.get(5)? & 0x02 == 0 {
        return None;
    }
    let segments = usize::from(*out.get(26)?);
    let table = out.get(HEADER..HEADER + segments)?;
    let len: usize = table.iter().map(|&b| usize::from(b)).sum();
    let start = HEADER + segments;
    out.get(start..start + len)
}
