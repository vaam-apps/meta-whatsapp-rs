//! The converter's contract, against stand-in programs (shell scripts that
//! behave as `ffmpeg` might): the arguments, the missing binary, the
//! bounds, the timeout and the withheld diagnostics. The conversion itself
//! runs against a real `ffmpeg` in `tests/ffmpeg.rs`.

use super::*;

/// The exact command line, literal by literal. Each of the guards is one
/// of these: dropping the protocol or format whitelist, mono, the codec or
/// the container fails here.
#[test]
fn the_arguments_are_fixed_literals() {
    let expected: Vec<&str> = "-hide_banner -nostdin -nostats -loglevel error \
         -max_alloc 16777216 -abort_on empty_output \
         -protocol_whitelist pipe \
         -format_whitelist wav,mp3,ogg,flac,aac,mov,amr,matroska \
         -i pipe:0 \
         -map 0:a:0 -map_metadata -1 -map_chapters -1 \
         -ac 1 -ar 48000 -c:a libopus -b:a 32k -application voip \
         -f ogg pipe:1"
        .split_whitespace()
        .collect();
    assert_eq!(arguments(TargetFormat::OggOpus), Some(&expected[..]));
    // The input options come before `-i`, where ffmpeg applies them to the
    // input; after it they would be output options and restrict nothing.
    let input = OGG_OPUS_ARGS.iter().position(|a| *a == "-i").unwrap();
    for option in ["-protocol_whitelist", "-format_whitelist"] {
        let at = OGG_OPUS_ARGS.iter().position(|a| *a == option).unwrap();
        assert!(at < input, "{option} must precede -i");
    }
}

fn opus_page(channels: u8) -> Vec<u8> {
    // RFC 7845 § 5.1: "OpusHead", version 1, channels, pre-skip, rate,
    // gain, mapping family 0.
    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, channels, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0, 0]);
    let mut page = b"OggS".to_vec();
    page.extend_from_slice(&[0, 0x02]); // version, beginning of stream
    page.extend_from_slice(&[0; 8 + 4 + 4 + 4]); // granule, serial, sequence, CRC
    page.push(1);
    page.push(u8::try_from(head.len()).unwrap());
    page.extend_from_slice(&head);
    page
}

#[test]
fn the_output_check_wants_mono_ogg_opus() {
    let t = TargetFormat::OggOpus;
    assert_eq!(check_output(t, &opus_page(1)), Ok(()));
    assert_eq!(
        check_output(t, &opus_page(2)),
        Err("Opus with more than one channel")
    );
    let mut vorbis = opus_page(1);
    vorbis[28..36].copy_from_slice(b"\x01vorbis\0");
    assert_eq!(
        check_output(t, &vorbis),
        Err("an Ogg stream that is not Opus")
    );
    let mut not_first = opus_page(1);
    not_first[5] = 0;
    assert_eq!(check_output(t, &not_first), Err("no Ogg stream"));
    assert_eq!(check_output(t, b"RIFF....WAVE"), Err("no Ogg stream"));
    assert_eq!(check_output(t, &[]), Err("no Ogg stream"));
    let page = opus_page(1);
    assert_eq!(
        check_output(t, &page[..page.len() - 1]),
        Err("no Ogg stream")
    );
}

/// Every attempt of every image format: the input options before `-i`,
/// the same bounds, the encoder its format needs, the aspect ratio kept,
/// never enlarged, and sizes that only fall.
#[test]
fn the_image_arguments_are_fixed_literals() {
    let expected: Vec<&str> = "-hide_banner -nostdin -nostats -loglevel error \
         -max_alloc 268435456 -abort_on empty_output \
         -protocol_whitelist pipe -format_whitelist png_pipe,jpeg_pipe \
         -max_pixels 33554432 \
         -i pipe:0 -map 0:v:0 -map_metadata -1 -frames:v 1 \
         -vf"
        .split_whitespace()
        .chain([
            "scale=w='min(iw,16383)':h='min(ih,16383)':force_original_aspect_ratio=decrease,format=yuva420p|yuv420p",
        ])
        .chain(
            "-c:v libwebp -lossless 0 -compression_level 6 -quality 80 -f image2pipe pipe:1"
                .split_whitespace(),
        )
        .collect();
    assert_eq!(image::arguments(&image::STICKER[0]), expected);

    for (target, encoder, format) in [
        (
            TargetFormat::StaticSticker,
            "libwebp",
            "format=yuva420p|yuv420p",
        ),
        (TargetFormat::Jpeg, "mjpeg", "format=yuvj420p"),
        (TargetFormat::Png, "png", "format=rgba|rgb24"),
    ] {
        let ladder = image::ladder(target).unwrap();
        assert!(ladder.len() >= 3, "{target:?}");
        let mut cap = u32::MAX;
        for rung in ladder {
            let args = image::arguments(rung);
            let input = args.iter().position(|a| *a == "-i").unwrap();
            for option in ["-protocol_whitelist", "-format_whitelist", "-max_pixels"] {
                let at = args.iter().position(|a| *a == option).unwrap();
                assert!(at < input, "{option} must precede -i");
            }
            assert_eq!(&args[..image::INPUT_ARGS.len()], image::INPUT_ARGS);
            assert_eq!(&args[args.len() - 3..], ["-f", "image2pipe", "pipe:1"]);
            assert_eq!(&rung.encoder[..2], ["-c:v", encoder], "{target:?}");
            assert!(rung.filter.ends_with(format), "{}", rung.filter);
            if let Some(scale) = rung.filter.strip_prefix("scale=") {
                // `w='min(iw,N)':h='min(ih,N)'`: a square of N, never more
                // than the image's own size.
                let n: u32 = scale
                    .strip_prefix("w='min(iw,")
                    .and_then(|s| s.split_once(')'))
                    .unwrap()
                    .0
                    .parse()
                    .unwrap();
                assert!(
                    scale.starts_with(&format!(
                        "w='min(iw,{n})':h='min(ih,{n})':force_original_aspect_ratio=decrease,"
                    )),
                    "{scale}"
                );
                assert!(n <= cap, "{target:?}: sizes only fall");
                cap = n;
            } else {
                assert_eq!(cap, u32::MAX, "{target:?}: unscaled after scaled");
            }
        }
    }
    assert!(image::ladder(TargetFormat::OggOpus).is_none());
    assert!(arguments(TargetFormat::StaticSticker).is_none());
}

#[test]
fn the_image_input_check_wants_a_png_or_a_jpeg() {
    assert!(image::accepted(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"));
    assert!(image::accepted(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 16]));
    for refused in [
        &b"GIF89a\x01\0\x01\0"[..],
        b"GIF87a",
        b"RIFF\x1a\0\0\0WEBPVP8 ",
        b"<svg xmlns='http://www.w3.org/2000/svg'/>",
        b"II*\0",
        b"\x89PNG\r\n",
        &[0xFF, 0xD8],
        b"ffconcat version 1.0\nfile '/etc/hostname'\n",
        b"",
    ] {
        assert!(!image::accepted(refused), "{refused:?}");
    }
}

/// A WebP file: `RIFF`, its size, `WEBP`, then `chunk` with `body`.
fn webp(chunk: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&u32::try_from(4 + 8 + body.len()).unwrap().to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
    out.extend_from_slice(body);
    out
}

/// A JPEG file: start of image, an APP0 segment, a frame header of
/// `precision` bits and `components`, a scan, end of image.
fn jpeg(precision: u8, components: u8) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
    out.extend_from_slice(b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0");
    let len = 8 + 3 * u16::from(components);
    out.extend_from_slice(&[0xFF, 0xC0]);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&[precision, 0, 16, 0, 16, components]);
    for c in 0..components {
        out.extend_from_slice(&[c + 1, 0x11, 0]);
    }
    out.extend_from_slice(&[0xFF, 0xDA, 0, 2, 0x12, 0x34, 0xFF, 0xD9]);
    out
}

/// A PNG file: signature, a header of `depth` and `colour`, an end chunk.
fn png(depth: u8, colour: u8) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend_from_slice(&[0, 0, 0, 13]);
    out.extend_from_slice(b"IHDR");
    out.extend_from_slice(&[0, 0, 0, 16, 0, 0, 0, 16, depth, colour, 0, 0, 0]);
    out.extend_from_slice(&[0; 4]); // CRC, not read
    out.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
    out
}

#[test]
fn the_image_output_checks_read_the_headers() {
    let s = TargetFormat::StaticSticker;
    assert_eq!(check_output(s, &webp(*b"VP8 ", &[0; 10])), Ok(()));
    assert_eq!(check_output(s, &webp(*b"VP8L", &[0; 10])), Ok(()));
    // Extended header: alpha (0x10) is fine, animation (0x02) is not.
    assert_eq!(check_output(s, &webp(*b"VP8X", &[0x10; 10])), Ok(()));
    assert_eq!(
        check_output(s, &webp(*b"VP8X", &[0x12; 10])),
        Err("an animated WebP")
    );
    let mut zero_size = webp(*b"VP8 ", &[0; 10]);
    zero_size[4..8].copy_from_slice(&[0; 4]);
    assert_eq!(
        check_output(s, &zero_size),
        Err("a WebP file whose RIFF size is not its length")
    );
    let file = webp(*b"VP8 ", &[0; 10]);
    assert_eq!(
        check_output(s, &file[..file.len() - 1]),
        Err("a WebP file whose RIFF size is not its length")
    );
    assert_eq!(
        check_output(s, &webp(*b"ALPH", &[0; 10])),
        Err("a WebP file with no image")
    );
    assert_eq!(check_output(s, b"RIFF\x04\0\0\0WAVE"), Err("no WebP file"));
    assert_eq!(check_output(s, &png(8, 6)), Err("no WebP file"));
    assert_eq!(check_output(s, &[]), Err("no WebP file"));

    let j = TargetFormat::Jpeg;
    assert_eq!(check_output(j, &jpeg(8, 3)), Ok(()));
    assert_eq!(
        check_output(j, &jpeg(8, 1)),
        Err("a JPEG that is not in colour")
    );
    assert_eq!(
        check_output(j, &jpeg(8, 4)),
        Err("a JPEG that is not in colour")
    );
    assert_eq!(
        check_output(j, &jpeg(12, 3)),
        Err("a JPEG that is not 8-bit")
    );
    let whole = jpeg(8, 3);
    assert_eq!(
        check_output(j, &whole[..whole.len() - 1]),
        Err("no JPEG file")
    );
    // A scan before any frame header.
    let mut no_frame = vec![0xFF, 0xD8, 0xFF, 0xDA, 0, 2];
    no_frame.extend_from_slice(&[0xFF, 0xD9]);
    assert_eq!(
        check_output(j, &no_frame),
        Err("a JPEG with no frame header")
    );
    assert_eq!(check_output(j, &png(8, 2)), Err("no JPEG file"));

    let p = TargetFormat::Png;
    assert_eq!(check_output(p, &png(8, 2)), Ok(()));
    assert_eq!(check_output(p, &png(8, 6)), Ok(()));
    assert_eq!(check_output(p, &png(16, 6)), Err("a PNG that is not 8-bit"));
    for colour in [0, 3, 4] {
        assert_eq!(
            check_output(p, &png(8, colour)),
            Err("a PNG that is not RGB or RGBA"),
            "colour type {colour}"
        );
    }
    let whole = png(8, 6);
    // Cut short, two images in a row, or anything after the end chunk.
    for bad in [
        whole[..whole.len() - 1].to_vec(),
        [&whole[..], &whole[..]].concat(),
        [&whole[..], b"\0"].concat(),
    ] {
        assert_eq!(
            check_output(p, &bad),
            Err("a PNG file that does not end with its end chunk"),
            "{bad:?}"
        );
    }
    let mut no_header = png(8, 6);
    no_header[12..16].copy_from_slice(b"IDAT");
    assert_eq!(
        check_output(p, &no_header),
        Err("a PNG file without its header")
    );
    assert_eq!(check_output(p, &jpeg(8, 3)), Err("no PNG file"));
}

/// One PNG chunk: length, type, data, a CRC (not read).
fn chunk(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&[0xC0, 0xFF, 0xEE, 0x00]);
    out
}

/// A PNG file of these chunks, after its signature.
fn png_of(chunks: &[Vec<u8>]) -> Vec<u8> {
    [&b"\x89PNG\r\n\x1a\n"[..], &chunks.concat()].concat()
}

/// What ffmpeg gets of a PNG: its chunks up to `IEND`, without text or
/// EXIF (a `zTXt` chunk is what ffmpeg inflates and keeps); the colour
/// profile and an animation's chunks (for the format whitelist to refuse)
/// stay. A JPEG, or a PNG with nothing to take out, is passed on as it is,
/// uncopied; one whose chunks run past its end is refused.
#[test]
fn a_png_reaches_ffmpeg_without_its_text_and_nothing_after_its_end() {
    let ihdr = chunk(*b"IHDR", &[0, 0, 0, 16, 0, 0, 0, 16, 8, 2, 0, 0, 0]);
    let iccp = chunk(*b"iCCP", b"icc\0\0x\x9c");
    let actl = chunk(*b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]);
    let idat = chunk(*b"IDAT", b"x\x9c pixels");
    let iend = chunk(*b"IEND", b"");
    let file = png_of(&[
        ihdr.clone(),
        chunk(*b"tEXt", b"Author\0Jane Doe"),
        iccp.clone(),
        chunk(*b"zTXt", b"Comment\0\0x\x9c bomb"),
        actl.clone(),
        idat.clone(),
        chunk(*b"iTXt", b"Location\0\x01\0\0\0x\x9c"),
        chunk(*b"eXIf", b"MM\0*"),
        iend.clone(),
        // After the end: a second image, then anything.
        ihdr.clone(),
        chunk(*b"zTXt", b"Comment\0\0x\x9c bomb"),
        iend.clone(),
        b"trailing".to_vec(),
    ]);
    let given = image::prepared(&Bytes::from(file)).unwrap();
    assert_eq!(
        &given[..],
        &png_of(&[ihdr.clone(), iccp, actl, idat.clone(), iend.clone()])[..]
    );

    // Nothing to take out: the same bytes, not a copy.
    let clean = Bytes::from(png_of(&[ihdr.clone(), idat.clone(), iend.clone()]));
    let given = image::prepared(&clean).unwrap();
    assert_eq!((given.as_ptr(), given.len()), (clean.as_ptr(), clean.len()));
    // Only an appended tail: a slice of it.
    let tailed = Bytes::from([&clean[..], b"tail"].concat());
    assert_eq!(&image::prepared(&tailed).unwrap()[..], &clean[..]);
    let jpeg = Bytes::from(jpeg(8, 3));
    assert_eq!(image::prepared(&jpeg).unwrap(), jpeg);

    let truncated = [
        png_of(&[ihdr.clone(), idat.clone()]),
        png_of(&[ihdr.clone(), idat.clone(), iend[..iend.len() - 1].to_vec()]),
        png_of(&[ihdr.clone(), idat[..idat.len() - 5].to_vec()]),
        b"\x89PNG\r\n\x1a\n".to_vec(),
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec(),
    ];
    for file in truncated {
        assert_eq!(
            image::prepared(&Bytes::from(file.clone())),
            Err("a PNG whose chunks stop before its end chunk (a truncated file)"),
            "{file:?}"
        );
    }
    let mut huge = png_of(&[ihdr, idat, iend]);
    huge[33..37].copy_from_slice(&0x8000_0000_u32.to_be_bytes());
    assert_eq!(
        image::prepared(&Bytes::from(huge)),
        Err("a PNG chunk longer than PNG allows")
    );
}

#[tokio::test]
async fn a_missing_ffmpeg_is_a_typed_error() {
    let converter = FfmpegConverter::new().with_program("/nonexistent-l19/bin/ffmpeg");
    let err = converter
        .convert(MediaInput::new(vec![1u8; 16]), TargetFormat::OggOpus)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::BinaryMissing(_))),
        "{err:?}"
    );
    assert!(err.to_string().contains("ffmpeg not found"), "{err}");
    assert!(!err.may_have_been_sent() && !err.is_retryable());
    assert_eq!(
        err.kind(),
        meta_whatsapp_core::ErrorKind::FeatureNotAvailable
    );
    let err = converter.check().await.unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::BinaryMissing(_))),
        "{err:?}"
    );
}

#[test]
fn defaults() {
    let c = FfmpegConverter::default();
    assert_eq!(c.program(), Path::new("ffmpeg"));
    assert_eq!(c.timeout, Duration::from_secs(60));
    assert_eq!(c.max_input_bytes, 64 * 1024 * 1024);
}

#[cfg(unix)]
mod processes {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::OnceLock;
    use std::time::Instant;

    use super::*;

    /// A test that hangs fails instead.
    const BOUND: Duration = Duration::from_secs(20);

    struct Fakes {
        dir: PathBuf,
    }

    impl Fakes {
        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }

        fn converter(&self, name: &str) -> FfmpegConverter {
            FfmpegConverter::new().with_program(self.path(name))
        }
    }

    /// The image tests' stand-ins, and the files they answer, written in
    /// `dir`.
    fn image_scripts(dir: &Path) -> Vec<(&'static str, String)> {
        std::fs::write(dir.join("webp.bin"), webp(*b"VP8 ", &[7; 64])).unwrap();
        std::fs::write(dir.join("animated.bin"), webp(*b"VP8X", &[0x12; 10])).unwrap();
        // A static sticker of exactly the limit.
        std::fs::write(
            dir.join("webp_at_limit.bin"),
            webp(*b"VP8 ", &vec![7; 99_980]),
        )
        .unwrap();
        let d = dir.display();
        vec![
            // The image tests' own: `marker` runs in one of the voice
            // tests, which may come first.
            ("image_marker", format!("touch '{d}/image_marker.ran'\n")),
            // Counts its runs and records their arguments, one line
            // each; answers more than a static sticker takes twice,
            // then a WebP.
            (
                "ladder",
                format!(
                    "n=$(cat '{d}/ladder.count' 2>/dev/null || echo 0)\nn=$((n + 1))\necho $n > '{d}/ladder.count'\n\
                     echo \"$*\" >> '{d}/ladder.args'\ncat > /dev/null\n\
                     if [ $n -le 2 ]; then head -c 100001 /dev/zero; else cat '{d}/webp.bin'; fi\n"
                ),
            ),
            // Counts its runs; always answers too much.
            (
                "too_much",
                format!(
                    "n=$(cat '{d}/too_much.count' 2>/dev/null || echo 0)\necho $((n + 1)) > '{d}/too_much.count'\n\
                     cat > /dev/null\nexec cat /dev/zero\n"
                ),
            ),
            // Counts its runs; the first takes 1.2 s and answers too
            // much, the next ones hang.
            (
                "slow_ladder",
                format!(
                    "n=$(cat '{d}/slow_ladder.count' 2>/dev/null || echo 0)\nn=$((n + 1))\necho $n > '{d}/slow_ladder.count'\n\
                     cat > /dev/null\nif [ $n -eq 1 ]; then sleep 1.2; exec cat /dev/zero; fi\nexec sleep 30\n"
                ),
            ),
            // Records its arguments and its input, answers a static
            // sticker of exactly the limit.
            (
                "record_image",
                format!(
                    "printf '%s\\n' \"$@\" >> '{d}/record_image.args'\ncat > '{d}/record_image.stdin'\ncat '{d}/webp_at_limit.bin'\n"
                ),
            ),
            // Records its pid, answers more than a static sticker
            // takes, then hangs on, deaf to the closed pipe: only a
            // kill ends it.
            (
                "overflow_then_hang",
                format!(
                    "echo $$ >> '{d}/overflow_then_hang.pids'\ncat > /dev/null\ntrap '' PIPE\n\
                     head -c 100001 /dev/zero\nexec sleep 30\n"
                ),
            ),
            // Counts its runs; fails.
            (
                "image_fails",
                format!(
                    "n=$(cat '{d}/image_fails.count' 2>/dev/null || echo 0)\necho $((n + 1)) > '{d}/image_fails.count'\n\
                     cat > /dev/null\nexit 1\n"
                ),
            ),
            // Each encoder name, each of the wrong kind.
            (
                "encoders_wrong_kind",
                "printf ' A....D libwebp   libwebp\\n A....D mjpeg   MJPEG\\n A....D png   PNG\\n V....D libopus   libopus\\n'\n"
                    .to_owned(),
            ),
            (
                "animated",
                format!("cat > /dev/null\ncat '{d}/animated.bin'\n"),
            ),
            (
                "encoders_images",
                "printf ' V....D libwebp   libwebp WebP image (codec webp)\\n VFS... mjpeg   MJPEG\\n VF...D png   PNG\\n A....D opus   Opus\\n'\n"
                    .to_owned(),
            ),
        ]
    }

    /// Every stand-in, written once before any test starts one: writing
    /// an executable while another thread forks can make its exec fail
    /// with "text file busy", so no test spawns until all are written.
    fn fakes() -> &'static Fakes {
        static FAKES: OnceLock<Fakes> = OnceLock::new();
        FAKES.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!(
                "meta-whatsapp-adapters-ffmpeg-{}-{}",
                std::process::id(),
                time::OffsetDateTime::now_utc().unix_timestamp_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let d = dir.display();
            std::fs::write(dir.join("opus.bin"), opus_page(1)).unwrap();
            std::fs::write(dir.join("stereo.bin"), opus_page(2)).unwrap();
            let scripts = [
                // Records what it was given, answers mono Ogg Opus.
                (
                    "record",
                    format!(
                        "printf '%s\\n' \"$@\" > '{d}/record.args'\ncat > '{d}/record.stdin'\ncat '{d}/opus.bin'\n"
                    ),
                ),
                // Records its environment, answers mono Ogg Opus.
                (
                    "environment",
                    format!("env > '{d}/environment.env'\ncat > /dev/null\ncat '{d}/opus.bin'\n"),
                ),
                (
                    "stereo",
                    format!("cat > /dev/null\ncat '{d}/stereo.bin'\n"),
                ),
                ("not_opus", "cat > /dev/null\nprintf 'OggS but not really'\n".to_owned()),
                // A diagnostic naming a file, then a megabyte more of
                // them (the pipe must be drained), then failure.
                (
                    "fail_stderr",
                    "printf '/home/alice/private/recording-0042.wav: Invalid data\\n' >&2\n\
                     head -c 1048576 /dev/zero >&2\nexit 1\n"
                        .to_owned(),
                ),
                ("sleep", format!("echo $$ > '{d}/sleep.pid'\nexec sleep 30\n")),
                ("sleep_quietly", "exec sleep 30\n".to_owned()),
                ("flood", "exec cat /dev/zero\n".to_owned()),
                ("signal", "kill -9 $$\n".to_owned()),
                ("marker", format!("touch '{d}/marker.ran'\n")),
                (
                    "encoders_ok",
                    "printf ' V....D libx264   H.264\\n A....D libopus   libopus Opus (codec opus)\\n'\n"
                        .to_owned(),
                ),
                (
                    "encoders_without",
                    "printf ' A....D opus   Opus\\n A....D libvorbis   Vorbis\\n'\n".to_owned(),
                ),
            ];
            for (name, body) in scripts.into_iter().chain(image_scripts(&dir)) {
                let path = dir.join(name);
                std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let plain = dir.join("not_executable");
            std::fs::write(&plain, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
            Fakes { dir }
        })
    }

    async fn convert(c: &FfmpegConverter, input: &[u8]) -> Result<ConvertedMedia> {
        tokio::time::timeout(
            BOUND,
            c.convert(MediaInput::new(input.to_vec()), TargetFormat::OggOpus),
        )
        .await
        .expect("the converter must return within the test's bound")
    }

    #[tokio::test]
    async fn the_process_gets_exactly_the_arguments_and_the_input_on_stdin() {
        let f = fakes();
        let input = b"RIFF\x24\0\0\0WAVEfmt  -i http://example.com/ $(id)".to_vec();
        let out = convert(&f.converter("record"), &input).await.unwrap();
        assert_eq!(&out.data[..], &opus_page(1)[..]);
        assert_eq!(out.format, TargetFormat::OggOpus);
        let args = std::fs::read_to_string(f.path("record.args")).unwrap();
        assert_eq!(args.lines().collect::<Vec<_>>(), OGG_OPUS_ARGS);
        assert_eq!(std::fs::read(f.path("record.stdin")).unwrap(), input);
    }

    /// The process gets `PATH` and nothing else of the test's environment
    /// (`HOME`, `CARGO_*`): no `FFREPORT`, no `AV_LOG_FORCE_*`, no proxy
    /// reaches it. What remains besides is the stand-in shell's own.
    #[tokio::test]
    async fn the_process_gets_path_and_no_other_variable() {
        let f = fakes();
        convert(&f.converter("environment"), b"RIFF").await.unwrap();
        let env = std::fs::read_to_string(f.path("environment.env")).unwrap();
        let names: Vec<&str> = env
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name))
            .collect();
        let shells_own = ["PWD", "OLDPWD", "SHLVL", "_"];
        for name in &names {
            assert!(
                *name == "PATH" || shells_own.contains(name),
                "{name} reached the process: {names:?}"
            );
        }
        let path = std::env::var("PATH").unwrap();
        assert!(env.lines().any(|l| l == format!("PATH={path}")), "{env}");
        // The control: this process does have variables to pass on.
        assert!(
            std::env::vars_os().any(|(k, _)| k != "PATH" && !shells_own.iter().any(|s| k == *s)),
            "the test's environment has nothing but PATH"
        );
    }

    #[tokio::test]
    async fn a_binary_that_cannot_run_is_a_configuration_error() {
        let f = fakes();
        let err = convert(&f.converter("not_executable"), b"x")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::BinaryMissing(_))),
            "{err:?}"
        );
        assert!(err.to_string().contains("not executable"), "{err}");
    }

    #[tokio::test]
    async fn an_input_over_the_bound_is_refused_before_ffmpeg_starts() {
        let f = fakes();
        let c = f.converter("marker").with_max_input_bytes(10);
        let err = convert(&c, &[0u8; 11]).await.unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "input"),
            "{err:?}"
        );
        let err = convert(&c, &[]).await.unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "input"),
            "{err:?}"
        );
        assert!(!f.path("marker.ran").exists(), "ffmpeg was started");
        // At the bound it runs (and this stand-in answers nothing, which is
        // no Ogg Opus).
        let err = convert(&c, &[0u8; 10]).await.unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::OutputInvalid(_))),
            "{err:?}"
        );
        assert!(f.path("marker.ran").exists());
    }

    #[tokio::test]
    async fn a_run_past_the_timeout_is_killed() {
        let f = fakes();
        let c = f
            .converter("sleep")
            .with_timeout(Duration::from_millis(500));
        let started = Instant::now();
        let err = convert(&c, b"RIFF").await.unwrap_err();
        assert_eq!(
            err.conversion(),
            Some(&ConversionError::Timeout(Duration::from_millis(500))),
            "{err:?}"
        );
        // Worth trying again, and nothing went to Meta.
        assert!(err.is_retryable() && !err.may_have_been_sent(), "{err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        let pid = std::fs::read_to_string(f.path("sleep.pid")).unwrap();
        let pid = pid.trim();
        assert!(!pid.is_empty());
        if cfg!(target_os = "linux") {
            // Killed and reaped: no process, not even a zombie.
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "the timed-out ffmpeg ({pid}) is still there"
            );
        }
    }

    #[tokio::test]
    async fn the_diagnostics_never_reach_the_error() {
        let f = fakes();
        let err = convert(&f.converter("fail_stderr"), b"RIFF")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
            "{err:?}"
        );
        for shown in [err.to_string(), format!("{err:?}")] {
            assert!(shown.contains("exit status 1"), "{shown}");
            for leak in ["alice", "private", "recording-0042", "Invalid data"] {
                assert!(!shown.contains(leak), "{leak} leaked: {shown}");
            }
        }
        assert!(!err.may_have_been_sent());
    }

    #[tokio::test]
    async fn output_past_the_upload_limit_is_refused_and_ffmpeg_killed() {
        let f = fakes();
        let started = Instant::now();
        let err = convert(&f.converter("flood"), b"RIFF").await.unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "input"),
            "{err:?}"
        );
        assert!(err.to_string().contains("16777216 bytes"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn output_that_is_not_mono_ogg_opus_is_refused() {
        let f = fakes();
        for (name, found) in [
            ("stereo", "Opus with more than one channel"),
            ("not_opus", "no Ogg stream"),
        ] {
            let err = convert(&f.converter(name), b"RIFF").await.unwrap_err();
            assert!(
                matches!(&err, Error::Conversion(ConversionError::OutputInvalid(_))),
                "{name}: {err:?}"
            );
            assert!(err.to_string().contains(found), "{name}: {err}");
        }
    }

    #[tokio::test]
    async fn a_signal_is_no_input_error() {
        let f = fakes();
        let err = convert(&f.converter("signal"), b"RIFF").await.unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::Killed(_))),
            "{err:?}"
        );
        assert!(err.to_string().contains("killed by a signal"), "{err}");
    }

    #[tokio::test]
    async fn check_wants_libopus() {
        let f = fakes();
        f.converter("encoders_ok").check().await.unwrap();
        let err = f.converter("encoders_without").check().await.unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::EncoderMissing(_))),
            "{err:?}"
        );
        assert!(err.to_string().contains("no libopus encoder"), "{err}");
        let err = f
            .converter("sleep_quietly")
            .with_timeout(Duration::from_millis(300))
            .check()
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::Timeout(_))),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn check_for_wants_each_format_s_encoder() {
        let f = fakes();
        let images = [
            TargetFormat::StaticSticker,
            TargetFormat::Jpeg,
            TargetFormat::Png,
        ];
        f.converter("encoders_images")
            .check_for(&images)
            .await
            .unwrap();
        f.converter("encoders_images").check_for(&[]).await.unwrap();
        let err = f
            .converter("encoders_ok")
            .check_for(&images)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::EncoderMissing(_))),
            "{err:?}"
        );
        assert!(err.to_string().contains("no libwebp encoder"), "{err}");
        // `opus` (ffmpeg's own) is not libopus.
        let err = f
            .converter("encoders_images")
            .check_for(&[TargetFormat::Png, TargetFormat::OggOpus])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no libopus encoder"), "{err}");
        let err = f.converter("encoders_images").check().await.unwrap_err();
        assert!(err.to_string().contains("no libopus encoder"), "{err}");
    }

    async fn convert_to(
        c: &FfmpegConverter,
        input: &[u8],
        target: TargetFormat,
    ) -> Result<ConvertedMedia> {
        tokio::time::timeout(BOUND, c.convert(MediaInput::new(input.to_vec()), target))
            .await
            .expect("the converter must return within the test's bound")
    }

    /// A PNG's signature and end chunk: what passes the converter's own
    /// checks, for a stand-in to answer.
    const PNG_START: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xaeB`\x82";

    /// A GIF (or anything not a PNG or a JPEG) is refused by its first
    /// bytes: `ffmpeg` never starts.
    #[tokio::test]
    async fn an_image_off_the_list_never_reaches_ffmpeg() {
        let f = fakes();
        let c = f.converter("image_marker");
        for target in [
            TargetFormat::StaticSticker,
            TargetFormat::Jpeg,
            TargetFormat::Png,
        ] {
            for input in [&b"GIF89a\x01\0\x01\0\0\0\0;"[..], b"RIFF\x1a\0\0\0WEBPVP8 "] {
                let err = convert_to(&c, input, target).await.unwrap_err();
                assert!(
                    matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
                    "{err:?}"
                );
                assert!(err.to_string().contains("not a PNG's or a JPEG's"), "{err}");
            }
        }
        assert!(!f.path("image_marker.ran").exists(), "ffmpeg was started");
    }

    /// Too large, too large, then within the limit: the third attempt's
    /// result, after the ladder's first three command lines, in order.
    #[tokio::test]
    async fn the_ladder_steps_down_until_the_result_fits() {
        let f = fakes();
        let out = convert_to(
            &f.converter("ladder"),
            PNG_START,
            TargetFormat::StaticSticker,
        )
        .await
        .unwrap();
        assert_eq!(&out.data[..], &webp(*b"VP8 ", &[7; 64])[..]);
        assert_eq!(out.format, TargetFormat::StaticSticker);
        let runs = std::fs::read_to_string(f.path("ladder.args")).unwrap();
        let expected: Vec<String> = image::STICKER[..3]
            .iter()
            .map(|rung| image::arguments(rung).join(" "))
            .collect();
        assert_eq!(runs.lines().collect::<Vec<_>>(), expected);
    }

    #[tokio::test]
    async fn every_attempt_over_the_limit_is_a_validation_error() {
        let f = fakes();
        let started = Instant::now();
        let err = convert_to(
            &f.converter("too_much"),
            PNG_START,
            TargetFormat::StaticSticker,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "input"),
            "{err:?}"
        );
        assert!(
            err.to_string().contains("each of the 6 sizes")
                && err.to_string().contains("100000 bytes"),
            "{err}"
        );
        let runs = std::fs::read_to_string(f.path("too_much.count")).unwrap();
        assert_eq!(runs.trim(), image::STICKER.len().to_string());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
    }

    /// The timeout bounds the whole ladder, not each attempt: after a first
    /// attempt of 1.2 s, the second gets what is left of 2 s (and is killed
    /// at 2 s), not 2 s more.
    #[tokio::test]
    async fn the_timeout_covers_every_attempt_together() {
        let f = fakes();
        let c = f
            .converter("slow_ladder")
            .with_timeout(Duration::from_secs(2));
        let started = Instant::now();
        let err = convert_to(&c, PNG_START, TargetFormat::StaticSticker)
            .await
            .unwrap_err();
        assert_eq!(
            err.conversion(),
            Some(&ConversionError::Timeout(Duration::from_secs(2))),
            "{err:?}"
        );
        let runs = std::fs::read_to_string(f.path("slow_ladder.count")).unwrap();
        assert_eq!(runs.trim(), "2", "the second attempt ran");
        assert!(
            started.elapsed() < Duration::from_millis(2800),
            "{:?}",
            started.elapsed()
        );
    }

    /// ffmpeg reads the PNG without its text chunks and what follows its
    /// end, and a result of exactly the limit is taken from the first
    /// attempt; a truncated PNG never reaches ffmpeg.
    #[tokio::test]
    async fn ffmpeg_gets_the_png_without_its_text_and_the_limit_is_inclusive() {
        let f = fakes();
        let ihdr = chunk(*b"IHDR", &[0, 0, 0, 16, 0, 0, 0, 16, 8, 6, 0, 0, 0]);
        let idat = chunk(*b"IDAT", b"x\x9c pixels");
        let iend = chunk(*b"IEND", b"");
        let input = [
            png_of(&[
                ihdr.clone(),
                chunk(*b"zTXt", b"Comment\0\0x\x9c bomb"),
                idat.clone(),
                iend.clone(),
            ]),
            b"appended".to_vec(),
        ]
        .concat();
        let out = convert_to(
            &f.converter("record_image"),
            &input,
            TargetFormat::StaticSticker,
        )
        .await
        .unwrap();
        assert_eq!(out.data.len(), 100_000);
        assert_eq!(&out.data[..], &webp(*b"VP8 ", &vec![7; 99_980])[..]);
        assert_eq!(
            std::fs::read(f.path("record_image.stdin")).unwrap(),
            png_of(&[ihdr.clone(), idat.clone(), iend])
        );
        let args = std::fs::read_to_string(f.path("record_image.args")).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            image::arguments(&image::STICKER[0]),
            "one attempt"
        );

        let c = f.converter("image_marker");
        let err = convert_to(&c, &png_of(&[ihdr, idat]), TargetFormat::Png)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
            "{err:?}"
        );
        assert!(err.to_string().contains("truncated"), "{err}");
        assert!(!f.path("image_marker.ran").exists(), "ffmpeg was started");
    }

    /// Every attempt over the limit is killed and reaped before the next
    /// one starts or the conversion returns, even one that outlives its
    /// closed pipe: no process is left, not even a zombie.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_attempt_over_the_limit_is_killed_and_reaped() {
        let f = fakes();
        let err = convert_to(
            &f.converter("overflow_then_hang"),
            PNG_START,
            TargetFormat::StaticSticker,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "input"),
            "{err:?}"
        );
        let pids = std::fs::read_to_string(f.path("overflow_then_hang.pids")).unwrap();
        let pids: Vec<&str> = pids.lines().collect();
        assert_eq!(pids.len(), image::STICKER.len());
        for pid in pids {
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "the attempt killed on overflow ({pid}) is still there"
            );
        }
    }

    /// ffmpeg failing on an image is the input's fault: no other attempt
    /// runs.
    #[tokio::test]
    async fn a_failed_attempt_ends_the_ladder() {
        let f = fakes();
        let err = convert_to(&f.converter("image_fails"), PNG_START, TargetFormat::Jpeg)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
            "{err:?}"
        );
        assert!(err.to_string().contains("exit status 1"), "{err}");
        let runs = std::fs::read_to_string(f.path("image_fails.count")).unwrap();
        assert_eq!(runs.trim(), "1");
    }

    /// An encoder of the right name but the wrong kind (`A` for audio, `V`
    /// for images) is not the one a format needs.
    #[tokio::test]
    async fn check_for_wants_the_encoder_s_kind_too() {
        let f = fakes();
        for target in [
            TargetFormat::StaticSticker,
            TargetFormat::Jpeg,
            TargetFormat::Png,
            TargetFormat::OggOpus,
        ] {
            let err = f
                .converter("encoders_wrong_kind")
                .check_for(&[target])
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::Conversion(ConversionError::EncoderMissing(_))),
                "{target:?}: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_animated_webp_is_no_static_sticker() {
        let f = fakes();
        let err = convert_to(
            &f.converter("animated"),
            PNG_START,
            TargetFormat::StaticSticker,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::OutputInvalid(_))),
            "{err:?}"
        );
        assert!(err.to_string().contains("an animated WebP"), "{err}");
        assert!(err.to_string().contains("libwebp"), "{err}");
    }
}
