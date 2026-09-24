//! Public-API tests: every built-in template renders real PDFs and PNGs from
//! its fixture, every input field is visible in the output, output is
//! deterministic, and failures are typed errors rather than panics.
//!
//! What "real" means here: PNGs are fully decoded (with typst's own decoder)
//! and checked by size and pixel; PDFs are checked for typst-pdf's own
//! markers and for input-dependent bytes. Reading the *text* back out of a PDF
//! needs the uncompressed export, so that test lives next to it in
//! `src/render.rs`.

use pretty_assertions::assert_eq;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use typst::foundations::Bytes;
use typst::visualize::{ExchangeFormat, RasterImage};
use wa_typst::{
    Diagnostic, InvoiceInput, MAX_PNG_PIXELS, ReceiptInput, RenderError, RenderedDocument,
    Renderer, Template, VoucherInput,
};

// ---------------------------------------------------------------- helpers

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn typed<T: DeserializeOwned>(name: &str) -> T {
    serde_json::from_value(fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn builtins() -> [(Template, Value); 3] {
    [
        (Template::invoice(), fixture("invoice")),
        (Template::receipt(), fixture("receipt")),
        (Template::voucher(), fixture("voucher")),
    ]
}

/// Fully decode a PNG and return it.
fn decode_png(png: &[u8]) -> RasterImage {
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "no PNG signature");
    RasterImage::plain(Bytes::new(png.to_vec()), ExchangeFormat::Png)
        .unwrap_or_else(|e| panic!("PNG does not decode: {e}"))
}

fn png_size(png: &[u8]) -> (u32, u32) {
    let image = decode_png(png);
    (image.width(), image.height())
}

fn rgba_at(png: &[u8], x: u32, y: u32) -> [u8; 4] {
    decode_png(png).dynamic().to_rgba8().get_pixel(x, y).0
}

fn compile_error(result: Result<impl std::fmt::Debug, RenderError>) -> (String, Vec<Diagnostic>) {
    match result {
        Err(RenderError::Compile {
            template,
            diagnostics,
        }) => (template, diagnostics),
        other => panic!("expected a compile error, got {other:?}"),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

const TOKEN_TEMPLATE: &str = "#let data = json(bytes(sys.inputs.data))
#set page(width: 400pt, height: auto, margin: 10pt)
Before #data.token after";

// ---------------------------------------------------------------- fixtures

#[test]
fn fixtures_round_trip_through_their_typed_inputs() {
    // Exact equality: a fixture cannot lack a field of its input type or
    // carry one the type does not have, so the field-by-field tests below
    // cover every field of the public struct.
    fn round_trip<T: Serialize + DeserializeOwned>(name: &str) {
        let value = fixture(name);
        let typed: T = typed(name);
        assert_eq!(
            serde_json::to_value(&typed).expect("serializes"),
            value,
            "{name}"
        );
    }
    round_trip::<InvoiceInput>("invoice");
    round_trip::<ReceiptInput>("receipt");
    round_trip::<VoucherInput>("voucher");
}

// ---------------------------------------------------------------- PDF

fn assert_real_pdf(template: &Template, input: &impl Serialize, trivial_len: usize) {
    let name = template.name();
    let pdf = Renderer::new()
        .render_pdf(template, input)
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(pdf.mime_type, "application/pdf");
    assert_eq!(pdf.filename, format!("{name}.pdf"));
    assert!(pdf.bytes.starts_with(b"%PDF-1.7"), "{name}: no PDF header");
    assert!(
        pdf.bytes.trim_ascii_end().ends_with(b"%%EOF"),
        "{name}: no EOF marker"
    );
    // Written by typst-pdf with a font embedded, not by hand.
    assert!(
        contains(&pdf.bytes, b"/Creator(Typst "),
        "{name}: not written by typst-pdf"
    );
    assert!(
        contains(&pdf.bytes, b"/FontFile"),
        "{name}: no embedded font"
    );
    // Fonts and real content make it far bigger than an empty page.
    assert!(
        pdf.bytes.len() > trivial_len + 5_000,
        "{name}: {} bytes vs {trivial_len} for an empty document",
        pdf.bytes.len()
    );
}

#[test]
fn builtin_templates_render_real_pdfs() {
    let trivial = Renderer::new()
        .render_pdf(&Template::from_source("empty", ""), &json!({}))
        .expect("empty document renders")
        .bytes
        .len();
    // From the typed inputs, not the raw JSON: the public structs are what
    // must fit the templates.
    assert_real_pdf(
        &Template::invoice(),
        &typed::<InvoiceInput>("invoice"),
        trivial,
    );
    assert_real_pdf(
        &Template::receipt(),
        &typed::<ReceiptInput>("receipt"),
        trivial,
    );
    assert_real_pdf(
        &Template::voucher(),
        &typed::<VoucherInput>("voucher"),
        trivial,
    );
}

#[test]
fn pdf_bytes_depend_on_the_input() {
    let renderer = Renderer::new();
    let template = Template::from_source("token", TOKEN_TEMPLATE);
    let render = |token: &str| {
        renderer
            .render_pdf(&template, &json!({ "token": token }))
            .expect("renders")
            .bytes
    };
    let a = render("QZX-5521-TOKEN");
    assert_eq!(a, render("QZX-5521-TOKEN"), "same input, different bytes");
    assert_ne!(
        a,
        render("QZX-5522-TOKEN"),
        "one character changed, same bytes"
    );

    // Metadata is stored uncompressed, so here the input is literally visible.
    let titled = Template::from_source(
        "titled",
        format!("#set document(title: json(bytes(sys.inputs.data)).token)\n{TOKEN_TEMPLATE}"),
    );
    let pdf = renderer
        .render_pdf(&titled, &json!({ "token": "QZX-5521-TOKEN" }))
        .expect("renders");
    assert!(contains(&pdf.bytes, b"/Title(QZX-5521-TOKEN)"));
}

#[test]
fn builtin_pdfs_carry_their_title() {
    let pdf = Renderer::new()
        .render_pdf(&Template::invoice(), &fixture("invoice"))
        .expect("renders");
    assert!(contains(&pdf.bytes, b"/Title(Invoice INV-2026-0042)"));
    assert!(contains(&pdf.bytes, b"/Author(Vymalo Boutique GmbH)"));
}

// ---------------------------------------------------------------- PNG

#[test]
fn builtin_templates_render_pngs_at_the_requested_resolution() {
    let renderer = Renderer::new();
    // Page size in points × ppi / 72, rounded: A4 is 595.28 × 841.89 pt,
    // A5 419.53 × 595.28 pt, the voucher 400 × 210 pt.
    let cases = [
        (Template::invoice(), fixture("invoice"), 72.0, (595, 842)),
        (Template::invoice(), fixture("invoice"), 144.0, (1191, 1684)),
        (Template::receipt(), fixture("receipt"), 144.0, (839, 1191)),
        (Template::voucher(), fixture("voucher"), 144.0, (800, 420)),
        (Template::voucher(), fixture("voucher"), 200.0, (1111, 583)),
    ];
    for (template, input, ppi, expected) in cases {
        let png = renderer
            .render_png(&template, &input, ppi)
            .expect("renders");
        assert_eq!(png.mime_type, "image/png");
        assert_eq!(png.filename, format!("{}.png", template.name()));
        assert_eq!(
            png_size(&png.bytes),
            expected,
            "{} at {ppi} ppi",
            template.name()
        );
    }
}

#[test]
fn pngs_show_the_page_background() {
    let renderer = Renderer::new();
    let invoice = renderer
        .render_png(&Template::invoice(), &fixture("invoice"), 72.0)
        .expect("renders");
    assert_eq!(rgba_at(&invoice.bytes, 0, 0), [255, 255, 255, 255]);

    // `accent_color` is the voucher background: #0B6E4F.
    let voucher = renderer
        .render_png(&Template::voucher(), &fixture("voucher"), 72.0)
        .expect("renders");
    let [r, g, b, a] = rgba_at(&voucher.bytes, 0, 0);
    assert_eq!(a, 255);
    for (got, want) in [(r, 0x0B), (g, 0x6E), (b, 0x4F)] {
        assert!(got.abs_diff(want) <= 1, "voucher background {r},{g},{b}");
    }
}

/// Replace one leaf with a different value of the same shape.
fn mutate(value: &mut Value, path: &str) {
    match value {
        Value::String(s) if path == "accent_color" => "#6E0B4F".clone_into(s),
        Value::String(s) => s.push('Q'),
        Value::Number(n) => {
            let Some(n) = n.as_u64() else {
                panic!("{path}: not an unsigned number")
            };
            *value = json!(n + 1);
        }
        other => panic!("{path}: not a leaf: {other}"),
    }
}

fn leaf_paths(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        // Either would leave a field of the input type unchecked.
        Value::Null => panic!("{path}: null in the fixture; set every optional field"),
        Value::Array(items) if items.is_empty() => {
            panic!("{path}: empty list in the fixture; give it at least one item")
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                leaf_paths(item, &format!("{path}/{i}"), out);
            }
        }
        Value::Object(fields) => {
            for (key, item) in fields {
                leaf_paths(item, &format!("{path}/{key}"), out);
            }
        }
        _ => out.push(path.to_owned()),
    }
}

#[test]
fn every_input_field_changes_the_rendered_image() {
    // The fixtures set every field (see the round-trip test), so this proves
    // each field of each input type is actually drawn.
    let renderer = Renderer::new();
    for (template, input) in builtins() {
        let baseline = renderer
            .render_png(&template, &input, 72.0)
            .expect("renders")
            .bytes;
        let mut paths = Vec::new();
        leaf_paths(&input, "", &mut paths);
        assert!(
            paths.len() >= 7,
            "{}: only {} fields",
            template.name(),
            paths.len()
        );
        for path in paths {
            let mut changed = input.clone();
            let leaf = changed.pointer_mut(&path).expect("path exists");
            let key = path.rsplit('/').next().unwrap_or_default().to_owned();
            mutate(leaf, &key);
            let png = renderer
                .render_png(&template, &changed, 72.0)
                .unwrap_or_else(|e| panic!("{} {path}: {e}", template.name()))
                .bytes;
            assert_ne!(
                png,
                baseline,
                "{}: changing `{path}` changed nothing",
                template.name()
            );
        }
    }
}

#[test]
fn render_png_pages_returns_every_page_in_order() {
    let template = Template::from_source(
        "multi",
        "#set page(width: 100pt, height: 50pt)\nOne #pagebreak() Two #pagebreak() Three",
    );
    let renderer = Renderer::new();
    let pages = renderer
        .render_png_pages(&template, &json!({}), 72.0)
        .expect("renders");
    let names: Vec<&str> = pages.iter().map(|p| p.filename.as_str()).collect();
    assert_eq!(names, ["multi-1.png", "multi-2.png", "multi-3.png"]);
    for page in &pages {
        assert_eq!(page.mime_type, "image/png");
        assert_eq!(png_size(&page.bytes), (100, 50));
    }
    assert_ne!(pages[0].bytes, pages[1].bytes);
    assert_ne!(pages[1].bytes, pages[2].bytes);

    let first = renderer
        .render_png(&template, &json!({}), 72.0)
        .expect("renders");
    assert_eq!(first.bytes, pages[0].bytes, "render_png is the first page");
    assert_eq!(first.filename, "multi.png");
}

#[test]
fn invalid_resolutions_are_rejected() {
    let renderer = Renderer::new();
    let template = Template::voucher();
    let input = fixture("voucher");
    for ppi in [0.0, -72.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(
            matches!(
                renderer.render_png(&template, &input, ppi),
                Err(RenderError::InvalidPpi { .. })
            ),
            "render_png accepted {ppi}"
        );
        assert!(
            matches!(
                renderer.render_png_pages(&template, &input, ppi),
                Err(RenderError::InvalidPpi { .. })
            ),
            "render_png_pages accepted {ppi}"
        );
    }
}

#[test]
fn oversized_rasters_are_refused_before_allocating() {
    // Without the budget typst-render would try to allocate this buffer and
    // panic (or abort) inside the library.
    let renderer = Renderer::new();
    let huge = Template::from_source("huge", "#set page(width: 10000pt, height: 10000pt)\nx");
    assert!(10_000u64 * 10_000 > u64::from(MAX_PNG_PIXELS));
    match renderer.render_png(&huge, &json!({}), 72.0) {
        Err(RenderError::ImageTooLarge { page: 1, limit, .. }) => assert_eq!(limit, MAX_PNG_PIXELS),
        other => panic!("expected ImageTooLarge, got {other:?}"),
    }
    match renderer.render_png(&Template::voucher(), &fixture("voucher"), 1.0e9) {
        Err(RenderError::ImageTooLarge { page: 1, .. }) => {}
        other => panic!("expected ImageTooLarge, got {other:?}"),
    }

    let second_page_huge = Template::from_source(
        "mixed",
        "#set page(width: 100pt, height: 100pt)\nsmall\n#set page(width: 10000pt, height: 10000pt)\nbig",
    );
    match renderer.render_png_pages(&second_page_huge, &json!({}), 72.0) {
        Err(RenderError::ImageTooLarge { page: 2, .. }) => {}
        other => panic!("expected ImageTooLarge on page 2, got {other:?}"),
    }
    // The first page alone is fine.
    assert!(
        renderer
            .render_png(&second_page_huge, &json!({}), 72.0)
            .is_ok()
    );
}

// ---------------------------------------------------------------- determinism

#[test]
fn identical_input_gives_identical_bytes() {
    let date = time::macros::date!(2026 - 09 - 24);
    for (template, input) in builtins() {
        // Separate renderers, and a render of something else in between, so
        // neither shared state nor typst's cache can hide a difference.
        let first = Renderer::new().with_today(date);
        let second = Renderer::new().with_today(date);
        let pdf = first.render_pdf(&template, &input).expect("renders").bytes;
        let png = first
            .render_png(&template, &input, 72.0)
            .expect("renders")
            .bytes;
        second
            .render_pdf(&Template::from_source("other", "other"), &json!({}))
            .expect("renders");
        assert_eq!(
            pdf,
            second.render_pdf(&template, &input).expect("renders").bytes,
            "{} pdf",
            template.name()
        );
        assert_eq!(
            png,
            second
                .render_png(&template, &input, 72.0)
                .expect("renders")
                .bytes,
            "{} png",
            template.name()
        );
    }
}

#[test]
fn renders_on_other_threads_are_identical() {
    let renderer = Renderer::new();
    let input = fixture("invoice");
    let here = renderer
        .render_pdf(&Template::invoice(), &input)
        .expect("renders")
        .bytes;
    let there: Vec<Vec<u8>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    renderer
                        .render_pdf(&Template::invoice(), &input)
                        .expect("renders")
                        .bytes
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect()
    });
    for bytes in there {
        assert_eq!(bytes, here);
    }
}

#[test]
fn today_is_fixed_by_the_caller_never_the_clock() {
    let template = Template::from_source("dated", "Today is #datetime.today().display()");

    let (_, diagnostics) = compile_error(Renderer::new().render_pdf(&template, &json!({})));
    assert!(!diagnostics[0].message.is_empty());

    let on = |date| {
        Renderer::new()
            .with_today(date)
            .render_pdf(&template, &json!({}))
            .expect("renders")
            .bytes
    };
    let sept = on(time::macros::date!(2026 - 09 - 24));
    assert_eq!(sept, on(time::macros::date!(2026 - 09 - 24)));
    assert_ne!(sept, on(time::macros::date!(2026 - 09 - 25)));
    // The configured date is also the PDF creation date.
    assert!(contains(&sept, b"D:20260924"));

    // Without a date, no creation date is written at all.
    let undated = Renderer::new()
        .render_pdf(&Template::from_source("plain", "x"), &json!({}))
        .expect("renders");
    assert!(!contains(&undated.bytes, b"/CreationDate"));
}

// ---------------------------------------------------------------- errors

#[test]
fn syntax_errors_are_compile_errors_with_a_position() {
    let template = Template::from_source("broken", "first line\n#let = 1\n");
    let (name, diagnostics) = compile_error(Renderer::new().render_pdf(&template, &json!({})));
    assert_eq!(name, "broken");
    let first = &diagnostics[0];
    assert!(first.message.contains("expected pattern"), "{first}");
    assert_eq!(first.line, Some(2), "{first}");
    assert!(first.column.is_some(), "{first}");

    // An error whose span is unambiguous: the identifier `nope`, 1-based
    // column 5 in *characters* (it is byte 7, after two 2-byte letters).
    let template = Template::from_source("unknown", "ab\nçé #nope\n");
    let (_, diagnostics) = compile_error(Renderer::new().render_pdf(&template, &json!({})));
    let first = &diagnostics[0];
    assert!(first.message.contains("unknown variable: nope"), "{first}");
    assert_eq!((first.line, first.column), (Some(2), Some(5)), "{first}");
    assert!(
        first.to_string().starts_with("2:5: unknown variable: nope"),
        "{first}"
    );
}

#[test]
fn package_imports_fail_with_a_clear_error() {
    let template = Template::from_source("pkg", "#import \"@preview/x:0.1.0\": *\nHello");
    let err = Renderer::new()
        .render_pdf(&template, &json!({}))
        .expect_err("must fail");
    let message = err.to_string();
    assert!(
        message.contains("package imports are disabled"),
        "{message}"
    );
    assert!(message.contains("@preview/x:0.1.0"), "{message}");
    let (_, diagnostics) = compile_error(Err::<(), _>(err));
    assert_eq!(diagnostics[0].line, Some(1));
}

#[test]
fn file_access_fails_with_a_clear_error() {
    for source in [
        "#read(\"/etc/hostname\")",
        "#image(\"logo.png\")",
        "#include \"other.typ\"",
    ] {
        let template = Template::from_source("files", source);
        let err = Renderer::new()
            .render_pdf(&template, &json!({}))
            .expect_err(source);
        assert!(
            matches!(err, RenderError::Compile { .. }),
            "{source}: {err:?}"
        );
        assert!(
            err.to_string().contains("file access is disabled"),
            "{source}: {err}"
        );
    }
}

#[test]
fn missing_input_fields_are_compile_errors_not_panics() {
    let (name, diagnostics) =
        compile_error(Renderer::new().render_pdf(&Template::invoice(), &json!({})));
    assert_eq!(name, "invoice");
    assert!(
        diagnostics[0].message.contains("invoice_number"),
        "{:?}",
        diagnostics[0]
    );
    assert!(diagnostics[0].line.is_some());

    // The wrong input type for a template is the same kind of error.
    let voucher: VoucherInput = typed("voucher");
    compile_error(Renderer::new().render_png(&Template::invoice(), &voucher, 72.0));
    compile_error(Renderer::new().render_png_pages(&Template::receipt(), &voucher, 72.0));
}

#[test]
fn unserializable_input_is_an_input_error() {
    let mut input = std::collections::BTreeMap::new();
    input.insert(vec![1u8], 1u8); // JSON object keys must be strings.
    let err = Renderer::new()
        .render_pdf(&Template::from_source("x", "x"), &input)
        .expect_err("must fail");
    assert!(matches!(err, RenderError::Input(_)), "{err:?}");
}

#[test]
fn render_errors_become_wa_core_errors_and_keep_their_type() {
    let err = Renderer::new()
        .render_png(&Template::voucher(), &fixture("voucher"), 0.0)
        .expect_err("must fail");
    let core: wa_core::Error = err.into();
    let wa_core::Error::Other(inner) = &core else {
        panic!("expected Error::Other, got {core:?}");
    };
    assert!(matches!(
        inner.downcast_ref::<RenderError>(),
        Some(RenderError::InvalidPpi { .. })
    ));
    assert_eq!(core.kind(), wa_core::ErrorKind::Unknown);
    assert!(!core.is_retryable());
}

// ---------------------------------------------------------------- API shape

#[test]
fn rendered_document_debug_omits_the_bytes() {
    let doc = RenderedDocument {
        bytes: vec![7; 4096],
        mime_type: "application/pdf",
        filename: "x.pdf".to_owned(),
    };
    let debug = format!("{doc:?}");
    assert!(debug.contains("[4096 bytes]"), "{debug}");
    assert!(debug.len() < 200, "{debug}");
}

#[test]
fn renderer_and_template_are_cheap_to_share() {
    fn shareable<T: Send + Sync + Clone + 'static>() {}
    fn copy<T: Copy>() {}
    fn error<T: std::error::Error + Send + Sync + 'static>() {}
    shareable::<Renderer>();
    copy::<Renderer>();
    shareable::<Template>();
    shareable::<RenderedDocument>();
    error::<RenderError>();

    assert_eq!(Template::invoice().name(), "invoice");
    assert_eq!(Template::receipt().name(), "receipt");
    assert_eq!(Template::voucher().name(), "voucher");
    let custom = Template::from_source("mine", "Hi");
    assert_eq!((custom.name(), custom.source()), ("mine", "Hi"));
    assert_eq!(Renderer::new().today(), None);
}
