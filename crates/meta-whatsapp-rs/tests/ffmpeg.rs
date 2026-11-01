//! `adapters::media::FfmpegConverter` against a real `ffmpeg`, its result
//! through `client::media::Media::upload_converted` (roadmap L19's,
//! L19b's and L19c's decisive tests: voice, then stickers and images, then
//! animated stickers). In the facade, where the adapters and the client
//! meet: neither depends on the other. The tests make their inputs
//! themselves (in Rust, or with ffmpeg's `lavfi` sources); no media file
//! is committed.
//!
//! The binary is `META_WHATSAPP_RS_TEST_FFMPEG` when set, else `ffmpeg` on
//! `PATH` (and `META_WHATSAPP_RS_TEST_FFPROBE`, else `ffprobe`). Without
//! one that starts and has libopus (and, for the image tests, libwebp,
//! mjpeg and png; for the animation tests, `libwebp_anim`), every test here
//! skips, printing why,
//! unless `META_WHATSAPP_RS_REQUIRE_FFMPEG=1`, which turns the skip into a
//! failure. CI installs `ffmpeg` on its runner and sets it, so the tests
//! count there; no image or dev container carries one. That variable is
//! not `META_WHATSAPP_RS_REQUIRE_LIVE`, and no test name here contains
//! `live_`: `just test-live` requires Postgres and Redis, not an `ffmpeg`.
#![cfg(feature = "ffmpeg")]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

/// The tests that assert a wall-clock bound take the CPU alone: run beside
/// the animation tests (each an ffmpeg using every core), a conversion the
/// CI runner finishes in about 11 s took over 15. Every other test shares.
static CPU: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

/// A share of the CPU, beside the other tests that share it.
async fn cpu_shared() -> tokio::sync::RwLockReadGuard<'static, ()> {
    CPU.read().await
}

/// The CPU alone, for a test that asserts how long a conversion takes.
async fn cpu_alone() -> tokio::sync::RwLockWriteGuard<'static, ()> {
    CPU.write().await
}

/// The converter over the test's `ffmpeg`, or `None` (skip) without one.
///
/// # Panics
///
/// Without one when `META_WHATSAPP_RS_REQUIRE_FFMPEG=1`.
async fn converter() -> Option<FfmpegConverter> {
    converter_for(&[TargetFormat::OggOpus]).await
}

/// The converter over the test's `ffmpeg`, checked for `targets`' encoders,
/// or `None` (skip) without them.
///
/// # Panics
///
/// Without them when `META_WHATSAPP_RS_REQUIRE_FFMPEG=1`.
async fn converter_for(targets: &[TargetFormat]) -> Option<FfmpegConverter> {
    let c = FfmpegConverter::new().with_program(program("META_WHATSAPP_RS_TEST_FFMPEG", "ffmpeg"));
    match c.check_for(targets).await {
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
/// `input` on stdin, in the system's temporary directory; its stdout, or
/// `None` when it fails.
fn run(program: &PathBuf, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(std::env::temp_dir())
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
    let _cpu = cpu_shared().await;
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
    let _cpu = cpu_shared().await;
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
    let _cpu = cpu_shared().await;
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
    let _cpu = cpu_shared().await;
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

/// A directory of one test's own under the system's temporary directory,
/// removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "meta-whatsapp-rs-ffmpeg-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A WAV header for `data`: 16-bit PCM, `channels` at `rate` Hz.
fn wav_of(channels: u16, rate: u32, data: &[u8]) -> Vec<u8> {
    let data_len = u32::try_from(data.len()).unwrap();
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(data);
    out
}

fn be32(b: &[u8], at: usize) -> usize {
    usize::try_from(u32::from_be_bytes(b[at..at + 4].try_into().unwrap())).unwrap()
}

/// The offset and size of the first `kind` atom in `b[from..to]`.
fn atom(b: &[u8], from: usize, to: usize, kind: [u8; 4]) -> (usize, usize) {
    let mut at = from;
    while at < to {
        let size = be32(b, at);
        if b[at + 4..at + 8] == kind {
            return (at, size);
        }
        at += size;
    }
    panic!("no {} atom", String::from_utf8_lossy(&kind));
}

/// `mov` (its index first) with its one data reference replaced by a Mac
/// alias (`alis`) naming `target` by absolute path: the track's samples
/// are then to be read from `target`, at the offsets the index gives.
fn with_external_alias(mov: &[u8], target: &Path) -> Vec<u8> {
    let path = target.to_str().unwrap().as_bytes();
    assert!(!path.contains(&b':'), "ffmpeg reads ':' in an alias as '/'");
    let name = target.file_name().unwrap().to_str().unwrap().as_bytes();
    // The alias record as ffmpeg's `mov` reader parses it: an empty volume
    // name, the file name, one level up and down (0: absolute only), then
    // tagged fields, type 2 being the absolute path, -1 the end.
    let mut alias = vec![0u8; 10];
    alias.push(0);
    alias.extend_from_slice(&[0; 27]);
    alias.extend_from_slice(&[0; 12]);
    alias.push(u8::try_from(name.len()).unwrap());
    let mut padded = name.to_vec();
    padded.resize(63, 0);
    alias.extend_from_slice(&padded);
    alias.extend_from_slice(&[0; 16]);
    alias.extend_from_slice(&[0, 0, 0, 0]);
    alias.extend_from_slice(&[0; 16]);
    alias.extend_from_slice(&2u16.to_be_bytes());
    alias.extend_from_slice(&u16::try_from(path.len()).unwrap().to_be_bytes());
    alias.extend_from_slice(path);
    if path.len() % 2 == 1 {
        alias.push(0);
    }
    alias.extend_from_slice(&[0xff, 0xff, 0, 0]);
    let mut entry = u32::try_from(12 + alias.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    entry.extend_from_slice(b"alis");
    entry.extend_from_slice(&[0; 4]); // flags 0: the data is elsewhere
    entry.extend_from_slice(&alias);
    let mut dref = u32::try_from(16 + entry.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    dref.extend_from_slice(b"dref");
    dref.extend_from_slice(&[0; 4]);
    dref.extend_from_slice(&1u32.to_be_bytes());
    dref.extend_from_slice(&entry);

    let mut chain = Vec::new();
    let (mut from, mut to) = (0, mov.len());
    for kind in [*b"moov", *b"trak", *b"mdia", *b"minf", *b"dinf", *b"dref"] {
        let (at, size) = atom(mov, from, to, kind);
        chain.push((at, size));
        (from, to) = (at + 8, at + size);
    }
    let (at, size) = chain.pop().unwrap();
    let grown = dref.len() - size;
    let mut out = [&mov[..at], &dref[..], &mov[at + size..]].concat();
    for &(parent, parent_size) in &chain {
        out[parent..parent + 4]
            .copy_from_slice(&u32::try_from(parent_size + grown).unwrap().to_be_bytes());
    }
    // The media data moved by as much: shift the chunk offsets.
    let (index, index_size) = atom(&out, 0, out.len(), *b"moov");
    let stco = index
        + out[index..index + index_size]
            .windows(4)
            .position(|w| w == b"stco")
            .unwrap()
        - 4;
    for k in 0..be32(&out, stco + 12) {
        let at = stco + 16 + 4 * k;
        let shifted = u32::try_from(be32(&out, at) + grown).unwrap();
        out[at..at + 4].copy_from_slice(&shifted.to_be_bytes());
    }
    out
}

/// An MP4 whose samples are in another file, named by absolute path (a
/// `dref` alias), is converted from its own bytes only: the file never
/// reaches the result. The control shows that this input does name a
/// readable file, which ffmpeg reads once told to follow references and
/// allowed `file:`; the converter gives it neither, and the reference's
/// silence of its own is no audio.
#[tokio::test]
async fn ffmpeg_never_reads_a_file_an_mp4_names() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter().await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let dir = Scratch::new("dref");
    // The file named: two seconds of the tone, raw, mono.
    let tone: Vec<u8> = wav()[44..]
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|frame| [frame[0], frame[1]])
        .collect();
    let secret = dir.path("secret.pcm");
    std::fs::write(&secret, [&tone[..], &tone[..]].concat()).unwrap();
    // A MOV of one second of silence, its index first.
    let base = dir.path("base.mov");
    run(
        &ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "wav",
            "-i",
            "pipe:0",
            "-c:a",
            "pcm_s16le",
            "-movflags",
            "+faststart",
            "-f",
            "mov",
            base.to_str().unwrap(),
        ],
        &wav_of(1, 44_100, &vec![0; 88_200]),
    )
    .expect("making the MOV");
    let crafted = with_external_alias(&std::fs::read(&base).unwrap(), &secret);

    // The control: told to follow it, and allowed `file:`, ffmpeg reads the
    // named file and converts the tone (a second of Opus, kilobytes).
    let followed = run(
        &ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "pipe,file",
            "-enable_drefs",
            "1",
            "-use_absolute_path",
            "1",
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
        &crafted,
    )
    .expect("the control run");
    assert!(
        followed.len() > 2000,
        "the control read {} bytes",
        followed.len()
    );

    let err = converter
        .convert(MediaInput::new(crafted), TargetFormat::OggOpus)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?}"
    );
    assert!(!err.to_string().contains("secret"), "{err}");
}

/// An input that decodes to no audio is refused, not returned as an empty
/// voice note.
#[tokio::test]
async fn ffmpeg_refuses_input_with_no_audio() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter().await else {
        return;
    };
    let err = converter
        .convert(
            MediaInput::new(wav_of(1, 48_000, &[])),
            TargetFormat::OggOpus,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?}"
    );
}

/// How long an Ogg Opus stream plays: its last page's granule position
/// less the header's pre-skip, at 48 kHz (RFC 7845 § 4).
fn ogg_opus_duration(ogg: &[u8]) -> Duration {
    let pre_skip = u16::from_le_bytes([opus_head(ogg)[10], opus_head(ogg)[11]]);
    let (mut at, mut granule) = (0, 0u64);
    while at + 27 <= ogg.len() {
        assert_eq!(&ogg[at..at + 4], b"OggS", "a page at {at}");
        granule = u64::from_le_bytes(ogg[at + 6..at + 14].try_into().unwrap());
        let segments = usize::from(ogg[at + 26]);
        let body: usize = ogg[at + 27..at + 27 + segments]
            .iter()
            .map(|&b| usize::from(b))
            .sum();
        at += 27 + segments + body;
    }
    let samples = granule.saturating_sub(u64::from(pre_skip));
    Duration::from_secs_f64(f64::from(u32::try_from(samples).unwrap()) / 48_000.0)
}

/// An M4A whose index follows its media data, as many recorders (and
/// ffmpeg by default) write one: from a pipe, ffmpeg itself gets next to
/// nothing out of it (the control), since it finds the index only past
/// the samples and cannot seek back; the converter moves the index first
/// and converts all twenty seconds.
#[tokio::test]
async fn ffmpeg_reads_an_m4a_whose_index_comes_last() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter().await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let dir = Scratch::new("index-last");
    let m4a = dir.path("index-last.m4a");
    let tone = wav();
    let twenty: Vec<u8> = tone[44..].repeat(20);
    run(
        &ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "wav",
            "-i",
            "pipe:0",
            "-c:a",
            "aac",
            "-f",
            "mp4",
            m4a.to_str().unwrap(),
        ],
        &wav_of(2, 44_100, &twenty),
    )
    .expect("making the M4A");
    let file = std::fs::read(&m4a).unwrap();
    let moov = file.windows(4).position(|w| w == b"moov").unwrap();
    let mdat = file.windows(4).position(|w| w == b"mdat").unwrap();
    assert!(mdat < moov, "the M4A has its index first");

    let control = run(
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
    )
    .expect("the control run");
    let read = ogg_opus_duration(&control);
    assert!(
        read < Duration::from_secs(1),
        "ffmpeg read {read:?} from the pipe"
    );

    let note = converter
        .convert(MediaInput::new(file), TargetFormat::OggOpus)
        .await
        .unwrap();
    let played = ogg_opus_duration(&note.data);
    assert!(
        played > Duration::from_millis(19_500) && played < Duration::from_millis(20_500),
        "{played:?}"
    );
}

/// A 16 KiB WAV declaring one sample a second: resampled to 48 kHz, each
/// of its frames would be a buffer of hundreds of megabytes, and the
/// whole more than two hours of audio. It is refused before the timeout:
/// by `-max_alloc` at once where the build makes such a buffer (ffmpeg
/// 6.1.1 here), or by the result cap where the build's buffers stay under
/// the bound (the CI runner's ffmpeg reached the cap in about 11 s).
#[tokio::test]
async fn ffmpeg_refuses_a_header_that_inflates_the_input() {
    let _cpu = cpu_alone().await;
    let Some(converter) = converter().await else {
        return;
    };
    let converter = converter.with_timeout(Duration::from_secs(15));
    let data: Vec<u8> = (0..16_384u32).map(|i| i.to_le_bytes()[0]).collect();
    let started = Instant::now();
    let err = converter
        .convert(MediaInput::new(wav_of(1, 1, &data)), TargetFormat::OggOpus)
        .await
        .unwrap_err();
    let refused = matches!(&err, Error::Conversion(ConversionError::InputRejected(_)))
        || matches!(&err, Error::Validation(v) if v.field == "input");
    assert!(refused, "{err:?} after {:?}", started.elapsed());
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
}

// Images (roadmap L19b).

/// The image formats the converter makes.
const IMAGES: [TargetFormat; 3] = [
    TargetFormat::StaticSticker,
    TargetFormat::Jpeg,
    TargetFormat::Png,
];

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// One frame of a `lavfi` source, encoded with `encoder`: the tests'
/// images, made here rather than committed.
fn lavfi(ffmpeg: &PathBuf, source: &str, encoder: &[&str]) -> Vec<u8> {
    let mut args = vec![
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        source,
        "-frames:v",
        "1",
    ];
    args.extend_from_slice(encoder);
    args.extend_from_slice(&["-f", "image2pipe", "pipe:1"]);
    run(ffmpeg, &args, &[]).unwrap_or_else(|| panic!("making {source}"))
}

fn le16(b: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn le24(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], 0])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// A WebP file, read here apart from the converter's own check (Google's
/// "WebP Container Specification"): its chunks, its size, and whether it
/// has an alpha channel.
#[derive(Debug)]
struct Webp {
    chunks: Vec<[u8; 4]>,
    width: u32,
    height: u32,
    alpha: bool,
}

fn webp(file: &[u8]) -> Webp {
    assert_eq!(&file[..4], b"RIFF", "not a RIFF file");
    assert_eq!(&file[8..12], b"WEBP", "not a WebP file");
    assert_eq!(
        usize::try_from(le32(file, 4)).unwrap() + 8,
        file.len(),
        "the RIFF size is not the file's"
    );
    let mut chunks = Vec::new();
    let mut at = 12;
    while at < file.len() {
        chunks.push(<[u8; 4]>::try_from(&file[at..at + 4]).unwrap());
        let len = usize::try_from(le32(file, at + 4)).unwrap();
        at += 8 + len + len % 2;
    }
    assert_eq!(at, file.len(), "chunks past the end");
    let data = &file[20..];
    let (width, height, alpha) = match &chunks[0] {
        b"VP8 " => {
            assert_eq!(&data[3..6], &[0x9d, 0x01, 0x2a], "no VP8 start code");
            (le16(data, 6) & 0x3fff, le16(data, 8) & 0x3fff, false)
        }
        b"VP8L" => {
            assert_eq!(data[0], 0x2f, "no VP8L signature");
            let bits = le32(data, 1);
            (
                (bits & 0x3fff) + 1,
                ((bits >> 14) & 0x3fff) + 1,
                bits >> 28 & 1 == 1,
            )
        }
        b"VP8X" => {
            assert_eq!(data[0] & 0x02, 0, "the animation flag is set");
            (1 + le24(data, 4), 1 + le24(data, 7), data[0] & 0x10 != 0)
        }
        other => panic!("first chunk {}", String::from_utf8_lossy(other)),
    };
    Webp {
        chunks,
        width,
        height,
        alpha,
    }
}

/// A JPEG's frame header, read here: precision, width, height and
/// component count.
fn jpeg_frame(file: &[u8]) -> (u8, u32, u32, u8) {
    assert_eq!(&file[..2], &[0xFF, 0xD8], "no start of image");
    assert_eq!(&file[file.len() - 2..], &[0xFF, 0xD9], "no end of image");
    let mut at = 2;
    loop {
        assert_eq!(file[at], 0xFF, "a marker at {at}");
        let marker = file[at + 1];
        let len = usize::from(u16::from_be_bytes([file[at + 2], file[at + 3]]));
        if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
            let be = |i: usize| u32::from(u16::from_be_bytes([file[at + i], file[at + i + 1]]));
            return (file[at + 4], be(7), be(5), file[at + 9]);
        }
        assert_ne!(marker, 0xDA, "a scan before the frame header");
        at += 2 + len;
    }
}

/// A PNG's header, read here: width, height, bit depth, colour type.
fn png_header(file: &[u8]) -> (u32, u32, u8, u8) {
    assert!(file.starts_with(PNG_SIGNATURE), "no PNG signature");
    assert_eq!(&file[12..16], b"IHDR", "no header first");
    let be = |at: usize| u32::from_be_bytes(file[at..at + 4].try_into().unwrap());
    (be(16), be(20), file[24], file[25])
}

/// `ffprobe`'s view of one image's stream (`codec|width|height|pix_fmt`),
/// or `None` (skip) without `ffprobe`.
fn probe_image(data: &[u8]) -> Option<String> {
    let ffprobe = program("META_WHATSAPP_RS_TEST_FFPROBE", "ffprobe");
    let out = run(
        &ffprobe,
        &[
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,width,height,pix_fmt",
            "-of",
            "compact=p=0:nk=1",
            "pipe:0",
        ],
        data,
    );
    match out {
        Some(out) => Some(String::from_utf8(out).unwrap().trim().to_owned()),
        None if required() => panic!("{} did not run", ffprobe.display()),
        None => {
            eprintln!("ffprobe not run: {} did not start", ffprobe.display());
            None
        }
    }
}

/// Upload `media` as `stem` and assert the exact request: its format's
/// type, its name, its bytes.
async fn assert_uploaded(media: meta_whatsapp_rs::client::media::ConvertedMedia, stem: &str) {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"id": "1013859600285441"}));
    let client = Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .build()
        .unwrap();
    let id = client
        .media(PhoneNumberId::new("106540352242922").unwrap())
        .upload_converted(media.clone(), stem)
        .await
        .unwrap();
    assert_eq!(id.as_str(), "1013859600285441");
    let req = t.last_request().unwrap();
    assert_eq!(req.method.as_str(), "POST");
    assert_eq!(req.path(), "/v25.0/106540352242922/media");
    assert_eq!(req.bearer(), Some("TOKEN"));
    let (_, _, product) = req.multipart_field("messaging_product").unwrap();
    assert_eq!(&product[..], b"whatsapp");
    let mime = media.format.mime_type();
    let (_, _, kind) = req.multipart_field("type").unwrap();
    assert_eq!(&kind[..], mime.as_bytes());
    let (name, ctype, data) = req.multipart_field("file").unwrap();
    let file_name = format!("{stem}.{}", media.format.extension());
    assert_eq!((name, ctype), (Some(file_name.as_str()), Some(mime)));
    assert_eq!(data, &media.data);
    let RecordedBody::Multipart(parts) = &req.body else {
        panic!("expected multipart");
    };
    assert_eq!(parts.len(), 3);
    assert_eq!(t.remaining(), 0);
}

/// A busy picture whose PNG is megabytes, and whose WebP at its own size is
/// several times the static sticker limit, comes out as a static WebP
/// within 100 000 bytes, scaled down with its aspect ratio kept, which
/// `Media::upload`'s type check takes as a sticker.
#[tokio::test]
async fn ffmpeg_turns_a_png_into_a_static_webp_sticker_that_upload_accepts() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&IMAGES).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let picture = lavfi(
        &ffmpeg,
        "testsrc2=s=1600x1200,noise=alls=60:allf=t+u",
        &["-c:v", "png"],
    );
    assert_eq!(png_header(&picture), (1600, 1200, 8, 2));
    assert!(picture.len() > 1_000_000, "{} bytes", picture.len());

    let sticker = converter
        .convert(MediaInput::new(picture), TargetFormat::StaticSticker)
        .await
        .unwrap();
    assert_eq!(sticker.format, TargetFormat::StaticSticker);
    let len = sticker.data.len();
    assert!(len <= 100_000, "{len} bytes");
    let w = webp(&sticker.data);
    for animated in [b"ANIM", b"ANMF"] {
        assert!(!w.chunks.contains(animated), "{w:?}");
    }
    assert!(
        w.width < 1600 && w.width * 3 == w.height * 4,
        "{}x{}",
        w.width,
        w.height
    );
    if let Some(probe) = probe_image(&sticker.data) {
        assert_eq!(probe, format!("webp|{}|{}|yuv420p", w.width, w.height));
    }

    assert_eq!(
        validate_upload(sticker.format.mime_type(), u64::try_from(len).unwrap()),
        Ok(MediaKind::Sticker)
    );
    assert_uploaded(sticker, "sticker").await;
}

/// A small picture with transparency fits as it is: its size and its
/// alpha channel are kept.
#[tokio::test]
async fn ffmpeg_keeps_a_small_sticker_s_size_and_transparency() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&IMAGES).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let logo = lavfi(
        &ffmpeg,
        "testsrc2=s=320x240,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(lt(X,160),255,0)'",
        &["-c:v", "png"],
    );
    assert_eq!(png_header(&logo), (320, 240, 8, 6));
    let sticker = converter
        .convert(MediaInput::new(logo), TargetFormat::StaticSticker)
        .await
        .unwrap();
    let w = webp(&sticker.data);
    assert_eq!((w.width, w.height, w.alpha), (320, 240, true), "{w:?}");
    assert_eq!(&w.chunks[0], b"VP8X");
    if let Some(probe) = probe_image(&sticker.data) {
        assert_eq!(probe, "webp|320|240|yuva420p");
    }
}

/// A JPEG over 5 MB (more than the upload check takes, even read as MiB)
/// comes out as an 8-bit colour JPEG under 5 000 000 bytes, its aspect
/// ratio kept.
#[tokio::test]
async fn ffmpeg_brings_a_jpeg_over_five_megabytes_under_the_image_limit() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&IMAGES).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let photo = lavfi(
        &ffmpeg,
        "nullsrc=s=4000x3000,geq=lum='random(1)*255':cb=128:cr=128",
        &["-c:v", "mjpeg", "-q:v", "1", "-qmin", "1"],
    );
    let (_, width, height, _) = jpeg_frame(&photo);
    assert_eq!((width, height), (4000, 3000));
    let input_len = u64::try_from(photo.len()).unwrap();
    assert!(input_len > 5 * 1024 * 1024, "{input_len} bytes");
    // The control: the upload check refuses it as it is.
    assert_eq!(
        validate_upload("image/jpeg", input_len).unwrap_err().field,
        "file"
    );

    let image = converter
        .convert(MediaInput::new(photo), TargetFormat::Jpeg)
        .await
        .unwrap();
    assert_eq!(image.format, TargetFormat::Jpeg);
    let len = u64::try_from(image.data.len()).unwrap();
    assert!(len <= 5_000_000, "{len} bytes");
    let (precision, width, height, components) = jpeg_frame(&image.data);
    assert_eq!((precision, components), (8, 3));
    assert!(width <= 4000 && width * 3 == height * 4, "{width}x{height}");
    if let Some(probe) = probe_image(&image.data) {
        assert_eq!(probe, format!("mjpeg|{width}|{height}|yuvj420p"));
    }
    assert_eq!(
        validate_upload(image.format.mime_type(), len),
        Ok(MediaKind::Image)
    );
    assert_uploaded(image, "photo").await;
}

/// A 16-bit PNG over 5 MB comes out as an 8-bit RGBA PNG under the limit
/// (Meta: "Images must be 8-bit, RGB or RGBA"); a small 16-bit RGB one
/// keeps its size and becomes 8-bit RGB.
#[tokio::test]
async fn ffmpeg_makes_a_png_eight_bit_and_under_the_image_limit() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&IMAGES).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let deep = lavfi(
        &ffmpeg,
        "testsrc2=s=1600x1200,noise=alls=60:allf=t+u,format=rgba64be",
        &["-c:v", "png"],
    );
    assert_eq!(png_header(&deep), (1600, 1200, 16, 6));
    assert!(deep.len() > 5 * 1024 * 1024, "{} bytes", deep.len());
    let image = converter
        .convert(MediaInput::new(deep), TargetFormat::Png)
        .await
        .unwrap();
    let len = u64::try_from(image.data.len()).unwrap();
    assert!(len <= 5_000_000, "{len} bytes");
    let (width, height, depth, colour) = png_header(&image.data);
    assert_eq!((depth, colour), (8, 6));
    assert!(width < 1600 && width * 3 == height * 4, "{width}x{height}");
    assert!(image.data.ends_with(b"IEND\xAE\x42\x60\x82"));
    assert_eq!(
        validate_upload(image.format.mime_type(), len),
        Ok(MediaKind::Image)
    );

    let small = lavfi(
        &ffmpeg,
        "testsrc2=s=640x480,format=rgb48be",
        &["-c:v", "png"],
    );
    assert_eq!(png_header(&small), (640, 480, 16, 2));
    let image = converter
        .convert(MediaInput::new(small), TargetFormat::Png)
        .await
        .unwrap();
    assert_eq!(png_header(&image.data), (640, 480, 8, 2));
}

/// The converter's own command line for one attempt, but without its
/// format whitelist: the control that shows a file is one ffmpeg reads
/// from a pipe and converts, when nothing stops it.
fn without_whitelist(ffmpeg: &PathBuf, file: &[u8]) -> Option<Vec<u8>> {
    run(
        ffmpeg,
        &[
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-protocol_whitelist",
            "pipe",
            "-max_pixels",
            "33554432",
            "-i",
            "pipe:0",
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-vf",
            "format=yuvj420p",
            "-c:v",
            "mjpeg",
            "-f",
            "image2pipe",
            "pipe:1",
        ],
        file,
    )
}

/// A GIF, a JPEG-LS image, an animated PNG and a PNG signature over
/// something else are refused, for every image format.
///
/// The GIF is refused by its first bytes. The JPEG-LS image and the
/// animated PNG begin as a JPEG and a PNG do, so they pass that check, and
/// ffmpeg's format whitelist refuses them: the JPEG-LS image is the
/// decisive one, since without the whitelist ffmpeg reads it from a pipe
/// and converts it (the control). ffmpeg 6.1.1 cannot read an animated PNG
/// from a pipe at all (its reader seeks), so that one is refused even
/// without the whitelist.
#[tokio::test]
async fn ffmpeg_refuses_a_gif_a_jpeg_ls_an_animated_png_and_a_crafted_png() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&IMAGES).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let animation = |muxer: &[&str]| {
        let mut args = vec![
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x120:r=5",
            "-t",
            "1",
        ];
        args.extend_from_slice(muxer);
        args.push("pipe:1");
        run(&ffmpeg, &args, &[]).unwrap_or_else(|| panic!("making {muxer:?}"))
    };
    let gif = animation(&["-f", "gif"]);
    assert!(gif.starts_with(b"GIF89a"));
    let apng = animation(&["-plays", "0", "-f", "apng"]);
    assert!(apng.starts_with(PNG_SIGNATURE));
    assert!(apng.windows(4).any(|w| w == b"acTL"), "not animated");
    let jpeg_ls = lavfi(&ffmpeg, "testsrc2=s=160x120", &["-c:v", "jpegls"]);
    // Start of image, then JPEG-LS's own frame marker (SOF55).
    assert_eq!(&jpeg_ls[..4], &[0xFF, 0xD8, 0xFF, 0xF7]);
    for (what, file) in [("gif", &gif), ("jpeg-ls", &jpeg_ls)] {
        assert!(
            without_whitelist(&ffmpeg, file).is_some_and(|o| o.starts_with(&[0xFF, 0xD8])),
            "ffmpeg did not read the {what}"
        );
    }
    let crafted = [
        PNG_SIGNATURE,
        b"ffconcat version 1.0\nfile '/etc/hostname'\n",
    ]
    .concat();
    for target in IMAGES {
        for (what, file) in [
            ("gif", &gif),
            ("jpeg-ls", &jpeg_ls),
            ("apng", &apng),
            ("crafted", &crafted),
        ] {
            let err = converter
                .convert(MediaInput::new(file.clone()), target)
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
                "{what} to {target:?}: {err:?}"
            );
            assert!(!err.to_string().contains("/etc/hostname"), "{err}");
        }
    }
}

/// An image of 36 million pixels (6000 × 6000, a small file of one colour)
/// is refused by the pixel bound; the control: ffmpeg without the bound
/// converts it.
#[tokio::test]
async fn ffmpeg_refuses_an_image_over_the_pixel_bound() {
    let _cpu = cpu_alone().await;
    let Some(converter) = converter_for(&IMAGES).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let huge = lavfi(&ffmpeg, "color=c=red:s=6000x6000", &["-c:v", "png"]);
    assert_eq!(png_header(&huge), (6000, 6000, 8, 2));
    assert!(huge.len() < 5_000_000, "{} bytes", huge.len());
    let direct = run(
        &ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            "-c:v",
            "mjpeg",
            "-f",
            "image2pipe",
            "pipe:1",
        ],
        &huge,
    );
    assert!(
        direct.is_some_and(|o| o.starts_with(&[0xFF, 0xD8])),
        "ffmpeg did not convert it"
    );
    let started = Instant::now();
    let err = converter
        .convert(MediaInput::new(huge), TargetFormat::Jpeg)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}

/// `jpeg` with an EXIF segment of one tag, Orientation, set to
/// `orientation` (6: the camera was turned a quarter clockwise; the
/// picture is shown rotated 90° clockwise).
fn with_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
    let mut tiff = b"II*\0".to_vec();
    tiff.extend_from_slice(&8u32.to_le_bytes()); // the first IFD
    tiff.extend_from_slice(&1u16.to_le_bytes()); // one entry
    tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // Orientation
    tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_le_bytes()); // one value
    tiff.extend_from_slice(&orientation.to_le_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    let payload = [&b"Exif\0\0"[..], &tiff].concat();
    let mut segment = vec![0xFF, 0xE1];
    segment.extend_from_slice(&u16::try_from(payload.len() + 2).unwrap().to_be_bytes());
    segment.extend_from_slice(&payload);
    [&jpeg[..2], &segment, &jpeg[2..]].concat()
}

/// A photo whose EXIF says it was taken turned comes out upright: its
/// pixels rotated (640 × 480 becomes 480 × 640), its EXIF dropped.
#[tokio::test]
async fn ffmpeg_applies_a_jpeg_s_orientation_and_drops_its_exif() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&IMAGES).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let plain = lavfi(&ffmpeg, "testsrc2=s=640x480", &["-c:v", "mjpeg"]);
    let turned = with_orientation(&plain, 6);
    let (_, width, height, _) = jpeg_frame(&turned);
    assert_eq!((width, height), (640, 480));
    let image = converter
        .convert(MediaInput::new(turned.clone()), TargetFormat::Jpeg)
        .await
        .unwrap();
    let (_, width, height, _) = jpeg_frame(&image.data);
    assert_eq!((width, height), (480, 640));
    assert!(
        !image.data.windows(6).any(|w| w == b"Exif\0\0"),
        "EXIF kept"
    );
    let sticker = converter
        .convert(MediaInput::new(turned), TargetFormat::StaticSticker)
        .await
        .unwrap();
    let w = webp(&sticker.data);
    assert_eq!((w.width, w.height), (480, 640));
}

/// One PNG chunk: length, type, data and a zero CRC, never checked (the
/// converter takes these chunks out before ffmpeg reads them).
fn png_chunk(kind: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(&kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&[0, 0, 0, 0]);
    out
}

/// A PNG whose text chunks (a `zTXt` among them, which ffmpeg would
/// inflate) the converter takes out, followed by a second image, and two
/// JPEGs in a row: what ffmpeg reads of each still converts, and only the
/// first image comes out, alone, without the text.
#[tokio::test]
async fn ffmpeg_converts_the_first_image_of_a_png_without_its_text() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&IMAGES).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let first = lavfi(&ffmpeg, "testsrc2=s=320x240", &["-c:v", "png"]);
    let second = lavfi(&ffmpeg, "testsrc2=s=64x48", &["-c:v", "png"]);
    // After the header (signature and IHDR: 33 bytes).
    let png = [
        &first[..33],
        &png_chunk(*b"tEXt", b"Author\0Jane Doe"),
        &png_chunk(*b"zTXt", b"Comment\0\0x\x9c\x03\0\0\0\0\x01"),
        &first[33..],
        &second,
    ]
    .concat();
    for target in IMAGES {
        let out = converter
            .convert(MediaInput::new(png.clone()), target)
            .await
            .unwrap_or_else(|e| panic!("{target:?}: {e}"));
        assert!(!out.data.windows(4).any(|w| w == b"Jane"), "{target:?}");
        let (width, height) = match target {
            TargetFormat::StaticSticker => {
                let w = webp(&out.data);
                (w.width, w.height)
            }
            TargetFormat::Jpeg => {
                let (_, width, height, _) = jpeg_frame(&out.data);
                (width, height)
            }
            _ => {
                let (width, height, _, _) = png_header(&out.data);
                assert_eq!(out.data.windows(4).filter(|w| *w == b"IEND").count(), 1);
                (width, height)
            }
        };
        assert_eq!((width, height), (320, 240), "{target:?}");
    }

    let jpeg = lavfi(&ffmpeg, "testsrc2=s=320x240", &["-c:v", "mjpeg"]);
    let small = lavfi(&ffmpeg, "testsrc2=s=64x48", &["-c:v", "mjpeg"]);
    let out = converter
        .convert(
            MediaInput::new([&jpeg[..], &small[..]].concat()),
            TargetFormat::Jpeg,
        )
        .await
        .unwrap();
    assert_eq!(
        out.data.windows(2).filter(|w| *w == [0xFF, 0xD8]).count(),
        1
    );
    let (_, width, height, _) = jpeg_frame(&out.data);
    assert_eq!((width, height), (320, 240));
}

/// A 16-bit RGBA PNG of exactly 32 Mi pixels (8192 × 4096, a small file
/// of one colour) decodes to a frame of 256 MiB and ffmpeg's padding: the
/// allocation bound refuses it, two rows fewer pass. The control: ffmpeg
/// without the bound converts it.
#[tokio::test]
async fn ffmpeg_refuses_a_frame_over_the_allocation_bound() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&IMAGES).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let deep = |size: &str| {
        lavfi(
            &ffmpeg,
            &format!("color=c=red:s={size},format=rgba64be"),
            &["-c:v", "png"],
        )
    };
    let at_bound = deep("8192x4096");
    assert_eq!(png_header(&at_bound), (8192, 4096, 16, 6));
    let direct = run(
        &ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-max_pixels",
            "33554432",
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            "-vf",
            "format=yuvj420p",
            "-c:v",
            "mjpeg",
            "-f",
            "image2pipe",
            "pipe:1",
        ],
        &at_bound,
    );
    assert!(
        direct.is_some_and(|o| o.starts_with(&[0xFF, 0xD8])),
        "ffmpeg did not convert it"
    );
    let err = converter
        .convert(MediaInput::new(at_bound), TargetFormat::Jpeg)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?}"
    );
    // Two rows fewer (lavfi's `color` source keeps sizes even).
    let under = deep("8192x4094");
    assert_eq!(png_header(&under), (8192, 4094, 16, 6));
    let under = converter
        .convert(MediaInput::new(under), TargetFormat::Jpeg)
        .await
        .unwrap();
    let (_, width, height, _) = jpeg_frame(&under.data);
    assert_eq!((width, height), (8192, 4094));
}

// Animated stickers (roadmap L19c).

/// An animation made by a `lavfi` source, in the container `muxer` names
/// (a GIF, an animated PNG, an animated WebP): the tests' animations,
/// made here rather than committed.
fn lavfi_animation(ffmpeg: &PathBuf, source: &str, frames: usize, muxer: &[&str]) -> Vec<u8> {
    let frames = frames.to_string();
    let mut args = vec![
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        source,
        "-frames:v",
        frames.as_str(),
    ];
    args.extend_from_slice(muxer);
    args.push("pipe:1");
    run(ffmpeg, &args, &[]).unwrap_or_else(|| panic!("making {source} as {muxer:?}"))
}

/// A GIF of `frames` frames of `source`.
fn gif(ffmpeg: &PathBuf, source: &str, frames: usize) -> Vec<u8> {
    let gif = lavfi_animation(ffmpeg, source, frames, &["-f", "gif"]);
    assert!(gif.starts_with(b"GIF89a"), "not a GIF");
    gif
}

/// An animated WebP, read here apart from the converter's own check
/// (Google's "WebP Container Specification", extended file format): its
/// chunks in order, the extended header's flags and canvas, the animation
/// header's loop count, and its frames with their total duration.
#[derive(Debug)]
struct AnimatedWebp {
    chunks: Vec<[u8; 4]>,
    flags: u8,
    width: u32,
    height: u32,
    loops: u32,
    frames: usize,
    duration_ms: u32,
}

fn animated_webp(file: &[u8]) -> AnimatedWebp {
    assert_eq!(&file[..4], b"RIFF", "not a RIFF file");
    assert_eq!(&file[8..12], b"WEBP", "not a WebP file");
    assert_eq!(
        usize::try_from(le32(file, 4)).unwrap() + 8,
        file.len(),
        "the RIFF size is not the file's"
    );
    let mut w = AnimatedWebp {
        chunks: Vec::new(),
        flags: 0,
        width: 0,
        height: 0,
        loops: u32::MAX,
        frames: 0,
        duration_ms: 0,
    };
    let mut at = 12;
    while at < file.len() {
        let kind = <[u8; 4]>::try_from(&file[at..at + 4]).unwrap();
        let len = usize::try_from(le32(file, at + 4)).unwrap();
        let data = &file[at + 8..at + 8 + len];
        match &kind {
            b"VP8X" => {
                w.flags = data[0];
                w.width = 1 + le24(data, 4);
                w.height = 1 + le24(data, 7);
            }
            b"ANIM" => w.loops = le16(data, 4),
            b"ANMF" => {
                w.frames += 1;
                w.duration_ms += le24(data, 12);
            }
            _ => {}
        }
        w.chunks.push(kind);
        at += 8 + len + len % 2;
    }
    assert_eq!(at, file.len(), "chunks past the end");
    w
}

/// A busy animated GIF (5 s of a moving picture at 20 frames a second,
/// 480 × 360) comes out as an animated WebP within 500 000 bytes: its
/// extended header has the animation flag, an `ANIM` chunk comes before
/// its `ANMF` frames, it loops forever and plays for the GIF's time, its
/// aspect ratio kept; `Media::upload`'s type check takes it as a
/// sticker.
#[tokio::test]
async fn ffmpeg_turns_a_gif_into_an_animated_webp_sticker_that_upload_accepts() {
    let _cpu = cpu_alone().await;
    let Some(converter) = converter_for(&[TargetFormat::AnimatedSticker]).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let animation = gif(&ffmpeg, "testsrc2=s=480x360:r=20", 100);
    assert!(animation.len() > 500_000, "{} bytes", animation.len());

    let started = Instant::now();
    let sticker = converter
        .convert(MediaInput::new(animation), TargetFormat::AnimatedSticker)
        .await
        .unwrap();
    eprintln!(
        "animated sticker: {} bytes in {:?}",
        sticker.data.len(),
        started.elapsed()
    );
    assert_eq!(sticker.format, TargetFormat::AnimatedSticker);
    let len = sticker.data.len();
    assert!(len <= 500_000, "{len} bytes");
    let w = animated_webp(&sticker.data);
    assert_eq!(&w.chunks[0], b"VP8X");
    assert_eq!(w.flags & 0x02, 0x02, "the animation flag is not set: {w:?}");
    let anim = w
        .chunks
        .iter()
        .position(|c| c == b"ANIM")
        .expect("no ANIM chunk");
    let first_frame = w
        .chunks
        .iter()
        .position(|c| c == b"ANMF")
        .expect("no ANMF chunk");
    assert!(anim < first_frame, "{w:?}");
    assert!(w.frames >= 2, "{w:?}");
    for still in [b"VP8 ", b"VP8L"] {
        assert!(!w.chunks.contains(still), "{w:?}");
    }
    assert_eq!(w.loops, 0, "loops forever");
    assert!(
        (4800..=5200).contains(&w.duration_ms),
        "plays {} ms",
        w.duration_ms
    );
    assert!(
        w.width <= 480 && w.width * 3 == w.height * 4,
        "{}x{}",
        w.width,
        w.height
    );

    assert_eq!(
        validate_upload(sticker.format.mime_type(), u64::try_from(len).unwrap()),
        Ok(MediaKind::Sticker)
    );
    assert_uploaded(sticker, "animated").await;
}

/// The frame bound: a GIF of 501 frames is refused, before ffmpeg reads
/// it; one of 500 converts. The control: ffmpeg alone converts the 501
/// frames, so the bound is the converter's.
#[tokio::test]
async fn ffmpeg_refuses_an_animation_over_the_frame_bound() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&[TargetFormat::AnimatedSticker]).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let over = gif(&ffmpeg, "testsrc2=s=32x32:r=50", 501);
    let direct = run(
        &ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-c:v",
            "libwebp_anim",
            "-f",
            "image2pipe",
            "pipe:1",
        ],
        &over,
    )
    .expect("ffmpeg did not convert it");
    assert_eq!(animated_webp(&direct).flags & 0x02, 0x02);

    let err = converter
        .convert(MediaInput::new(over), TargetFormat::AnimatedSticker)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?}"
    );
    assert!(err.to_string().contains("more than 500 frames"), "{err}");

    let at_bound = gif(&ffmpeg, "testsrc2=s=32x32:r=50", 500);
    let sticker = converter
        .convert(MediaInput::new(at_bound), TargetFormat::AnimatedSticker)
        .await
        .unwrap();
    assert!(animated_webp(&sticker.data).frames >= 2);
}

/// The time bound: 11 frames of a second each are refused; 10 convert.
#[tokio::test]
async fn ffmpeg_refuses_an_animation_over_the_time_bound() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&[TargetFormat::AnimatedSticker]).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let over = gif(&ffmpeg, "testsrc2=s=32x32:r=1", 11);
    let err = converter
        .convert(MediaInput::new(over), TargetFormat::AnimatedSticker)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?}"
    );
    assert!(err.to_string().contains("longer than 10 seconds"), "{err}");
    let at_bound = gif(&ffmpeg, "testsrc2=s=32x32:r=1", 10);
    let sticker = converter
        .convert(MediaInput::new(at_bound), TargetFormat::AnimatedSticker)
        .await
        .unwrap();
    let w = animated_webp(&sticker.data);
    assert!(
        (9_000..=10_000).contains(&w.duration_ms),
        "plays {} ms",
        w.duration_ms
    );
}

/// The bound on frames times pixels: 101 frames of 512 × 512 (within the
/// frame, time and pixel bounds) are refused, before ffmpeg reads them.
/// The control: ffmpeg alone converts them.
#[tokio::test]
async fn ffmpeg_refuses_an_animation_over_the_frame_pixel_bound() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&[TargetFormat::AnimatedSticker]).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let over = gif(&ffmpeg, "testsrc2=s=512x512:r=25", 101);
    let direct = run(
        &ffmpeg,
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            "pipe:0",
            "-vf",
            "scale=64:64",
            "-c:v",
            "libwebp_anim",
            "-f",
            "image2pipe",
            "pipe:1",
        ],
        &over,
    )
    .expect("ffmpeg did not convert it");
    assert!(animated_webp(&direct).frames >= 2);
    let err = converter
        .convert(MediaInput::new(over), TargetFormat::AnimatedSticker)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?}"
    );
    assert!(
        err.to_string().contains("more frames than its size allows"),
        "{err}"
    );
}

/// The pixel bound: a GIF whose canvas is 2048 × 1026 (two rows past
/// 2 Mi pixels: lavfi keeps sizes even) is refused; 2048 × 1024 converts,
/// scaled within 512. The control: ffmpeg alone converts the larger one.
#[tokio::test]
async fn ffmpeg_refuses_an_animation_over_the_pixel_bound() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&[TargetFormat::AnimatedSticker]).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let over = gif(&ffmpeg, "testsrc2=s=2048x1026:r=5", 2);
    assert!(
        run(
            &ffmpeg,
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                "pipe:0",
                "-c:v",
                "libwebp_anim",
                "-f",
                "image2pipe",
                "pipe:1",
            ],
            &over,
        )
        .is_some_and(|o| o.starts_with(b"RIFF")),
        "ffmpeg did not convert it"
    );
    let err = converter
        .convert(MediaInput::new(over), TargetFormat::AnimatedSticker)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?}"
    );
    assert!(err.to_string().contains("over 2097152 pixels"), "{err}");

    let at_bound = gif(&ffmpeg, "testsrc2=s=2048x1024:r=5", 2);
    let sticker = converter
        .convert(MediaInput::new(at_bound), TargetFormat::AnimatedSticker)
        .await
        .unwrap();
    let w = animated_webp(&sticker.data);
    assert_eq!((w.width, w.height), (512, 256), "{w:?}");
}

/// `gif` with `extensions` spliced in after its header and global colour
/// table, before its first block.
fn with_extensions(gif: &[u8], extensions: &[u8]) -> Vec<u8> {
    let flags = gif[10];
    let header = 13
        + if flags & 0x80 == 0 {
            0
        } else {
            3 << ((flags & 7) + 1)
        };
    [&gif[..header], extensions, &gif[header..]].concat()
}

/// `data` as GIF sub-blocks: runs of at most 255 bytes, each after its
/// length, then a zero length.
fn sub_blocks(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for run in data.chunks(255) {
        out.push(u8::try_from(run.len()).unwrap());
        out.extend_from_slice(run);
    }
    out.push(0);
    out
}

/// A GIF with megabytes of comments, XMP and an ICC profile around its
/// frames (which ffmpeg would hold in memory with the first frame) still
/// converts, and nothing of them reaches the sticker.
#[tokio::test]
async fn ffmpeg_makes_a_sticker_of_a_gif_without_its_comments_and_extensions() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&[TargetFormat::AnimatedSticker]).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let plain = gif(&ffmpeg, "testsrc2=s=160x120:r=10", 10);
    let comment = b"Jane Doe, 12 Main Street. ".repeat(80_000);
    // A comment, two application extensions (an identifier of eleven
    // bytes, then data) and a plain text one (a header of twelve, then
    // text).
    let extensions = [
        &[0x21, 0xFE][..],
        &sub_blocks(&comment),
        &[0x21, 0xFF, 11],
        b"XMP DataXMP",
        &sub_blocks(b"<x:xmpmeta>Jane</x:xmpmeta>"),
        &[0x21, 0xFF, 11],
        b"ICCRGBG1012",
        &sub_blocks(&[7; 3000]),
        &[0x21, 0x01, 12],
        &[0; 12],
        &sub_blocks(b"Jane"),
    ]
    .concat();
    let rich = [
        with_extensions(&plain, &extensions),
        b"Jane, after the trailer".to_vec(),
    ]
    .concat();
    assert!(rich.len() > 2_000_000, "{} bytes", rich.len());
    let sticker = converter
        .convert(MediaInput::new(rich), TargetFormat::AnimatedSticker)
        .await
        .unwrap();
    assert!(!sticker.data.windows(4).any(|w| w == b"Jane"));
    let w = animated_webp(&sticker.data);
    assert_eq!((w.width, w.height, w.frames), (160, 120, 10), "{w:?}");
    for metadata in [b"EXIF", b"XMP ", b"ICCP"] {
        assert!(!w.chunks.contains(metadata), "{w:?}");
    }
}

/// An animated PNG, an animated WebP, a PNG, a JPEG, a GIF of one frame
/// and a GIF whose frames are all alike are refused for an animated
/// sticker; a GIF is still refused for a static one.
#[tokio::test]
async fn ffmpeg_refuses_what_is_not_an_animated_gif() {
    let _cpu = cpu_shared().await;
    let Some(converter) = converter_for(&[TargetFormat::AnimatedSticker]).await else {
        return;
    };
    let ffmpeg = converter.program().to_path_buf();
    let source = "testsrc2=s=160x120:r=5";
    let apng = lavfi_animation(&ffmpeg, source, 5, &["-plays", "0", "-f", "apng"]);
    assert!(apng.windows(4).any(|w| w == b"acTL"), "not animated");
    let webp = lavfi_animation(
        &ffmpeg,
        source,
        5,
        &["-c:v", "libwebp_anim", "-f", "image2pipe"],
    );
    assert_eq!(animated_webp(&webp).flags & 0x02, 0x02, "not animated");
    let png = lavfi(&ffmpeg, source, &["-c:v", "png"]);
    let jpeg = lavfi(&ffmpeg, source, &["-c:v", "mjpeg"]);
    let one = gif(&ffmpeg, source, 1);
    let still = gif(&ffmpeg, "color=c=red:s=160x120:r=5", 5);
    for (what, file, why) in [
        ("apng", &apng, "not a GIF's"),
        ("webp", &webp, "not a GIF's"),
        ("png", &png, "not a GIF's"),
        ("jpeg", &jpeg, "not a GIF's"),
        ("one frame", &one, "fewer than two frames"),
        ("still", &still, "all alike"),
    ] {
        let err = converter
            .convert(MediaInput::new(file.clone()), TargetFormat::AnimatedSticker)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
            "{what}: {err:?}"
        );
        assert!(err.to_string().contains(why), "{what}: {err}");
    }
    let animated = gif(&ffmpeg, source, 5);
    let err = converter
        .convert(MediaInput::new(animated), TargetFormat::StaticSticker)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?}"
    );
}
