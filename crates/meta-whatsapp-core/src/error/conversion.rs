//! The media conversion node of the error tree: [`ConversionError`].

use std::time::Duration;

use super::ErrorKind;

/// A [`crate::media::MediaConverter`] failed.
///
/// A conversion runs before an upload, on the integrator's side (an
/// `ffmpeg` process, a conversion service), and sends nothing to Meta:
/// [`ConversionError::may_have_been_sent`] is always `false`. Its failures
/// are not input errors alone, which is why it is a node of its own and
/// not a [`ValidationError`](super::ValidationError): a converter that is
/// not installed or lacks an encoder is the deployment's to fix, a
/// timeout is worth trying again, and an output of the wrong format is a
/// converter that misbehaves. Each variant decides its [`ErrorKind`] and
/// [`ConversionError::is_retryable`].
///
/// An input that is empty, or larger than the converter takes (or whose
/// result would be larger than an upload takes), is not this node: it is
/// a [`ValidationError`](super::ValidationError) on field `input`, like
/// any input refused before work starts.
///
/// The messages carry no converter diagnostics (an `ffmpeg`'s stderr
/// names files and URLs on its host) and nothing of the input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConversionError {
    /// The converter's program could not be started: not installed, not
    /// at the configured path, or not executable. The deployment's to fix.
    #[error("media converter unavailable: {0}")]
    BinaryMissing(String),
    /// The converter runs, but cannot produce the format asked for (an
    /// `ffmpeg` built without libopus). The deployment's to fix.
    #[error("media converter lacks an encoder: {0}")]
    EncoderMissing(String),
    /// The conversion ran past the converter's time limit and was
    /// stopped. The one retryable variant.
    #[error("media conversion ran past {0:?} and was stopped")]
    Timeout(Duration),
    /// The converter could not read the input: not media it decodes, or
    /// a format it is not allowed to read.
    #[error("media converter refused the input: {0}")]
    InputRejected(String),
    /// The converter produced something other than the format asked for:
    /// it is not the program it should be.
    #[error("media converter output is not the format asked for: {0}")]
    OutputInvalid(String),
    /// The converter stopped before it finished, for a reason other than
    /// its time limit: killed by a signal (the system's out-of-memory
    /// killer), a broken pipe to it, or a start that failed for a reason
    /// other than a missing program (out of processes).
    #[error("media converter was stopped: {0}")]
    Killed(String),
}

impl ConversionError {
    /// Classification, from the kinds that exist:
    ///
    /// | Variant | Kind |
    /// | --- | --- |
    /// | `BinaryMissing`, `EncoderMissing` | [`ErrorKind::FeatureNotAvailable`]: conversion is not available on this deployment |
    /// | `Timeout` | [`ErrorKind::ServiceUnavailable`]: the converter is busy or slow |
    /// | `InputRejected` | [`ErrorKind::InvalidParameter`]: fix the input |
    /// | `OutputInvalid`, `Killed` | [`ErrorKind::Unknown`] |
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::BinaryMissing(_) | Self::EncoderMissing(_) => ErrorKind::FeatureNotAvailable,
            Self::Timeout(_) => ErrorKind::ServiceUnavailable,
            Self::InputRejected(_) => ErrorKind::InvalidParameter,
            Self::OutputInvalid(_) | Self::Killed(_) => ErrorKind::Unknown,
        }
    }

    /// Whether the same conversion may succeed later: only after a
    /// [`Timeout`](Self::Timeout). A missing binary or encoder needs the
    /// deployment changed, a refused input another input, and a converter
    /// that misbehaves or is killed someone to look.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout(_))
    }

    /// Always `false`: a conversion sends nothing to Meta.
    pub fn may_have_been_sent(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_decides_kind_retry_and_sentness() {
        let rows = [
            (
                ConversionError::BinaryMissing("x".into()),
                ErrorKind::FeatureNotAvailable,
                false,
            ),
            (
                ConversionError::EncoderMissing("x".into()),
                ErrorKind::FeatureNotAvailable,
                false,
            ),
            (
                ConversionError::Timeout(Duration::from_secs(1)),
                ErrorKind::ServiceUnavailable,
                true,
            ),
            (
                ConversionError::InputRejected("x".into()),
                ErrorKind::InvalidParameter,
                false,
            ),
            (
                ConversionError::OutputInvalid("x".into()),
                ErrorKind::Unknown,
                false,
            ),
            (
                ConversionError::Killed("x".into()),
                ErrorKind::Unknown,
                false,
            ),
        ];
        for (e, kind, retryable) in rows {
            assert_eq!(e.kind(), kind, "{e}");
            assert_eq!(e.is_retryable(), retryable, "{e}");
            assert!(!e.may_have_been_sent(), "{e}");
        }
    }
}
