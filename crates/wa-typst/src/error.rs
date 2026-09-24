//! Error types for rendering.

/// Result type for rendering operations.
pub type Result<T> = std::result::Result<T, RenderError>;

/// Errors that can occur during rendering.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RenderError {
    /// Typst compilation failed.
    #[error("compilation error: {message}")]
    Compile {
        /// Error message.
        message: String,
        /// Line number (1-indexed) if available.
        line: Option<usize>,
        /// Column number (1-indexed) if available.
        column: Option<usize>,
    },

    /// Failed to serialize input to JSON.
    #[error("input serialization error: {0}")]
    InputSerialization(#[source] serde_json::Error),

    /// Failed to export to the requested format.
    #[error("export error: {0}")]
    Export(String),

    /// Template not found.
    #[error("template not found: {0}")]
    TemplateNotFound(String),

    /// Invalid configuration.
    #[error("invalid configuration: {0}")]
    Config(String),
}

impl RenderError {
    /// Create a compilation error with message and optional span.
    pub fn compile(message: impl Into<String>, line: Option<usize>, column: Option<usize>) -> Self {
        Self::Compile {
            message: message.into(),
            line,
            column,
        }
    }

    /// Create an input serialization error.
    pub fn input_serialization(err: serde_json::Error) -> Self {
        Self::InputSerialization(err)
    }

    /// Create an export error.
    pub fn export(err: impl Into<String>) -> Self {
        Self::Export(err.into())
    }
}

impl From<RenderError> for wa_core::Error {
    fn from(err: RenderError) -> Self {
        wa_core::Error::Other(anyhow::anyhow!("{err}"))
    }
}
