//! Rendering pipeline.

use serde::Serialize;
use std::sync::Arc;
use typst::foundations::Bytes;
use typst::text::Font;
use typst::utils::LazyHash;
use typst::{Library, LibraryExt};

use crate::error::Result;
use crate::templates::Template;
use crate::world::WaWorld;

/// A rendered document ready for WhatsApp media upload.
#[derive(Clone)]
pub struct RenderedDocument {
    /// Document bytes (PDF or PNG).
    pub bytes: Vec<u8>,
    /// MIME type ("application/pdf" or "image/png").
    pub mime_type: &'static str,
    /// Suggested filename.
    pub filename: String,
}

/// Renders Typst templates to PDF or PNG.
#[derive(Clone)]
pub struct Renderer {
    /// Typst library (cached).
    library: Arc<LazyHash<Library>>,
    /// Font book (cached).
    font_book: Arc<LazyHash<typst::text::FontBook>>,
    /// Fonts.
    fonts: Vec<Font>,
}

impl Renderer {
    /// Create a new renderer.
    pub fn new() -> Self {
        let library = Arc::new(LazyHash::new(Library::default()));

        // Build font book and load fonts
        let mut font_book = typst::text::FontBook::new();
        let mut fonts = Vec::new();

        // Load embedded fonts from typst-assets
        for data in typst_assets::fonts() {
            if let Some(font) = Font::new(Bytes::new(data), 0) {
                font_book.push(font.info().clone());
                fonts.push(font);
            }
        }

        let font_book = Arc::new(LazyHash::new(font_book));

        Self {
            library,
            font_book,
            fonts,
        }
    }

    /// Render a template to PDF.
    pub fn render_pdf<T: Serialize>(
        &self,
        template: &Template,
        input: &T,
    ) -> Result<RenderedDocument> {
        self.render(template, input, OutputFormat::Pdf)
    }

    /// Render a template to PNG (first page only).
    pub fn render_png<T: Serialize>(
        &self,
        _template: &Template,
        _input: &T,
        _ppi: f32,
    ) -> Result<RenderedDocument> {
        Err(crate::RenderError::Config(
            "PNG rendering not yet implemented".to_string(),
        ))
    }

    /// Render a template to PNG for all pages.
    pub fn render_png_pages<T: Serialize>(
        &self,
        _template: &Template,
        _input: &T,
        _ppi: f32,
    ) -> Result<RenderedDocument> {
        Err(crate::RenderError::Config(
            "PNG rendering not yet implemented".to_string(),
        ))
    }

    #[allow(clippy::unnecessary_wraps)]
    fn render<T: Serialize>(
        &self,
        template: &Template,
        _input: &T,
        _format: OutputFormat,
    ) -> Result<RenderedDocument> {
        // Create world.
        let _world = WaWorld::new(
            template.name(),
            template.source(),
            self.library.clone(),
            self.font_book.clone(),
            self.fonts.clone(),
        );

        // TODO: Compile the template and export to PDF
        // For now, just generate empty PDF to test compilation
        let bytes = b"%PDF-1.4\n1 0 obj\n<< >>\nendobj\nxref\n0 1\n0000000000 65535 f\ntrailer\n<< /Size 1 >>\nstartxref\n44\n%%EOF\n".to_vec();

        let filename = format!("{}.pdf", template.name());

        Ok(RenderedDocument {
            bytes,
            mime_type: "application/pdf",
            filename,
        })
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

enum OutputFormat {
    Pdf,
    #[allow(dead_code)]
    PngFirstPage(f32),
    #[allow(dead_code)]
    PngAllPages(f32),
}
