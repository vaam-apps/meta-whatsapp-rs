//! Render Typst templates to PDF and PNG for WhatsApp document and image
//! messages.
//!
//! A [`Template`] is Typst source; its input is any [`serde::Serialize`] value,
//! handed to the template as JSON in `sys.inputs.data`. A [`Renderer`]
//! compiles the two in a sandbox and exports with `typst-pdf` or
//! `typst-render`. The result is a [`RenderedDocument`] (bytes, MIME type,
//! filename) ready for `media().upload()` and a document or image message, or
//! a template header.
//!
//! Built-in templates, each with a typed input: [`Template::invoice`]
//! ([`InvoiceInput`]), [`Template::receipt`] ([`ReceiptInput`]),
//! [`Template::voucher`] ([`VoucherInput`]). Sample inputs live in
//! `crates/wa-typst/tests/fixtures/`.
//!
//! # Never render OTPs or secrets
//!
//! Authentication codes, PINs, passwords and access tokens must never be
//! rendered into a document or image. Media is stored by Meta and on the
//! customer's phone, forwarded, and screenshotted; none of that can be taken
//! back. OTPs go only through authentication templates.
//!
//! # Sandbox and determinism
//!
//! - Fonts are the ones bundled by `typst-assets`, never system fonts. Set
//!   them by family:
//!   `"Libertinus Serif"` (typst's default),
//!   `"New Computer Modern"`,
//!   `"New Computer Modern Math"`,
//!   `"DejaVu Sans Mono"`.
//!   Any other family silently falls back to these.
//! - No file system and no network: a template reads nothing but its input,
//!   and `@preview` package imports fail with a
//!   [`RenderError::Compile`] saying packages are disabled.
//! - No clock: `datetime.today()` is the date given to
//!   [`Renderer::with_today`] and an error otherwise.
//!
//! The same template, input and date therefore produce byte-identical files
//! everywhere.
//!
//! Input strings placed with `#data.field` (as the built-in templates do) are
//! text, not markup: an input value of `*bold* #panic()` is printed exactly as
//! written. Only a template that passes input to `eval` would run it.
//!
//! # Example
//!
//! ```no_run
//! use wa_typst::{InvoiceInput, Renderer, Template};
//!
//! # fn run(input: InvoiceInput) -> wa_core::Result<()> {
//! let renderer = Renderer::new();
//! let pdf = renderer.render_pdf(&Template::invoice(), &input)?;
//! assert_eq!(pdf.mime_type, "application/pdf");
//! // Upload `pdf.bytes` as `pdf.filename`, then send a document message.
//! # Ok(())
//! # }
//! ```
//!
//! A custom template:
//!
//! ```
//! use wa_typst::{Renderer, Template};
//!
//! # fn main() -> Result<(), wa_typst::RenderError> {
//! let template = Template::from_source(
//!     "greeting",
//!     r#"#let data = json(bytes(sys.inputs.data))
//! #set page(width: 200pt, height: 100pt)
//! Hello, #data.name!"#,
//! );
//! let png = Renderer::new().render_png(&template, &serde_json::json!({ "name": "Ada" }), 144.0)?;
//! assert_eq!(png.filename, "greeting.png");
//! assert!(png.bytes.starts_with(b"\x89PNG"));
//! # Ok(())
//! # }
//! ```

mod error;
mod input;
mod render;
mod templates;
mod world;

#[cfg(test)]
mod pdf_text;

pub use error::{Diagnostic, RenderError};
pub use input::{InvoiceInput, LineItem, Party, ReceiptInput, SummaryLine, VoucherInput};
pub use render::{MAX_PNG_PIXELS, RenderedDocument, Renderer};
pub use templates::Template;
