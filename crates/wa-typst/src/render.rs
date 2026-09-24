//! [`Renderer`]: compile a [`Template`] against an input and export the
//! result with `typst-pdf` or `typst-render`.

use std::fmt;

use serde::Serialize;
use typst::diag::Warned;
use typst::foundations::{Datetime, Smart};
use typst::utils::Scalar;
use typst_layout::{Page, PagedDocument};
use typst_pdf::{PdfOptions, Timestamp};
use typst_render::RenderOptions;

use crate::error::RenderError;
use crate::templates::Template;
use crate::world::RenderWorld;

/// Upper bound on the pixels of one rasterized page.
///
/// typst-render allocates the whole RGBA buffer up front and panics (or the
/// allocator aborts) when it cannot, so an unbounded resolution or a huge
/// custom page size would take the process down. 40 M pixels is an A4 page at
/// about 640 ppi, far beyond anything WhatsApp accepts (images ≤ 5 MB).
pub const MAX_PNG_PIXELS: u32 = 40_000_000;

/// How many evictions an unused entry of typst's global memoization cache
/// survives. typst never evicts on its own; without this a long-running
/// service that renders many distinct inputs grows without bound. 10 is what
/// the typst CLI uses in watch mode.
const CACHE_MAX_AGE: usize = 10;

/// Typographic points per inch.
const PT_PER_INCH: f64 = 72.0;

/// A rendered file, ready for `media().upload()` and a document or image
/// message.
#[derive(Clone, PartialEq, Eq)]
pub struct RenderedDocument {
    /// The file content.
    pub bytes: Vec<u8>,
    /// `"application/pdf"` or `"image/png"`.
    pub mime_type: &'static str,
    /// `{template name}.pdf`, `{template name}.png`, or
    /// `{template name}-{page}.png` for [`Renderer::render_png_pages`].
    pub filename: String,
}

impl fmt::Debug for RenderedDocument {
    // The bytes are a whole PDF or PNG; a derived `Debug` would print every one.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RenderedDocument")
            .field("bytes", &format_args!("[{} bytes]", self.bytes.len()))
            .field("mime_type", &self.mime_type)
            .field("filename", &self.filename)
            .finish()
    }
}

/// Renders [`Template`]s to PDF or PNG.
///
/// Cheap to copy and share: the bundled fonts are process-wide, loaded on the
/// first render. Rendering is synchronous and CPU-bound (tens to hundreds of
/// milliseconds); from async code, call it inside
/// `tokio::task::spawn_blocking` or equivalent.
///
/// Output is deterministic: the same template, input and
/// [`today`](Self::with_today) give byte-identical files on every machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Renderer {
    today: Option<time::Date>,
}

impl Renderer {
    /// A renderer with no date configured: templates calling
    /// `datetime.today()` fail to compile, and PDFs carry no creation date.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fix the date `datetime.today()` returns, which is also written as the
    /// PDF creation date.
    ///
    /// There is deliberately no fallback to the system clock: output would
    /// then change from one day to the next for the same input, and a
    /// document silently dated by a server in another time zone is worse than
    /// an error.
    #[must_use]
    pub fn with_today(mut self, date: time::Date) -> Self {
        self.today = Some(date);
        self
    }

    /// The date set with [`Renderer::with_today`], if any.
    pub fn today(&self) -> Option<time::Date> {
        self.today
    }

    /// Render every page of `template` to one PDF.
    pub fn render_pdf(
        &self,
        template: &Template,
        input: &impl Serialize,
    ) -> Result<RenderedDocument, RenderError> {
        let document = self.compile(template, input)?;
        Ok(RenderedDocument {
            bytes: self.export_pdf(&document, false)?,
            mime_type: "application/pdf",
            filename: format!("{}.pdf", template.name()),
        })
    }

    /// Render the first page of `template` to a PNG at `ppi` pixels per inch.
    ///
    /// 150–200 ppi suits WhatsApp image headers; keep the result under
    /// WhatsApp's 5 MB image limit.
    pub fn render_png(
        &self,
        template: &Template,
        input: &impl Serialize,
        ppi: f32,
    ) -> Result<RenderedDocument, RenderError> {
        let pixel_per_pt = pixel_per_pt(ppi)?;
        let document = self.compile(template, input)?;
        let page = document
            .pages()
            .first()
            .ok_or_else(|| RenderError::Export {
                format: "png",
                message: "the document has no pages".to_owned(),
            })?;
        Ok(RenderedDocument {
            bytes: rasterize(page, 1, ppi, pixel_per_pt)?,
            mime_type: "image/png",
            filename: format!("{}.png", template.name()),
        })
    }

    /// Render every page of `template` to its own PNG, in page order.
    pub fn render_png_pages(
        &self,
        template: &Template,
        input: &impl Serialize,
        ppi: f32,
    ) -> Result<Vec<RenderedDocument>, RenderError> {
        let pixel_per_pt = pixel_per_pt(ppi)?;
        let document = self.compile(template, input)?;
        document
            .pages()
            .iter()
            .enumerate()
            .map(|(index, page)| {
                let number = index + 1;
                Ok(RenderedDocument {
                    bytes: rasterize(page, number, ppi, pixel_per_pt)?,
                    mime_type: "image/png",
                    filename: format!("{}-{number}.png", template.name()),
                })
            })
            .collect()
    }

    /// The sandboxed world `template` compiles in, with `input` as
    /// `sys.inputs.data`.
    pub(crate) fn world(
        self,
        template: &Template,
        input: &impl Serialize,
    ) -> Result<RenderWorld, RenderError> {
        let json = serde_json::to_string(input).map_err(RenderError::Input)?;
        Ok(RenderWorld::new(template.source(), json, self.today))
    }

    /// Lay `template` out. Warnings are dropped: they never change whether
    /// output exists, and the built-in templates are tested to produce none.
    pub(crate) fn compile(
        self,
        template: &Template,
        input: &impl Serialize,
    ) -> Result<PagedDocument, RenderError> {
        let world = self.world(template, input)?;
        let Warned { output, .. } = typst::compile::<PagedDocument>(&world);
        let result =
            output.map_err(|errors| RenderError::compile(template.name(), &world, &errors));
        typst::comemo::evict(CACHE_MAX_AGE);
        result
    }

    /// Export with typst-pdf. `pretty` leaves content streams uncompressed;
    /// only tests use it, to read the text back out of the file.
    pub(crate) fn export_pdf(
        self,
        document: &PagedDocument,
        pretty: bool,
    ) -> Result<Vec<u8>, RenderError> {
        let options = PdfOptions {
            // `timestamp` is only written when the document does not set its
            // own date; `None` writes no date at all, which keeps output
            // reproducible. `ident` stays `Auto` (derived from title and
            // author, never random).
            timestamp: self
                .today
                .map(|date| Timestamp::new_utc(Datetime::Date(date))),
            ident: Smart::Auto,
            pretty,
            ..PdfOptions::default()
        };
        typst_pdf::pdf(document, &options).map_err(|errors| RenderError::export("pdf", &errors))
    }
}

/// Validate `ppi` and convert it to typst-render's pixels per point.
fn pixel_per_pt(ppi: f32) -> Result<f64, RenderError> {
    if ppi.is_finite() && ppi > 0.0 {
        Ok(f64::from(ppi) / PT_PER_INCH)
    } else {
        Err(RenderError::InvalidPpi { ppi })
    }
}

/// Rasterize one page and encode it as PNG, refusing pages whose pixel buffer
/// would exceed [`MAX_PNG_PIXELS`] before typst-render tries to allocate it.
fn rasterize(
    page: &Page,
    number: usize,
    ppi: f32,
    pixel_per_pt: f64,
) -> Result<Vec<u8>, RenderError> {
    // The size formula typst-render uses (it rounds in f32, so this can be
    // off by a pixel; it is a budget, not a prediction).
    let size = page.frame.size();
    let width = (pixel_per_pt * size.x.to_pt()).round().max(1.0);
    let height = (pixel_per_pt * size.y.to_pt()).round().max(1.0);
    if width * height > f64::from(MAX_PNG_PIXELS) {
        return Err(RenderError::ImageTooLarge {
            page: number,
            ppi,
            limit: MAX_PNG_PIXELS,
        });
    }
    let options = RenderOptions {
        pixel_per_pt: Scalar::new(pixel_per_pt),
        render_bleed: false,
    };
    typst_render::render(page, &options)
        .encode_png()
        .map_err(|err| RenderError::Export {
            format: "png",
            message: err.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::input::{InvoiceInput, ReceiptInput, VoucherInput};
    use crate::pdf_text;

    fn fixture(name: &str) -> Value {
        let text = match name {
            "invoice" => include_str!("../tests/fixtures/invoice.json"),
            "receipt" => include_str!("../tests/fixtures/receipt.json"),
            "voucher" => include_str!("../tests/fixtures/voucher.json"),
            other => panic!("no fixture {other}"),
        };
        serde_json::from_str(text).expect("fixture is JSON")
    }

    fn builtins() -> [(Template, Value); 3] {
        [
            (Template::invoice(), fixture("invoice")),
            (Template::receipt(), fixture("receipt")),
            (Template::voucher(), fixture("voucher")),
        ]
    }

    /// The same compile and export as `render_pdf`, uncompressed so the text
    /// can be read back.
    fn pretty_pdf(renderer: Renderer, template: &Template, input: &impl Serialize) -> Vec<u8> {
        let document = renderer.compile(template, input).expect("compiles");
        renderer.export_pdf(&document, true).expect("exports")
    }

    const TOKEN_TEMPLATE: &str = "#let data = json(bytes(sys.inputs.data))
#set page(width: 400pt, height: auto, margin: 10pt)
Before #data.token after";

    #[test]
    fn input_text_is_laid_out_as_glyphs_in_the_pdf() {
        let template = Template::from_source("token", TOKEN_TEMPLATE);
        let pdf = pretty_pdf(
            Renderer::new(),
            &template,
            &json!({ "token": "QZX-5521-TOKEN" }),
        );
        assert!(pdf_text::contains(&pdf, "Before QZX-5521-TOKEN after"));
        // Negative control: the extractor does not just match anything.
        assert!(!pdf_text::contains(&pdf, "QZX-5522-TOKEN"));
    }

    #[test]
    fn input_strings_are_printed_not_evaluated() {
        let markup = r"*not bold* _x_ #panic() $x^2$ <label> @ref \ `raw` = heading";
        let template = Template::from_source("token", TOKEN_TEMPLATE);
        let pdf = pretty_pdf(Renderer::new(), &template, &json!({ "token": markup }));
        assert!(pdf_text::contains(&pdf, markup));
    }

    /// Every string and number of every fixture, except the voucher colour
    /// (which is the background, checked by pixel in `tests/render.rs`).
    fn text_leaves(value: &Value, path: &str, out: &mut Vec<(String, String)>) {
        match value {
            Value::String(s) if path != "accent_color" => out.push((path.to_owned(), s.clone())),
            Value::Number(n) => out.push((path.to_owned(), n.to_string())),
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    text_leaves(item, &format!("{path}[{i}]"), out);
                }
            }
            Value::Object(fields) => {
                for (key, item) in fields {
                    let path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    text_leaves(item, &path, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn every_fixture_value_is_in_the_pdf_text() {
        for (template, input) in builtins() {
            let pdf = pretty_pdf(Renderer::new(), &template, &input);
            let mut leaves = Vec::new();
            text_leaves(&input, "", &mut leaves);
            assert!(leaves.len() >= 6, "{}: fixture too small", template.name());
            for (path, text) in leaves {
                assert!(
                    pdf_text::contains(&pdf, &text),
                    "{}: `{path}` = {text:?} is not in the PDF text",
                    template.name()
                );
            }
        }
    }

    #[test]
    fn absent_optional_fields_are_left_out_not_printed_as_none() {
        let mut invoice: InvoiceInput = serde_json::from_value(fixture("invoice")).expect("typed");
        invoice.due_date = None;
        invoice.payment_terms = None;
        invoice.notes = None;
        invoice.adjustments.clear();
        for party in [&mut invoice.seller, &mut invoice.buyer] {
            party.address.clear();
            party.tax_id = None;
            party.email = None;
            party.phone = None;
        }
        let pdf = pretty_pdf(Renderer::new(), &Template::invoice(), &invoice);
        assert!(pdf_text::contains(&pdf, "INV-2026-0042"));
        for gone in [
            "none",
            "8 October 2026",
            "Tax ID",
            "Payment terms",
            "Returns are accepted",
            "Friedrichstraße",
            "billing@vymalo.example",
            "WELCOME10",
        ] {
            assert!(
                !pdf_text::contains(&pdf, gone),
                "invoice still shows {gone:?}"
            );
        }

        let mut receipt: ReceiptInput = serde_json::from_value(fixture("receipt")).expect("typed");
        receipt.adjustments.clear();
        receipt.delivery_address.clear();
        receipt.estimated_delivery = None;
        receipt.support_contact = None;
        let pdf = pretty_pdf(Renderer::new(), &Template::receipt(), &receipt);
        assert!(pdf_text::contains(&pdf, "ORD-88213"));
        for gone in [
            "none",
            "Delivery address",
            "Estimated delivery",
            "Questions?",
            "Shipping",
        ] {
            assert!(
                !pdf_text::contains(&pdf, gone),
                "receipt still shows {gone:?}"
            );
        }

        let mut voucher: VoucherInput = serde_json::from_value(fixture("voucher")).expect("typed");
        voucher.valid_until = None;
        voucher.terms = None;
        let pdf = pretty_pdf(Renderer::new(), &Template::voucher(), &voucher);
        assert!(pdf_text::contains(&pdf, "AUTUMN20"));
        for gone in ["none", "Valid until", "One use per customer"] {
            assert!(
                !pdf_text::contains(&pdf, gone),
                "voucher still shows {gone:?}"
            );
        }
    }

    #[test]
    fn builtin_templates_compile_without_warnings() {
        // A warning here is usually "unknown font family": the template asked
        // for a font that is not bundled and typst silently fell back.
        for (template, input) in builtins() {
            let world = Renderer::new()
                .world(&template, &input)
                .expect("serializes");
            let warned = typst::compile::<PagedDocument>(&world);
            assert!(
                warned.output.is_ok(),
                "{} failed to compile",
                template.name()
            );
            let warnings: Vec<String> = warned
                .warnings
                .iter()
                .map(|w| w.message.to_string())
                .collect();
            assert!(warnings.is_empty(), "{}: {warnings:?}", template.name());
        }
    }

    #[test]
    fn today_is_the_configured_date() {
        let template = Template::from_source("dated", "#datetime.today().display()");
        let renderer = Renderer::new().with_today(time::macros::date!(2026 - 09 - 24));
        let pdf = pretty_pdf(renderer, &template, &json!({}));
        assert!(pdf_text::contains(&pdf, "2026-09-24"));
    }
}
