//! [`RenderError`] and the [`Diagnostic`]s it carries.

use std::fmt;

use typst::WorldExt as _;
use typst::diag::{Severity, SourceDiagnostic};

use crate::world::RenderWorld;

/// Why a render failed.
///
/// Converts into [`meta_whatsapp_core::Error::Other`] with `?`; the typed value survives
/// the conversion (`err.downcast_ref::<RenderError>()` on the inner
/// [`anyhow::Error`]), so callers can still branch on the variant.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RenderError {
    /// The template did not compile against the given input: a syntax error,
    /// a missing input field, a denied file or package import, a call to
    /// `datetime.today()` with no date configured, ...
    #[error(
        "template `{template}` failed to compile: {}",
        DiagnosticList(diagnostics)
    )]
    Compile {
        /// Name of the template, see [`Template::name`](crate::Template::name).
        template: String,
        /// Every error typst reported, in source order. Never empty.
        diagnostics: Vec<Diagnostic>,
    },

    /// The input could not be serialized to JSON (e.g. a map with non-string
    /// keys). Nothing was compiled.
    #[error("template input could not be serialized to JSON: {0}")]
    Input(#[source] serde_json::Error),

    /// The requested PNG resolution is not a finite, positive number.
    #[error("PNG resolution must be a finite number of pixels per inch above zero, got {ppi}")]
    InvalidPpi {
        /// The rejected value.
        ppi: f32,
    },

    /// Rasterizing a page at the requested resolution would exceed
    /// [`MAX_PNG_PIXELS`](crate::MAX_PNG_PIXELS).
    #[error(
        "page {page} at {ppi} ppi would exceed the limit of {limit} pixels; lower the resolution"
    )]
    ImageTooLarge {
        /// 1-based page number.
        page: usize,
        /// The requested resolution.
        ppi: f32,
        /// The pixel budget per page.
        limit: u32,
    },

    /// The document compiled but could not be written out (a PDF export
    /// error, a PNG encoding error, or a document without pages).
    #[error("{format} export failed: {message}")]
    Export {
        /// `"pdf"` or `"png"`.
        format: &'static str,
        /// What the exporter said.
        message: String,
    },
}

impl From<RenderError> for meta_whatsapp_core::Error {
    fn from(err: RenderError) -> Self {
        // `anyhow::Error::new`, not `anyhow!("{err}")`: the latter would
        // flatten the variant into a string nobody can branch on.
        meta_whatsapp_core::Error::Other(anyhow::Error::new(err))
    }
}

impl RenderError {
    /// Build [`RenderError::Compile`] from typst's diagnostics, resolving
    /// spans to line/column against the world that produced them.
    pub(crate) fn compile(
        template: &str,
        world: &RenderWorld,
        errors: &[SourceDiagnostic],
    ) -> Self {
        let mut diagnostics: Vec<Diagnostic> = errors
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| Diagnostic::from_typst(world, d))
            .collect();
        if diagnostics.is_empty() {
            // typst only returns `Err` with at least one error, but the
            // variant promises a non-empty list, so never hand out an empty one.
            diagnostics.push(Diagnostic {
                message: "compilation failed without a diagnostic".to_owned(),
                line: None,
                column: None,
                hints: Vec::new(),
            });
        }
        Self::Compile {
            template: template.to_owned(),
            diagnostics,
        }
    }

    /// Build [`RenderError::Export`] from diagnostics raised by an exporter.
    pub(crate) fn export(format: &'static str, errors: &[SourceDiagnostic]) -> Self {
        let message = errors
            .iter()
            .map(|d| d.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        Self::Export { format, message }
    }
}

/// One error reported by the typst compiler.
///
/// Positions refer to the template source. They are `None` when the error has
/// no location in it (e.g. a failure raised while laying out the document).
/// A message can contain input values: a template's own `panic(..)` or
/// `assert` text, or a hint quoting a string typst could not use. Log it with
/// the same care as the input itself (which must never hold a secret anyway).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// What went wrong, as typst phrased it.
    pub message: String,
    /// 1-based line in the template source.
    pub line: Option<usize>,
    /// 1-based column (in characters) in the template source.
    pub column: Option<usize>,
    /// Typst's suggestions for fixing it.
    pub hints: Vec<String>,
}

impl Diagnostic {
    fn from_typst(world: &RenderWorld, diag: &SourceDiagnostic) -> Self {
        let main = world.main_source();
        let position = (diag.span.id() == Some(main.id()))
            .then(|| world.range(diag.span))
            .flatten()
            .and_then(|range| main.lines().byte_to_line_column(range.start));
        Self {
            message: diag.message.to_string(),
            line: position.map(|(line, _)| line + 1),
            column: position.map(|(_, column)| column + 1),
            hints: diag.hints.iter().map(|hint| hint.v.to_string()).collect(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(column)) => write!(f, "{line}:{column}: {}", self.message)?,
            _ => f.write_str(&self.message)?,
        }
        for hint in &self.hints {
            write!(f, " (hint: {hint})")?;
        }
        Ok(())
    }
}

/// Displays the first diagnostic and how many more there are, so a log line
/// stays one line.
struct DiagnosticList<'a>(&'a [Diagnostic]);

impl fmt::Display for DiagnosticList<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            [] => f.write_str("no diagnostic"),
            [only] => only.fmt(f),
            [first, rest @ ..] => write!(f, "{first} (and {} more)", rest.len()),
        }
    }
}
