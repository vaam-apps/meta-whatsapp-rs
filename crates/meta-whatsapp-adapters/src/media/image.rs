//! Images into a static WebP sticker, a JPEG or a PNG: the fixed
//! arguments of each attempt (a ladder of sizes and qualities), the check
//! of the input's first bytes and the check of the output (see the
//! [module docs](super)).

use std::ops::Range;

use bytes::Bytes;
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

/// Whether `data` begins as a PNG (its eight-byte signature) or a JPEG
/// (start of image, then a marker) does. What `ffmpeg` then detects must
/// also be one of [`INPUT_FORMATS`]: this check only keeps everything
/// else from reaching it.
pub(super) fn accepted(data: &[u8]) -> bool {
    data.starts_with(PNG_SIGNATURE) || data.starts_with(JPEG_START)
}

/// The chunks taken out of a PNG before `ffmpeg` reads it: its text
/// (`tEXt`, `zTXt`, `iTXt`) and its EXIF (`eXIf`), metadata the result
/// never carries. `ffmpeg` 6.1 inflates every `zTXt` chunk it meets and
/// keeps the text until the frame is decoded, each chunk under the
/// allocation bound but not all of them together: a 100 KB chunk
/// inflating to 100 MiB cost some 400 MB of memory, four of them 1.6 GB,
/// and a 64 MiB input holds hundreds.
const PNG_DROPPED: &[&[u8; 4]] = &[b"tEXt", b"zTXt", b"iTXt", b"eXIf"];

/// The largest colour profile chunk (`iCCP`) passed on, in bytes of
/// chunk data (its name and its compressed profile): 16 KiB. `ffmpeg`
/// 6.1 inflates the profile whole: a 200 KB chunk inflating to 200 MiB
/// took it 660 MB, where a real profile is a few kilobytes. A larger
/// one, and every `iCCP` chunk after the first, is taken out.
pub(super) const PNG_ICCP_MAX: usize = 16 * 1024;

/// The longest chunk PNG allows (2^31 − 1 bytes of data).
const PNG_MAX_CHUNK: u32 = 0x7FFF_FFFF;

/// What a truncated PNG is, in words.
const PNG_TRUNCATED: &str = "a PNG whose chunks stop before its end chunk (a truncated file)";

/// What `ffmpeg` is given of an accepted image, or why it is refused, in
/// words: a PNG walked chunk by chunk ([`png_prepared`]), a JPEG segment
/// by segment ([`jpeg_prepared`]); anything else as it is.
pub(super) fn prepared(data: &Bytes) -> Result<Bytes, &'static str> {
    if data.starts_with(PNG_SIGNATURE) {
        png_prepared(data)
    } else if data.starts_with(&JPEG_START[..2]) {
        jpeg_prepared(data)
    } else {
        Ok(data.clone())
    }
}

/// A piece of what `ffmpeg` is given: a range of the input, or bytes
/// written in its place.
enum Piece {
    Input(Range<usize>),
    Written(Vec<u8>),
}

/// Keep `range` of the input, merged with the range before it when the
/// two touch.
fn keep(pieces: &mut Vec<Piece>, range: Range<usize>) {
    if let Some(Piece::Input(last)) = pieces.last_mut()
        && last.end == range.start
    {
        last.end = range.end;
        return;
    }
    pieces.push(Piece::Input(range));
}

/// Keep `range` of `data` when it is `written` already, or put `written`
/// in its place.
fn keep_or_write(pieces: &mut Vec<Piece>, data: &[u8], range: Range<usize>, written: &[u8]) {
    if data.get(range.clone()) == Some(written) {
        keep(pieces, range);
    } else {
        pieces.push(Piece::Written(written.to_vec()));
    }
}

/// The pieces, gathered: `data` itself, uncopied, when they are all of
/// it; a slice of it when they are one range; a copy otherwise.
fn gathered(data: &Bytes, pieces: &[Piece]) -> Bytes {
    match pieces {
        [Piece::Input(whole)] if *whole == (0..data.len()) => data.clone(),
        [Piece::Input(one)] => data.slice(one.clone()),
        _ => {
            let len = pieces
                .iter()
                .map(|piece| match piece {
                    Piece::Input(range) => range.len(),
                    Piece::Written(bytes) => bytes.len(),
                })
                .sum();
            let mut out = Vec::with_capacity(len);
            for piece in pieces {
                match piece {
                    Piece::Input(range) => out.extend_from_slice(&data[range.clone()]),
                    Piece::Written(bytes) => out.extend_from_slice(bytes),
                }
            }
            Bytes::from(out)
        }
    }
}

/// A PNG walked chunk by chunk from its signature to its end chunk
/// (`IEND`): without [`PNG_DROPPED`], with its first colour profile
/// chunk (`iCCP`) only if it is at most [`PNG_ICCP_MAX`], and without
/// anything after the end chunk (a second image, appended data); the
/// same bytes uncopied when there is nothing to take out. One whose
/// chunks do not reach an end chunk is refused.
fn png_prepared(data: &Bytes) -> Result<Bytes, &'static str> {
    let mut pieces = Vec::with_capacity(8);
    keep(&mut pieces, 0..PNG_SIGNATURE.len());
    let mut profiles = 0usize;
    png_chunks(data, |kind, chunk| {
        let kept = if kind == b"iCCP" {
            profiles += 1;
            // Length, type, data, CRC: the data is 12 bytes shorter.
            profiles == 1 && chunk.len().saturating_sub(12) <= PNG_ICCP_MAX
        } else {
            !PNG_DROPPED.iter().any(|dropped| &dropped[..] == kind)
        };
        if kept {
            keep(&mut pieces, chunk);
        }
    })?;
    Ok(gathered(data, &pieces))
}

/// Walk a PNG's chunks (`data` begins with its signature), from the first
/// to the end chunk, giving `each` the type and the byte range (length to
/// CRC) of each; the end of the end chunk, or what stopped the walk, in
/// words. The CRCs are not checked.
fn png_chunks(
    data: &[u8],
    mut each: impl FnMut(&[u8], Range<usize>),
) -> Result<usize, &'static str> {
    let mut at = PNG_SIGNATURE.len();
    loop {
        let header = data
            .get(at..)
            .and_then(|rest| rest.get(..8))
            .ok_or(PNG_TRUNCATED)?;
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        if len > PNG_MAX_CHUNK {
            return Err("a PNG chunk longer than PNG allows");
        }
        let kind = &header[4..8];
        // Length, type, data, CRC.
        let end = usize::try_from(len)
            .ok()
            .and_then(|len| at.checked_add(12)?.checked_add(len))
            .filter(|&end| end <= data.len())
            .ok_or(PNG_TRUNCATED)?;
        each(kind, at..end);
        at = end;
        if kind == b"IEND" {
            return Ok(at);
        }
    }
}

/// What a JPEG cut short is, in words.
const JPEG_TRUNCATED: &str = "a JPEG that stops before its end of image (a truncated file)";

/// What a JPEG segment longer than the file is, in words.
const JPEG_PAST_END: &str = "a JPEG segment whose length runs past the end of the file";

/// A JPEG walked segment by segment, from its start of image to its end
/// of image (`EOI`), keeping only what `ffmpeg` reads by the segment's
/// length:
///
/// - the tables, the frame header and the scans that `ffmpeg` 6.1 can
///   decode (`DQT`, `DHT`, `SOF0` to `SOF3` and JPEG-LS's `SOF55`, `DRI`,
///   `SOS`), each refused unless its length is exactly its content (as
///   libjpeg refuses one), and each scan's entropy-coded data (its
///   stuffed `FF 00` bytes and its restart markers) up to the next
///   marker;
/// - the first JFIF segment (`APP0`) and the first Adobe segment
///   (`APP14`, which says whether three components are RGB), rewritten
///   to their fixed fields (no JFIF thumbnail);
/// - in place of the first EXIF segment (`APP1`, `Exif\0\0`), a minimal
///   EXIF of its Orientation alone ([`minimal_exif`]), or nothing when
///   it has no valid one;
/// - the first colour profile segment (`APP2`, `ICC_PROFILE`) when it
///   holds a whole profile of at most 16 KiB ([`jpeg_profile_kept`]);
///
/// and the end of image. Everything else is dropped: every other `APP1`
/// (EXIF, XMP), every other application segment (any other `APP2`, so
/// the rest of a profile in several chunks, and with its first chunk
/// dropped too, the whole profile; `APP13`), comments, the markers `ffmpeg` does not decode, bytes
/// between segments, and anything after the end of image. One cut short
/// before its end of image, a segment whose length runs past the end or
/// is under two bytes, a header whose length is not its content, or a
/// second start of image, is refused.
///
/// The reason is memory: `ffmpeg` 6.1 copies an EXIF value once for
/// every IFD entry pointing at it, and reads every `APP1` it meets: a
/// 67 KB JPEG of one such segment took 916 MB, one of ten 8.4 GB. And
/// since `ffmpeg` moves on by the bytes it read, not by a segment's
/// length, a segment it reads only in part (a scan header it skips, a
/// JFIF thumbnail, which it does not skip) would show it whatever the
/// walk took for that segment's content: so each segment kept is one it
/// reads whole.
fn jpeg_prepared(data: &Bytes) -> Result<Bytes, &'static str> {
    let mut pieces = Vec::with_capacity(16);
    keep(&mut pieces, 0..2);
    let (mut exif, mut jfif, mut adobe, mut profile) = (false, false, false, false);
    // Just after a scan's header: what comes before the next marker is
    // its entropy-coded data.
    let mut scan = false;
    let mut at = 2;
    loop {
        let start = jpeg_marker(data, at).ok_or(JPEG_TRUNCATED)?;
        if std::mem::take(&mut scan) {
            keep(&mut pieces, at..start);
        }
        let marker = data[start + 1];
        match marker {
            // End of image: the end of what ffmpeg is given.
            0xD9 => {
                keep(&mut pieces, start..start + 2);
                return Ok(gathered(data, &pieces));
            }
            0xD8 => return Err("a JPEG with a second start of image before its end"),
            _ => {}
        }
        // A segment: its length counts itself, not the marker.
        let len = data
            .get(start + 2..start + 4)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
            .ok_or(JPEG_TRUNCATED)?;
        if len < 2 {
            return Err("a JPEG segment whose length is under the two bytes of its length field");
        }
        let end = start + 2 + len;
        let body = data.get(start + 4..end).ok_or(JPEG_PAST_END)?;
        let segment = start..end;
        match marker {
            0xC0..=0xC4 | 0xDA | 0xDB | 0xDD | 0xF7 => {
                if !jpeg_exact(marker, body) {
                    return Err(
                        "a JPEG table, frame or scan header whose length is not its content",
                    );
                }
                keep(&mut pieces, segment);
                scan = marker == 0xDA;
            }
            0xE1 if !exif && body.starts_with(b"Exif\0\0") => {
                exif = true;
                if let Some(orientation) = exif_orientation(&body[6..]) {
                    keep_or_write(&mut pieces, data, segment, &minimal_exif(orientation));
                }
            }
            0xE0 if !jfif && body.len() >= 14 && body.starts_with(b"JFIF\0") => {
                jfif = true;
                // Version, density units, X and Y density; no thumbnail.
                let mut written = *b"\xFF\xE0\0\x10JFIF\0\0\0\0\0\0\0\0\0\0";
                written[9..16].copy_from_slice(&body[5..12]);
                keep_or_write(&mut pieces, data, segment, &written);
            }
            0xE2 if !profile && body.starts_with(b"ICC_PROFILE") => {
                profile = true;
                if jpeg_profile_kept(body) {
                    keep(&mut pieces, segment);
                }
            }
            0xEE if !adobe && body.len() >= 12 && body.starts_with(b"Adobe") => {
                adobe = true;
                // Version, two flag words, the colour transform.
                let mut written = [0u8; 16];
                written[..4].copy_from_slice(&[0xFF, 0xEE, 0, 14]);
                written[4..].copy_from_slice(&body[..12]);
                keep_or_write(&mut pieces, data, segment, &written);
            }
            _ => {}
        }
        at = end;
    }
}

/// The largest JPEG colour profile passed on, in bytes of profile (after
/// the `APP2` segment's 14-byte header): 16 KiB, as for a PNG's
/// ([`PNG_ICCP_MAX`]).
pub(super) const JPEG_ICC_MAX: usize = PNG_ICCP_MAX;

/// Whether a colour profile segment's `body` (after its length, which
/// begins `ICC_PROFILE`, as `ffmpeg` 6.1 reads one) is kept: its header
/// whole (`ICC_PROFILE\0`), a whole profile in one segment (chunk 1 of
/// 1), of at least one byte and at most [`JPEG_ICC_MAX`]. `ffmpeg` reads
/// the header's 14 bytes (the name, the chunk's number, the count) and
/// takes the rest of the segment as the profile, so a whole header is all
/// its length needs to be its content. A profile in several chunks is
/// not kept: the chunks after the first are dropped with every other
/// `APP2`.
fn jpeg_profile_kept(body: &[u8]) -> bool {
    matches!(body.get(11..14), Some([0, 1, 1])) && (15..=14 + JPEG_ICC_MAX).contains(&body.len())
}

/// The position of the next marker at or after `from`, as `ffmpeg` 6.1
/// finds one: `FF` followed by `C0` to `FE`, a restart marker (`D0` to
/// `D7`) aside, which is part of a scan's data. Fill bytes (`FF FF`), a
/// stuffed `FF 00` and JPEG-LS's `FF` followed by under `80` are not
/// markers.
fn jpeg_marker(data: &[u8], from: usize) -> Option<usize> {
    let rest = data.get(from..)?;
    rest.windows(2)
        .position(|w| w[0] == 0xFF && matches!(w[1], 0xC0..=0xCF | 0xD8..=0xFE))
        .map(|i| from + i)
}

/// Whether a table, frame or scan header's `body` (after its length) is
/// exactly what its content says, as `ffmpeg` reads it: no byte left
/// over, none missing. A scan header must also hold no `FF`: `ffmpeg`
/// skips a scan it cannot decode without reading its header, and would
/// find a marker there.
fn jpeg_exact(marker: u8, mut body: &[u8]) -> bool {
    match marker {
        // Quantization tables: precision (0 or 1) and table (0 to 3),
        // then 64 values of 8 or 16 bits.
        0xDB => {
            while let Some(&pq_tq) = body.first() {
                if pq_tq >> 4 > 1 || pq_tq & 0x0F > 3 {
                    return false;
                }
                let size = 1 + 64 * (1 + usize::from(pq_tq >> 4));
                let Some(rest) = body.get(size..) else {
                    return false;
                };
                body = rest;
            }
            true
        }
        // Huffman tables: class (0 or 1) and table (0 to 3), 16 counts,
        // then as many values (at most 256).
        0xC4 => {
            while let Some(&tc_th) = body.first() {
                if tc_th >> 4 > 1 || tc_th & 0x0F > 3 {
                    return false;
                }
                let Some(counts) = body.get(1..17) else {
                    return false;
                };
                let values: usize = counts.iter().map(|&c| usize::from(c)).sum();
                let Some(rest) = body.get(17 + values..).filter(|_| values <= 256) else {
                    return false;
                };
                body = rest;
            }
            true
        }
        // A frame header: precision, height, width, the component count,
        // three bytes a component.
        0xC0..=0xC3 | 0xF7 => body
            .get(5)
            .is_some_and(|&n| body.len() == 6 + 3 * usize::from(n)),
        // A restart interval.
        0xDD => body.len() == 2,
        // A scan header: the component count, two bytes a component,
        // then three.
        0xDA => {
            body.first()
                .is_some_and(|&n| body.len() == 4 + 2 * usize::from(n))
                && !body.contains(&0xFF)
        }
        _ => false,
    }
}

/// A valid Orientation (1 to 8) in the first IFD of an EXIF segment's
/// TIFF data (after `Exif\0\0`): the tag `0x0112`, one SHORT. `None`
/// for anything else: no such tag, another type or count, a value out
/// of range, an offset out of the data.
fn exif_orientation(tiff: &[u8]) -> Option<u8> {
    let big = match tiff.get(..4)? {
        b"MM\0*" => true,
        b"II*\0" => false,
        _ => return None,
    };
    let u16_at = |at: usize| {
        let b = tiff.get(at..at.checked_add(2)?)?;
        Some(if big {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        })
    };
    let u32_at = |at: usize| {
        let b = tiff.get(at..at.checked_add(4)?)?;
        let b = [b[0], b[1], b[2], b[3]];
        Some(if big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    };
    let ifd = usize::try_from(u32_at(4)?).ok()?;
    let count = usize::from(u16_at(ifd)?);
    // Entries of 12 bytes after the count; the first Orientation.
    let entry = (0..count)
        .map_while(|i| {
            ifd.checked_add(2 + 12 * i)
                .filter(|&e| e + 12 <= tiff.len())
        })
        .find(|&e| u16_at(e) == Some(0x0112))?;
    if u16_at(entry + 2)? != 3 || u32_at(entry + 4)? != 1 {
        return None;
    }
    u8::try_from(u16_at(entry + 8)?)
        .ok()
        .filter(|o| (1..=8).contains(o))
}

/// The EXIF segment `ffmpeg` is given in place of a JPEG's own, 36
/// bytes: `APP1` and its length (34); `Exif\0\0`; a big-endian TIFF
/// header (`MM`, 42, the first IFD at 8); one IFD of one entry, the
/// Orientation (tag `0x0112`, SHORT, one value: `orientation`, padded to
/// four bytes); no next IFD. Enough for `ffmpeg` to rotate the frame,
/// and nothing it could copy.
pub(super) const fn minimal_exif(orientation: u8) -> [u8; 36] {
    let mut segment = *b"\xFF\xE1\x00\x22Exif\0\0MM\x00\x2A\x00\x00\x00\x08\
                         \x00\x01\x01\x12\x00\x03\x00\x00\x00\x01\x00\x00\x00\x00\
                         \x00\x00\x00\x00";
    segment[29] = orientation;
    segment
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
    // One image, whole: its chunks end with its end chunk, and so does
    // the file.
    if png_chunks(out, |_, _| {}) != Ok(out.len()) {
        return Err("a PNG file that does not end with its end chunk");
    }
    match (out.get(24), out.get(25)) {
        (Some(8), Some(2 | 6)) => Ok(()),
        (Some(8), _) => Err("a PNG that is not RGB or RGBA"),
        _ => Err("a PNG that is not 8-bit"),
    }
}
