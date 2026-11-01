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
/// whole more than two hours of audio. It is refused at once, not after
/// gigabytes and the timeout.
#[tokio::test]
async fn ffmpeg_refuses_a_header_that_inflates_the_input() {
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
    assert!(
        matches!(&err, Error::Conversion(ConversionError::InputRejected(_))),
        "{err:?} after {:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
}
