//! `adapters::media::FfmpegConverter` against a real `ffmpeg`, its result
//! through `client::media::Media::upload_converted` (roadmap L19's decisive
//! test). In the facade, where the adapters and the client meet: neither
//! depends on the other.
//!
//! The binary is `META_WHATSAPP_RS_TEST_FFMPEG` when set, else `ffmpeg` on
//! `PATH` (and `META_WHATSAPP_RS_TEST_FFPROBE`, else `ffprobe`). Without
//! one that starts and has libopus, every test here skips, printing why,
//! unless `META_WHATSAPP_RS_REQUIRE_FFMPEG=1`, which turns the skip into a
//! failure. CI installs `ffmpeg` on its runner and sets it, so the tests
//! count there; no image or dev container carries one. That variable is
//! not `META_WHATSAPP_RS_REQUIRE_LIVE`, and no test name here contains
//! `live_`: `just test-live` requires Postgres and Redis, not an `ffmpeg`.
#![cfg(feature = "ffmpeg")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::adapters::media::FfmpegConverter;
use meta_whatsapp_rs::client::media::{
    MediaConverter, MediaInput, MediaKind, TargetFormat, validate_upload,
};
use meta_whatsapp_rs::core::error::ConversionError;
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::core::testing::{RecordedBody, ScriptedTransport};
use serde_json::json;

fn required() -> bool {
    std::env::var("META_WHATSAPP_RS_REQUIRE_FFMPEG").is_ok_and(|v| v == "1")
}

fn program(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).map_or_else(|| PathBuf::from(default), PathBuf::from)
}

/// The converter over the test's `ffmpeg`, or `None` (skip) without one.
///
/// # Panics
///
/// Without one when `META_WHATSAPP_RS_REQUIRE_FFMPEG=1`.
async fn converter() -> Option<FfmpegConverter> {
    let c = FfmpegConverter::new().with_program(program("META_WHATSAPP_RS_TEST_FFMPEG", "ffmpeg"));
    match c.check().await {
        Ok(()) => Some(c),
        Err(e) if required() => panic!(
            "{e}; META_WHATSAPP_RS_REQUIRE_FFMPEG=1 turns a skipped ffmpeg test into a failure"
        ),
        Err(e) => {
            eprintln!(
                "skipping: {e} (set META_WHATSAPP_RS_TEST_FFMPEG; META_WHATSAPP_RS_REQUIRE_FFMPEG=1 makes this a failure)"
            );
            None
        }
    }
}

/// Run a tool directly (the tests' own controls, not the converter), with
/// `input` on stdin; its stdout, or `None` when it fails.
fn run(program: &PathBuf, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().ok()?;
    writer.join().unwrap();
    out.status.success().then_some(out.stdout)
}

/// One second of a 440 Hz tone: 16-bit PCM, stereo, 44.1 kHz, as a WAV
/// file (a phone's or a browser's recording, before conversion).
fn wav() -> Vec<u8> {
    const RATE: u32 = 44_100;
    const CHANNELS: u16 = 2;
    let samples: Vec<i16> = (0..RATE)
        .flat_map(|n| {
            let t = f64::from(n) / f64::from(RATE);
            #[allow(clippy::cast_possible_truncation)]
            let s = ((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16;
            [s; CHANNELS as usize]
        })
        .collect();
    let data_len = u32::try_from(samples.len() * 2).unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * u32::from(CHANNELS) * 2).to_le_bytes());
    out.extend_from_slice(&(CHANNELS * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// The same tone as a Sun AU file: audio ffmpeg reads, in a format off
/// the converter's list.
fn au() -> Vec<u8> {
    let pcm: Vec<u8> = wav()[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .flat_map(|s| i16::from_le_bytes(*s).to_be_bytes())
        .collect();
    let mut out = b".snd".to_vec();
    for word in [24u32, u32::try_from(pcm.len()).unwrap(), 3, 44_100, 2] {
        out.extend_from_slice(&word.to_be_bytes());
    }
    out.extend_from_slice(&pcm);
    out
}

/// The first Ogg page's packet, read here apart from the converter's own
/// check (RFC 3533 § 6, RFC 7845 § 5.1).
fn opus_head(ogg: &[u8]) -> &[u8] {
    assert_eq!(&ogg[..4], b"OggS", "not an Ogg stream");
    assert_eq!(
        ogg[5] & 0x02,
        0x02,
        "the first page does not begin the stream"
    );
    let segments = usize::from(ogg[26]);
    let len: usize = ogg[27..27 + segments].iter().map(|&b| usize::from(b)).sum();
    &ogg[27 + segments..27 + segments + len]
}

#[tokio::test]
async fn ffmpeg_turns_wav_into_ogg_opus_that_upload_accepts() {
    let Some(converter) = converter().await else {
        return;
    };
    let input = wav();
    let note = converter
        .convert(MediaInput::new(input.clone()), TargetFormat::OggOpus)
        .await
        .unwrap();
    assert_eq!(note.format, TargetFormat::OggOpus);

    // Ogg, its first packet an Opus header for one channel (Meta: "OPUS
    // codecs only", "mono input only").
    let head = opus_head(&note.data);
    assert_eq!(&head[..8], b"OpusHead");
    assert_eq!(head[9], 1, "channels");
    assert!(
        note.data.len() < input.len() / 10,
        "{} bytes",
        note.data.len()
    );

    // ffprobe agrees, when it is there.
    let ffprobe = program("META_WHATSAPP_RS_TEST_FFPROBE", "ffprobe");
    let probe = run(
        &ffprobe,
        &[
            "-v",
            "error",
            "-show_entries",
            "format=format_name:stream=codec_name,channels",
            "-of",
            "compact=p=0:nk=1",
            "pipe:0",
        ],
        &note.data,
    );
    match probe {
        Some(out) => {
            let out = String::from_utf8(out).unwrap();
            let lines: Vec<&str> = out.lines().collect();
            assert_eq!(lines, ["opus|1", "ogg"], "ffprobe says {out}");
        }
        None if required() => panic!("{} did not run", ffprobe.display()),
        None => eprintln!("ffprobe not run: {} did not start", ffprobe.display()),
    }

    // Media::upload's type check takes it, and upload_converted sends it as
    // audio/ogg, named after its format.
    assert_eq!(
        validate_upload(
            note.format.mime_type(),
            u64::try_from(note.data.len()).unwrap()
        ),
        Ok(MediaKind::Audio)
    );
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": "1037543291543636"}));
    let client = Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .build()
        .unwrap();
    let id = client
        .media(PhoneNumberId::new("106540352242922").unwrap())
        .upload_converted(note.clone(), "note")
        .await
        .unwrap();
    assert_eq!(id.as_str(), "1037543291543636");
    let req = t.last_request().unwrap();
    assert_eq!(req.method.as_str(), "POST");
    assert_eq!(req.path(), "/v25.0/106540352242922/media");
    assert_eq!(req.bearer(), Some("TOKEN"));
    let (_, _, product) = req.multipart_field("messaging_product").unwrap();
    assert_eq!(&product[..], b"whatsapp");
    let (_, _, kind) = req.multipart_field("type").unwrap();
    assert_eq!(&kind[..], b"audio/ogg");
    let (name, ctype, data) = req.multipart_field("file").unwrap();
    assert_eq!((name, ctype), (Some("note.ogg"), Some("audio/ogg")));
    assert_eq!(data, &note.data);
    let RecordedBody::Multipart(parts) = &req.body else {
        panic!("expected multipart");
    };
    assert_eq!(parts.len(), 3);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn ffmpeg_reads_browser_and_phone_recordings() {
    let Some(converter) = converter().await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    for (what, args) in [
        // Chrome's MediaRecorder: WebM with Opus, stereo here.
        ("webm", &["-f", "webm", "-c:a", "libopus"][..]),
        // An iPhone voice memo: AAC in M4A (MP4); `frag_keyframe` makes
        // it writable to a pipe.
        (
            "m4a",
            &[
                "-f",
                "mp4",
                "-c:a",
                "aac",
                "-movflags",
                "frag_keyframe+empty_moov",
            ][..],
        ),
        ("mp3", &["-f", "mp3"][..]),
    ] {
        let mut full = vec![
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "wav",
            "-i",
            "pipe:0",
        ];
        full.extend_from_slice(args);
        full.push("pipe:1");
        let recording = run(&ffmpeg, &full, &wav()).unwrap_or_else(|| panic!("making {what}"));
        let note = converter
            .convert(MediaInput::new(recording), TargetFormat::OggOpus)
            .await
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        let head = opus_head(&note.data);
        assert_eq!((&head[..8], head[9]), (&b"OpusHead"[..], 1), "{what}");
    }
}

#[tokio::test]
async fn ffmpeg_refuses_a_format_off_its_list() {
    let Some(converter) = converter().await else {
        return;
    };
    let file = au();
    // The control: ffmpeg itself reads this file and converts it.
    let ffmpeg = converter.program().to_path_buf();
    let direct = run(
        &ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-ac",
            "1",
            "-c:a",
            "libopus",
            "-f",
            "ogg",
            "pipe:1",
        ],
        &file,
    );
    assert!(
        direct.is_some_and(|o| o.starts_with(b"OggS")),
        "the AU file is not valid"
    );
    // The converter does not: AU is no recording format it lists.
    let err = converter
        .convert(MediaInput::new(file), TargetFormat::OggOpus)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?}"
    );
}

#[tokio::test]
async fn ffmpeg_refuses_input_it_cannot_read() {
    let Some(converter) = converter().await else {
        return;
    };
    let noise: Vec<u8> = (0..4096u32)
        .map(|i| i.wrapping_mul(2_654_435_761).to_le_bytes()[2])
        .collect();
    for input in [
        noise,
        b"ffconcat version 1.0\nfile '/etc/hostname'\n".to_vec(),
    ] {
        let err = converter
            .convert(MediaInput::new(input), TargetFormat::OggOpus)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
            "{err:?}"
        );
        assert!(!err.to_string().contains("/etc/hostname"), "{err}");
    }
}
