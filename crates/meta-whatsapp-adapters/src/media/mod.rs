//! Media conversion over an `ffmpeg` the integrator installs (feature
//! `ffmpeg`): [`FfmpegConverter`], a
//! [`MediaConverter`](meta_whatsapp_client::media::MediaConverter).
//!
//! Nothing is linked or bundled. The converter starts the `ffmpeg`
//! binary as a child process, feeds it the input on stdin and reads the
//! result from stdout; no file is written. Install `ffmpeg` built with
//! `libopus` on the host (Debian and Ubuntu: `apt-get install ffmpeg`),
//! or point [`FfmpegConverter::with_program`] at one, and call
//! [`FfmpegConverter::check`] at startup to fail early. The service's
//! public image carries no `ffmpeg`: a deployment that wants conversion
//! adds it, under the licence of the build it chooses.
//!
//! # What it runs
//!
//! For [`TargetFormat::OggOpus`], always these arguments, none of them
//! taken from the caller:
//!
//! ```text
//! ffmpeg -hide_banner -nostdin -nostats -loglevel error
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
//!   files or reach the network.
//! - **Only the audio formats a recording comes in are read**
//!   (`-format_whitelist`: WAV, MP3, Ogg, FLAC, AAC, MP4/M4A/3GP, AMR,
//!   Matroska and `WebM`). Anything else is refused before decoding.
//! - **The result is what Meta's table takes**: the first audio stream,
//!   mono (Meta: "mono input only"), Opus in Ogg; tags and chapters are
//!   dropped (they can carry personal data). At 32 kb/s a voice note
//!   stays under the 512 KB that keeps WhatsApp's play icon
//!   (`messages/audio-messages`) for about two minutes. The output is
//!   checked to be mono Ogg Opus before it is returned.
//!
//! # Bounds and errors
//!
//! | Bound or failure | Default | Error |
//! | --- | --- | --- |
//! | `ffmpeg` missing or not executable | — | [`Error::Config`] |
//! | the output is not mono Ogg Opus (a binary that is not the `ffmpeg` it should be) | — | [`Error::Config`] |
//! | an empty input, or one over [`with_max_input_bytes`](FfmpegConverter::with_max_input_bytes) | 64 MiB | [`Error::Validation`] on `input`, before `ffmpeg` starts |
//! | a result over the upload limit ([`TargetFormat::max_bytes`], 16 MB) | — | [`Error::Validation`] on `input`; `ffmpeg` is killed |
//! | `ffmpeg` fails (input it cannot read, a format off the list) | — | [`Error::Validation`] on `input` |
//! | a run over [`with_timeout`](FfmpegConverter::with_timeout) | 60 s | `Error::Transport(TransportError::Timeout)`; `ffmpeg` is killed |
//!
//! `ffmpeg`'s diagnostics (stderr) are read and discarded, never put in
//! an error or a log: they name files and URLs. An error says what
//! failed (the exit status, a bound) and nothing of the input. Dropping
//! a conversion in flight kills its process. Conversions are not
//! queued: bound how many run at once yourself (each one is a process
//! using a CPU core).
//!
//! ```no_run
//! # async fn demo(wav: Vec<u8>) -> meta_whatsapp_core::Result<()> {
//! use std::time::Duration;
//! use meta_whatsapp_adapters::media::FfmpegConverter;
//! use meta_whatsapp_client::media::{MediaConverter, MediaInput, TargetFormat};
//!
//! let converter = FfmpegConverter::new().with_timeout(Duration::from_secs(30));
//! converter.check().await?; // at startup: a missing ffmpeg is a configuration error
//! let note = converter
//!     .convert(MediaInput::new(wav), TargetFormat::OggOpus)
//!     .await?;
//! assert_eq!(note.format.mime_type(), "audio/ogg");
//! # Ok(()) }
//! ```

#[cfg(test)]
mod tests;

use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use meta_whatsapp_client::media::{ConvertedMedia, MediaConverter, MediaInput, TargetFormat};
use meta_whatsapp_core::error::{ConfigError, TransportError, ValidationError};
use meta_whatsapp_core::{Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// The demuxers `ffmpeg` may use on an input: the audio formats
/// recordings come in. `mov` reads MP4, M4A and 3GP; `matroska` reads
/// `WebM` (a browser's `MediaRecorder`).
const INPUT_FORMATS: &str = "wav,mp3,ogg,flac,aac,mov,amr,matroska";

/// The arguments for [`TargetFormat::OggOpus`], in order. Every one is a
/// literal: nothing of the caller's reaches the command line.
const OGG_OPUS_ARGS: &[&str] = &[
    "-hide_banner",
    "-nostdin",
    "-nostats",
    "-loglevel",
    "error",
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

/// The arguments that make `target`, or `None` for a format this
/// converter does not produce.
fn arguments(target: TargetFormat) -> Option<&'static [&'static str]> {
    match target {
        TargetFormat::OggOpus => Some(OGG_OPUS_ARGS),
        _ => None,
    }
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

impl FfmpegConverter {
    /// How long one run may take by default.
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

    /// The longest one run may take, writing the input and reading the
    /// result included; past it the process is killed
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
    /// `ffmpeg -encoders`: call it at startup, so that a missing or
    /// unsuitable `ffmpeg` is found before the first conversion.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the binary cannot be started, does not list
    /// its encoders, or lists no `libopus`;
    /// `Error::Transport(TransportError::Timeout)` past the timeout.
    pub async fn check(&self) -> Result<()> {
        let overflow = ConfigError::new(format!(
            "`{}` listed more than {ENCODERS_CAP} bytes of encoders: is it ffmpeg?",
            self.program.display()
        ));
        let (out, status) = self
            .run(ENCODERS_ARGS, None, ENCODERS_CAP, overflow.into())
            .await?;
        if !status.success() {
            return Err(ConfigError::new(format!(
                "`{}` did not list its encoders ({}): is it ffmpeg?",
                self.program.display(),
                describe(status)
            ))
            .into());
        }
        let listed = String::from_utf8_lossy(&out).lines().any(|line| {
            let mut words = line.split_whitespace();
            matches!(
                (words.next(), words.next()),
                (Some(flags), Some("libopus")) if flags.starts_with('A')
            )
        });
        if listed {
            Ok(())
        } else {
            Err(ConfigError::new(format!(
                "`{}` has no libopus encoder: Ogg Opus needs an ffmpeg built with libopus",
                self.program.display()
            ))
            .into())
        }
    }

    /// Start the program with `args`, feed it `input` (stdin is closed
    /// when there is none), read at most `max_out` bytes of its stdout
    /// (`overflow` past them) and discard its stderr, all within the
    /// timeout. On a timeout or an error the process is killed.
    async fn run(
        &self,
        args: &[&str],
        input: Option<Bytes>,
        max_out: u64,
        overflow: Error,
    ) -> Result<(Vec<u8>, ExitStatus)> {
        let mut child = Command::new(&self.program)
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
                Ok::<(), Error>(())
            };
            let collect = async move {
                match stdout {
                    Some(out) => read_bounded(out, max_out, overflow).await,
                    None => Ok(Vec::new()),
                }
            };
            // Read to the end so that ffmpeg never blocks on a full pipe,
            // and keep none of it: diagnostics name files and URLs.
            let drain = async move {
                if let Some(mut err) = stderr {
                    let _ = tokio::io::copy(&mut err, &mut tokio::io::sink()).await;
                }
                Ok::<(), Error>(())
            };
            let ((), out, ()) = tokio::try_join!(feed, collect, drain)?;
            let status = child
                .wait()
                .await
                .map_err(|e| process_error("wait for", e))?;
            Ok::<_, Error>((out, status))
        };
        let outcome = tokio::time::timeout(self.timeout, work).await;
        match outcome {
            Ok(Ok(done)) => Ok(done),
            Ok(Err(e)) => {
                let _ = child.kill().await;
                Err(e)
            }
            Err(_elapsed) => {
                let _ = child.kill().await;
                tracing::warn!(
                    timeout_ms = u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
                    "ffmpeg ran past its timeout and was killed"
                );
                Err(TransportError::Timeout.into())
            }
        }
    }
}

#[async_trait]
impl MediaConverter for FfmpegConverter {
    async fn convert(&self, input: MediaInput, target: TargetFormat) -> Result<ConvertedMedia> {
        let Some(args) = arguments(target) else {
            return Err(ValidationError::new(
                "target",
                format!("{target:?} is not a format FfmpegConverter produces"),
            )
            .into());
        };
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
        let max = target.max_bytes();
        let overflow = ValidationError::new(
            "input",
            format!(
                "converted to {target:?}, it exceeds {max} bytes, the most {:?} media can be uploaded with",
                target.kind()
            ),
        );
        let (out, status) = self
            .run(args, Some(input.data), max, overflow.into())
            .await?;
        if !status.success() {
            return Err(match status.code() {
                Some(_) => ValidationError::new(
                    "input",
                    format!(
                        "ffmpeg could not convert it to {target:?} ({}); its diagnostics are withheld",
                        describe(status)
                    ),
                )
                .into(),
                None => Error::Other(anyhow::anyhow!("ffmpeg was stopped ({})", describe(status))),
            });
        }
        check_output(target, &out).map_err(|why| {
            ConfigError::new(format!(
                "`{}` returned {why}, not mono Ogg Opus: is it ffmpeg with libopus?",
                self.program.display()
            ))
        })?;
        Ok(ConvertedMedia::new(out, target))
    }
}

/// Read `reader` to its end, or fail with `overflow` once it has given
/// more than `max` bytes.
async fn read_bounded<R: AsyncRead + Unpin>(
    reader: R,
    max: u64,
    overflow: Error,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    reader
        .take(max.saturating_add(1))
        .read_to_end(&mut out)
        .await
        .map_err(|e| process_error("read the output of", e))?;
    if u64::try_from(out.len()).unwrap_or(u64::MAX) > max {
        return Err(overflow);
    }
    Ok(out)
}

/// A failure to start the program. Missing or not executable is the
/// integrator's configuration.
fn spawn_error(program: &Path, e: &io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::NotFound => ConfigError::new(format!(
            "ffmpeg not found: `{}` could not be started; install ffmpeg, or point FfmpegConverter::with_program at it",
            program.display()
        ))
        .into(),
        io::ErrorKind::PermissionDenied => ConfigError::new(format!(
            "ffmpeg could not be started: `{}` is not executable",
            program.display()
        ))
        .into(),
        _ => Error::Other(anyhow::anyhow!("could not start ffmpeg: {e}")),
    }
}

fn process_error(what: &str, e: io::Error) -> Error {
    Error::Other(anyhow::Error::new(e).context(format!("could not {what} ffmpeg")))
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
/// § 5.1). Returns what it found instead, in words.
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
        _ => Err("a format it cannot check"),
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
