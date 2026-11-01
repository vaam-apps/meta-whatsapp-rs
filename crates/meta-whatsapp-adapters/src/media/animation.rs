//! Animations into an animated WebP sticker: the fixed arguments of each
//! attempt (a ladder of sizes, frame rates and qualities), the walk of a
//! GIF before `ffmpeg` reads it, and the check of the output (see the
//! [module docs](super)).

use std::ops::Range;

use bytes::Bytes;
use meta_whatsapp_core::media::TargetFormat;

use super::image::{self, Rung};

/// The demuxer `ffmpeg` may use on an animation: GIF's (`gif`; from a
/// pipe, ffmpeg 6.1 detects a GIF as it, never as `gif_pipe`). An
/// animated PNG (`apng`) and an animated WebP are not on it: ffmpeg 6.1
/// cannot read the first from a pipe (its reader seeks) and cannot decode
/// the second at all (its WebP decoder skips `ANIM` and `ANMF`).
pub(super) const INPUT_FORMATS: &str = "gif";

/// The most pixels an animation's canvas may have: 2 Mi (2 097 152), e.g.
/// 2048 × 1024, or 1920 × 1080. Every frame is decoded onto the whole
/// canvas, so decoding costs its pixels times its frames.
pub(super) const MAX_PIXELS: u32 = 2_097_152;

/// [`MAX_PIXELS`], as `-max_pixels` takes it.
pub(super) const MAX_PIXELS_ARG: &str = "2097152";

/// The most frames an animation may have: 500 (ten seconds at 50 frames
/// a second, a GIF's fastest common rate). A GIF of more is refused before
/// `ffmpeg` starts; ffmpeg's own `-frames:v` stops at as many, a second
/// wall.
pub(super) const MAX_FRAMES: usize = 500;

/// [`MAX_FRAMES`], as `-frames:v` takes it.
pub(super) const MAX_FRAMES_ARG: &str = "500";

/// The longest an animation may play, in hundredths of a second (a GIF's
/// unit): 30 s. A lower frame rate (`fps`) repeats a frame to fill its
/// time, so the length bounds what each attempt encodes.
pub(super) const MAX_DURATION_CS: u64 = 3000;

/// What a GIF frame shows for when it gives no delay, or one under
/// [`MIN_DELAY_CS`]: 10 hundredths, as browsers do and ffmpeg's default
/// (`default_delay`). Counted this way, a GIF's length is never shorter
/// than ffmpeg's reading of it.
const DEFAULT_DELAY_CS: u64 = 10;

/// The shortest delay counted as given (ffmpeg's `min_delay`).
const MIN_DELAY_CS: u16 = 2;

/// The largest single allocation `ffmpeg` may make on an animation
/// (`-max_alloc`), in bytes: 256 MiB. `libwebp_anim` hands over the whole
/// animated file as one packet when it ends: up to some 110 MB for 500
/// frames of 512 × 512 that compress badly (about 220 KB each), which a
/// smaller bound would refuse as out of memory instead of trying the next,
/// smaller attempt.
pub(super) const MAX_ALLOC: &str = "268435456";

/// Everything up to the filter, the same for every attempt: the bounds,
/// the input options (before `-i`), the first video stream without
/// metadata, each frame kept for its own time (`-fps_mode vfr`), and no
/// more than [`MAX_FRAMES`] of them.
pub(super) const INPUT_ARGS: &[&str] = &[
    "-hide_banner",
    "-nostdin",
    "-nostats",
    "-loglevel",
    "error",
    "-max_alloc",
    MAX_ALLOC,
    "-abort_on",
    "empty_output",
    // Input options: read stdin only, a GIF only, and no canvas over the
    // pixel bound.
    "-protocol_whitelist",
    "pipe",
    "-format_whitelist",
    INPUT_FORMATS,
    "-max_pixels",
    MAX_PIXELS_ARG,
    "-i",
    "pipe:0",
    "-map",
    "0:v:0",
    "-map_metadata",
    "-1",
    "-fps_mode",
    "vfr",
    "-frames:v",
    MAX_FRAMES_ARG,
];

const ANIM_75: &[&str] = &[
    "-c:v",
    "libwebp_anim",
    "-lossless",
    "0",
    "-compression_level",
    "4",
    "-quality",
    "75",
];
const ANIM_50: &[&str] = &[
    "-c:v",
    "libwebp_anim",
    "-lossless",
    "0",
    "-compression_level",
    "4",
    "-quality",
    "50",
];
const ANIM_40: &[&str] = &[
    "-c:v",
    "libwebp_anim",
    "-lossless",
    "0",
    "-compression_level",
    "4",
    "-quality",
    "40",
];
const ANIM_30: &[&str] = &[
    "-c:v",
    "libwebp_anim",
    "-lossless",
    "0",
    "-compression_level",
    "4",
    "-quality",
    "30",
];
const ANIM_25: &[&str] = &[
    "-c:v",
    "libwebp_anim",
    "-lossless",
    "0",
    "-compression_level",
    "4",
    "-quality",
    "25",
];

/// An animated sticker: lossy animated WebP (`libwebp_anim`, compression
/// level 4, libwebp's default: level 6 took eight times as long for 3 %
/// less), transparency kept. The first attempt keeps the frame rate and
/// fits the animation within a square of 512 pixels; the next ones lower
/// the frame rate (`fps`: 15, 12, 10, 8, then 5 a second), the size (512,
/// 384, 256, 192, then 128) and the quality (75, 50, 40, 40, 30, 25). The
/// aspect ratio is kept and an animation never enlarged. Lowering the
/// frame rate drops frames when the animation is faster and repeats them
/// when it is slower (libwebp merges a repeated frame into the one before
/// it).
pub(super) const LADDER: &[Rung] = &[
    Rung {
        filter: "scale=w='min(iw,512)':h='min(ih,512)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: ANIM_75,
    },
    Rung {
        filter: "fps=15,scale=w='min(iw,512)':h='min(ih,512)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: ANIM_50,
    },
    Rung {
        filter: "fps=12,scale=w='min(iw,384)':h='min(ih,384)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: ANIM_40,
    },
    Rung {
        filter: "fps=10,scale=w='min(iw,256)':h='min(ih,256)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: ANIM_40,
    },
    Rung {
        filter: "fps=8,scale=w='min(iw,192)':h='min(ih,192)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: ANIM_30,
    },
    Rung {
        filter: "fps=5,scale=w='min(iw,128)':h='min(ih,128)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: ANIM_25,
    },
];

/// The attempts that make `target`, in order, or `None` for a format that
/// is not an animation.
pub(super) fn ladder(target: TargetFormat) -> Option<&'static [Rung]> {
    match target {
        TargetFormat::AnimatedSticker => Some(LADDER),
        _ => None,
    }
}

/// The whole command line of one attempt: literals only.
pub(super) fn arguments(rung: &Rung) -> Vec<&'static str> {
    let mut args = Vec::with_capacity(INPUT_ARGS.len() + 2 + rung.encoder.len() + 3);
    args.extend_from_slice(INPUT_ARGS);
    args.push("-vf");
    args.push(rung.filter);
    args.extend_from_slice(rung.encoder);
    args.extend_from_slice(image::OUTPUT_ARGS);
    args
}

/// Whether `data` begins as a GIF does (`GIF87a` or `GIF89a`). What
/// `ffmpeg` then detects must also be [`INPUT_FORMATS`].
pub(super) fn accepted(data: &[u8]) -> bool {
    data.starts_with(b"GIF89a") || data.starts_with(b"GIF87a")
}

/// What a truncated GIF is, in words.
const GIF_TRUNCATED: &str = "a GIF whose blocks stop before its trailer (a truncated file)";

/// The extension kept: the graphic control extension (`0xF9`), which
/// carries a frame's delay, transparency and disposal, the last one
/// before each image only (`ffmpeg` 6.1 held 64 MB of them, given in a
/// row, as one 169 MB packet). Every other one is taken out: comments
/// (`0xFE`), plain text (`0x01`) and application extensions (`0xFF`: the
/// loop count, XMP, an ICC profile).
const GRAPHIC_CONTROL: u8 = 0xF9;

/// What `ffmpeg` is given of an accepted GIF: walked block by block from
/// its header to its trailer (`0x3B`), with its header, colour tables,
/// images and, before each image, the last graphic control extension
/// given for it (the one that counts, for `ffmpeg` as for a browser),
/// without any other extension and without anything after the trailer;
/// the same bytes uncopied when there is nothing to take out. Refused,
/// in words: a GIF cut short of its trailer, a block it does not know, a
/// graphic control extension of the wrong size, a canvas of no pixels or
/// over [`MAX_PIXELS`], fewer than two frames (no animation), more than
/// [`MAX_FRAMES`], or longer than
/// [`MAX_DURATION_CS`] (each frame for its delay, or
/// [`DEFAULT_DELAY_CS`] when it gives none or one under
/// [`MIN_DELAY_CS`]).
pub(super) fn prepared(data: &Bytes) -> Result<Bytes, &'static str> {
    // Header (6), logical screen descriptor (7): width, height, flags.
    let screen = data.get(6..13).ok_or(GIF_TRUNCATED)?;
    let width = u32::from(u16::from_le_bytes([screen[0], screen[1]]));
    let height = u32::from(u16::from_le_bytes([screen[2], screen[3]]));
    if width == 0 || height == 0 {
        return Err("a GIF whose canvas has no pixels");
    }
    if width * height > MAX_PIXELS {
        return Err("a GIF whose canvas is over 2097152 pixels (2 Mi)");
    }
    let mut at = 13 + colour_table(screen[4]);
    if at > data.len() {
        return Err(GIF_TRUNCATED);
    }
    let mut kept: Vec<Range<usize>> = Vec::with_capacity(8);
    kept.push(0..at);
    let mut frames = 0usize;
    let mut duration = 0u64;
    // The last graphic control extension since the last image, and the
    // delay it gives: kept, and counted, only when an image follows.
    let mut control: Option<(Range<usize>, u16)> = None;
    loop {
        match data.get(at) {
            // Extension: its label, then sub-blocks.
            Some(0x21) => {
                let label = *data.get(at + 1).ok_or(GIF_TRUNCATED)?;
                let end = sub_blocks(data, at + 2)?;
                if label == GRAPHIC_CONTROL {
                    // One sub-block of four bytes: flags, delay, colour.
                    let block = data.get(at + 2..at + 7).ok_or(GIF_TRUNCATED)?;
                    if block[0] != 4 || end != at + 8 {
                        return Err("a GIF graphic control extension of the wrong size");
                    }
                    control = Some((at..end, u16::from_le_bytes([block[2], block[3]])));
                }
                at = end;
            }
            // Image: its descriptor, its colour table, the LZW code size,
            // then sub-blocks.
            Some(0x2C) => {
                let descriptor = data.get(at + 1..at + 10).ok_or(GIF_TRUNCATED)?;
                let code_size = at + 10 + colour_table(descriptor[8]);
                data.get(code_size).ok_or(GIF_TRUNCATED)?;
                let end = sub_blocks(data, code_size + 1)?;
                let delay = match control.take() {
                    Some((range, delay)) => {
                        image::keep(&mut kept, range);
                        Some(delay)
                    }
                    None => None,
                };
                image::keep(&mut kept, at..end);
                frames += 1;
                if frames > MAX_FRAMES {
                    return Err("an animation of more than 500 frames");
                }
                duration += match delay {
                    Some(given) if given >= MIN_DELAY_CS => u64::from(given),
                    _ => DEFAULT_DELAY_CS,
                };
                if duration > MAX_DURATION_CS {
                    return Err("an animation longer than 30 seconds");
                }
                at = end;
            }
            Some(0x3B) => {
                image::keep(&mut kept, at..at + 1);
                break;
            }
            Some(_) => {
                return Err("a GIF block that is not an extension, an image or its trailer");
            }
            None => return Err(GIF_TRUNCATED),
        }
    }
    if frames < 2 {
        return Err("a GIF of fewer than two frames (no animation)");
    }
    Ok(image::gathered(data, &kept))
}

/// The length of the colour table a GIF's flags byte announces (screen or
/// image): `3 × 2^(n + 1)` bytes when its top bit is set, none otherwise.
fn colour_table(flags: u8) -> usize {
    if flags & 0x80 == 0 {
        0
    } else {
        3 << ((flags & 0x07) + 1)
    }
}

/// The end of a run of GIF sub-blocks starting at `at` (each a length
/// byte and as many bytes, the run ended by a zero length): the offset
/// after its terminator.
fn sub_blocks(data: &[u8], mut at: usize) -> Result<usize, &'static str> {
    loop {
        let len = usize::from(*data.get(at).ok_or(GIF_TRUNCATED)?);
        at += 1;
        if len == 0 {
            return Ok(at);
        }
        at = at
            .checked_add(len)
            .filter(|&end| end <= data.len())
            .ok_or(GIF_TRUNCATED)?;
    }
}

/// Check that `out` is an animated WebP: a RIFF file whose size is its
/// length, its chunks ending with the file, the first the extended header
/// (`VP8X`) with the animation flag, an animation header (`ANIM`) before
/// any frame, at least two frames (`ANMF`: one is a still image labelled
/// animated, which libwebp never makes), and no image outside the frames
/// (`VP8 `, `VP8L`, `ALPH`). What it found instead, in words.
pub(super) fn check(out: &[u8]) -> Result<(), &'static str> {
    if out.get(..4) != Some(&b"RIFF"[..]) || out.get(8..12) != Some(&b"WEBP"[..]) {
        return Err("no WebP file");
    }
    let size = out
        .get(4..8)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(u32::from_le_bytes)
        .ok_or("no WebP file")?;
    if usize::try_from(size).ok().and_then(|s| s.checked_add(8)) != Some(out.len()) {
        return Err("a WebP file whose RIFF size is not its length");
    }
    let mut at = 12usize;
    let mut first = true;
    let mut header = false;
    let mut frames = 0usize;
    while at < out.len() {
        let chunk = out
            .get(at..at + 8)
            .ok_or("a WebP chunk past the file's end")?;
        let len = usize::try_from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]))
            .map_err(|_| "a WebP chunk past the file's end")?;
        let body = at + 8;
        let end = body
            .checked_add(len)
            .and_then(|end| end.checked_add(len % 2))
            .filter(|&end| end <= out.len())
            .ok_or("a WebP chunk past the file's end")?;
        match (&chunk[..4], first) {
            (b"VP8X", true) => {
                // The flags byte: reserved, ICC, alpha, EXIF, XMP,
                // animation, reserved (most significant bit first).
                let flags = *out
                    .get(body)
                    .filter(|_| len >= 10)
                    .ok_or("a truncated WebP header")?;
                if flags & 0x02 == 0 {
                    return Err("a WebP that is not animated");
                }
            }
            (_, true) => return Err("a WebP that is not animated"),
            (b"ANIM", false) => {
                if len < 6 {
                    return Err("a truncated WebP animation header");
                }
                header = true;
            }
            (b"ANMF", false) => {
                if !header {
                    return Err("a WebP frame before its animation header");
                }
                if len < 16 {
                    return Err("a truncated WebP frame");
                }
                frames += 1;
            }
            (b"VP8 " | b"VP8L" | b"ALPH", false) => {
                return Err("an animated WebP with an image outside its frames");
            }
            _ => {}
        }
        first = false;
        at = end;
    }
    if !header {
        return Err("a WebP without its animation header (ANIM)");
    }
    match frames {
        0 => Err("an animated WebP without frames (ANMF)"),
        1 => Err("an animated WebP of one frame (ANMF): a still image"),
        _ => Ok(()),
    }
}
