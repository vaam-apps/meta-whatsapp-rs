//! Render Typst documents to PDF and PNG for WhatsApp media messages.
//!
//! # Overview
//!
//! This crate provides a minimal Typst rendering pipeline for e-commerce documents
//! (invoices, receipts, vouchers). Templates are Typst source files embedded in the
//! binary. JSON inputs are passed through `sys.inputs` and read with the Typst standard
//! library's `json()` function.
//!
//! # Output
//!
//! Rendered documents are returned as bytes with MIME type and filename, ready for
//! uploading via the WhatsApp Graph API media endpoint.
//!
//! # Security
//!
//! - No OTP codes or secrets are rendered (they go through authentication templates only).
//! - No network access; `@preview` package imports are rejected.
//! - No file system access; templates are embedded in the binary.
//! - Deterministic output: `today()` is fixed so identical inputs produce identical bytes.
//!
//! # Example
//!
//! ```no_run
//! use serde::Serialize;
//! use wa_typst::{Renderer, Template, RenderedDocument};
//!
//! #[derive(Serialize)]
//! struct InvoiceInput {
//!     invoice_number: String,
//!     buyer_name: String,
//!     // ... more fields
//! }
//!
//! async fn render_invoice() -> Result<(), Box<dyn std::error::Error>> {
//!     let renderer = Renderer::new();
//!     let template = Template::invoice();
//!     let input = InvoiceInput {
//!         invoice_number: "INV-001".to_string(),
//!         buyer_name: "Acme Corp".to_string(),
//!     };
//!
//!     let pdf = renderer.render_pdf(&template, &input)?;
//!     // pdf.bytes can be uploaded via wa_client
//!     Ok(())
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod render;
mod templates;
mod world;

pub use error::{RenderError, Result};
pub use render::{RenderedDocument, Renderer};
pub use templates::Template;
