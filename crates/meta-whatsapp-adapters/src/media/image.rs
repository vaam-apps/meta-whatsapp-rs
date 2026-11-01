//! Images into a static WebP sticker, a JPEG or a PNG: the fixed
//! arguments of each attempt (a ladder of sizes and qualities), the check
//! of the input's first bytes and the check of the output (see the
//! [module docs](super)).

use meta_whatsapp_core::media::TargetFormat;

/// The demuxers `ffmpeg` may use on an image: a single PNG or JPEG.
/// Animated PNG (`apng`), JPEG-LS (`jpegls_pipe`), GIF, WebP and
/// everything else are refused, even when the first bytes look like a PNG
/// or a JPEG (an APNG's and a JPEG-LS file's do).
pub(super) const INPUT_FORMATS: &str = "png_pipe,jpeg_pipe";

/// The most pixels a decoded image may have (`-max_pixels`): 32 Mi
/// (33 554 432), e.g. 8192 × 4096, past a 24-megapixel camera's
/// 6000 × 4000. A header declaring more is refused before its frame is
/// allocated.
pub(super) const MAX_PIXELS: &str = "33554432";

/// The largest single allocation `ffmpeg` may make on an image
/// (`-max_alloc`), in bytes: 256 MiB, one frame at [`MAX_PIXELS`] of
/// 16-bit RGBA (8 bytes a pixel), the largest a PNG decodes to.
pub(super) const MAX_ALLOC: &str = "268435456";

/// Everything up to the filter, the same for every attempt: the bounds,
/// the input options (before `-i`), and the first video stream's first
/// frame, without metadata (EXIF can carry a place and a name).
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
    // Input options: read stdin only, a PNG or a JPEG only, and no frame
    // over the pixel bound.
    "-protocol_whitelist",
    "pipe",
    "-format_whitelist",
    INPUT_FORMATS,
    "-max_pixels",
    MAX_PIXELS,
    "-i",
    "pipe:0",
    "-map",
    "0:v:0",
    "-map_metadata",
    "-1",
    "-frames:v",
    "1",
];

/// After the encoder: the encoder's packet as it is, on stdout. Not the
/// `webp` muxer: writing to a pipe, it left the RIFF size of a large
/// WebP at zero (it patches it by seeking back, which a pipe cannot do),
/// where libwebp's own packet carries it.
pub(super) const OUTPUT_ARGS: &[&str] = &["-f", "image2pipe", "pipe:1"];

/// One attempt: a filter (scale, then the pixel format) and an encoder
/// with its quality. Each is a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rung {
    /// The `-vf` filter graph.
    pub(super) filter: &'static str,
    /// The encoder and its options.
    pub(super) encoder: &'static [&'static str],
}

const WEBP_80: &[&str] = &[
    "-c:v",
    "libwebp",
    "-lossless",
    "0",
    "-compression_level",
    "6",
    "-quality",
    "80",
];
const WEBP_75: &[&str] = &[
    "-c:v",
    "libwebp",
    "-lossless",
    "0",
    "-compression_level",
    "6",
    "-quality",
    "75",
];
const WEBP_50: &[&str] = &[
    "-c:v",
    "libwebp",
    "-lossless",
    "0",
    "-compression_level",
    "6",
    "-quality",
    "50",
];

/// A static sticker: lossy WebP, transparency kept (`yuva420p` when the
/// image has an alpha channel, `yuv420p` otherwise). The first attempt
/// keeps the image's size (up to WebP's own limit, 16383 pixels a side);
/// the next ones fit it within a square of 1024, 512, then 256 pixels,
/// never enlarging it and keeping its aspect ratio.
pub(super) const STICKER: &[Rung] = &[
    Rung {
        filter: "scale=w='min(iw,16383)':h='min(ih,16383)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: WEBP_80,
    },
    Rung {
        filter: "scale=w='min(iw,1024)':h='min(ih,1024)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: WEBP_80,
    },
    Rung {
        filter: "scale=w='min(iw,1024)':h='min(ih,1024)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: WEBP_50,
    },
    Rung {
        filter: "scale=w='min(iw,512)':h='min(ih,512)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: WEBP_75,
    },
    Rung {
        filter: "scale=w='min(iw,512)':h='min(ih,512)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: WEBP_50,
    },
    Rung {
        filter: "scale=w='min(iw,256)':h='min(ih,256)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        encoder: WEBP_50,
    },
];

const MJPEG_2: &[&str] = &["-c:v", "mjpeg", "-q:v", "2"];
const MJPEG_5: &[&str] = &["-c:v", "mjpeg", "-q:v", "5"];
const MJPEG_10: &[&str] = &["-c:v", "mjpeg", "-q:v", "10"];

/// A JPEG: 8-bit YCbCr 4:2:0 (`yuvj420p`, full range). Its size first at
/// falling qualities (`-q:v` 2, 5, 10: lower is better), then fitted
/// within 2048 and 1024 pixels.
pub(super) const JPEG: &[Rung] = &[
    Rung {
        filter: "format=yuvj420p",
        encoder: MJPEG_2,
    },
    Rung {
        filter: "format=yuvj420p",
        encoder: MJPEG_5,
    },
    Rung {
        filter: "format=yuvj420p",
        encoder: MJPEG_10,
    },
    Rung {
        filter: "scale=w='min(iw,2048)':h='min(ih,2048)':force_original_aspect_ratio=decrease,format=yuvj420p",
        encoder: MJPEG_5,
    },
    Rung {
        filter: "scale=w='min(iw,1024)':h='min(ih,1024)':force_original_aspect_ratio=decrease,format=yuvj420p",
        encoder: MJPEG_10,
    },
];

const PNG_ENCODER: &[&str] = &["-c:v", "png", "-compression_level", "9", "-pred", "mixed"];

/// A PNG: 8-bit RGBA when the image has an alpha channel, RGB otherwise
/// (a 16-bit, grey or palette PNG becomes one of them). Lossless, so only
/// its size falls: as it is, then fitted within 2048 and 1024 pixels (at
/// 1024 × 1024, even incompressible RGBA is 4 MiB, under the limit).
pub(super) const PNG: &[Rung] = &[
    Rung {
        filter: "format=rgba|rgb24",
        encoder: PNG_ENCODER,
    },
    Rung {
        filter: "scale=w='min(iw,2048)':h='min(ih,2048)':force_original_aspect_ratio=decrease,format=rgba|rgb24",
        encoder: PNG_ENCODER,
    },
    Rung {
        filter: "scale=w='min(iw,1024)':h='min(ih,1024)':force_original_aspect_ratio=decrease,format=rgba|rgb24",
        encoder: PNG_ENCODER,
    },
];

/// The attempts that make `target`, in order, or `None` for a format that
/// is not an image.
pub(super) fn ladder(target: TargetFormat) -> Option<&'static [Rung]> {
    match target {
        TargetFormat::StaticSticker => Some(STICKER),
        TargetFormat::Jpeg => Some(JPEG),
        TargetFormat::Png => Some(PNG),
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
    args.extend_from_slice(OUTPUT_ARGS);
    args
}

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG_START: &[u8] = &[0xFF, 0xD8, 0xFF];
const PNG_END: &[u8] = &[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82];

/// Whether `data` begins as a PNG (its eight-byte signature) or a JPEG
/// (start of image, then a marker) does. What `ffmpeg` then detects must
/// also be one of [`INPUT_FORMATS`]: this check only keeps everything
/// else from reaching it.
pub(super) fn accepted(data: &[u8]) -> bool {
    data.starts_with(PNG_SIGNATURE) || data.starts_with(JPEG_START)
}

/// Check that `out` is `target`, reading its header; what it found
/// instead, in words.
pub(super) fn check(target: TargetFormat, out: &[u8]) -> Result<(), &'static str> {
    match target {
        TargetFormat::StaticSticker => static_webp(out),
        TargetFormat::Jpeg => jpeg(out),
        TargetFormat::Png => png(out),
        _ => Err("a format it cannot check"),
    }
}

/// A WebP file (`RIFF`, its size, `WEBP`), the size its length, its first
/// chunk an image (`VP8 `, lossy; `VP8L`, lossless) or the extended
/// header (`VP8X`) without the animation flag.
fn static_webp(out: &[u8]) -> Result<(), &'static str> {
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
    match out.get(12..16) {
        Some(b"VP8 " | b"VP8L") => Ok(()),
        // The flags byte: reserved, ICC, alpha, EXIF, XMP, animation,
        // reserved (most significant bit first).
        Some(b"VP8X") => match out.get(20) {
            Some(flags) if flags & 0x02 != 0 => Err("an animated WebP"),
            Some(_) => Ok(()),
            None => Err("a truncated WebP header"),
        },
        _ => Err("a WebP file with no image"),
    }
}

/// A JPEG file (start and end of image) whose frame is 8-bit with three
/// components (colour: not grey, not CMYK).
fn jpeg(out: &[u8]) -> Result<(), &'static str> {
    if !out.starts_with(&JPEG_START[..2]) || !out.ends_with(&[0xFF, 0xD9]) {
        return Err("no JPEG file");
    }
    match jpeg_frame(out) {
        Some((8, 3)) => Ok(()),
        Some((8, _)) => Err("a JPEG that is not in colour"),
        Some(_) => Err("a JPEG that is not 8-bit"),
        None => Err("a JPEG with no frame header"),
    }
}

/// The precision and the component count of a JPEG's frame header (its
/// first start-of-frame marker), walking the markers before it; `None`
/// when it reaches the scan, or the end, first.
fn jpeg_frame(b: &[u8]) -> Option<(u8, u8)> {
    let mut at = 2usize;
    loop {
        if *b.get(at)? != 0xFF {
            return None;
        }
        match *b.get(at + 1)? {
            // Fill bytes before a marker.
            0xFF => at += 1,
            // Markers without a length.
            0x01 | 0xD0..=0xD7 => at += 2,
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return Some((*b.get(at + 4)?, *b.get(at + 9)?));
            }
            // The scan or the end of the image, before any frame.
            0xD8..=0xDA => return None,
            _ => {
                let len = u16::from_be_bytes([*b.get(at + 2)?, *b.get(at + 3)?]);
                at = at.checked_add(2 + usize::from(len))?;
            }
        }
    }
}

/// A PNG file (its signature, a header first, the end chunk last) of
/// 8-bit RGB (colour type 2) or RGBA (6): Meta's "8-bit, RGB or RGBA".
fn png(out: &[u8]) -> Result<(), &'static str> {
    if !out.starts_with(PNG_SIGNATURE) {
        return Err("no PNG file");
    }
    if out.get(8..16) != Some(&[0, 0, 0, 13, b'I', b'H', b'D', b'R'][..]) {
        return Err("a PNG file without its header");
    }
    if !out.ends_with(PNG_END) {
        return Err("a truncated PNG file");
    }
    match (out.get(24), out.get(25)) {
        (Some(8), Some(2 | 6)) => Ok(()),
        (Some(8), _) => Err("a PNG that is not RGB or RGBA"),
        _ => Err("a PNG that is not 8-bit"),
    }
}
