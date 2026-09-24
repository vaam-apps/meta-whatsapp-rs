---
name: wa-rs-documents
description: "Rendering invoices, receipts and vouchers to PDF or PNG with wa-rs's Typst renderer (wa_rs::typst, feature typst) and sending them over WhatsApp - Renderer and with_today determinism, the built-in templates and their typed inputs (pre-formatted money and dates), custom Typst templates and the sandbox (bundled fonts, no files, no packages, no clock), running it off the async runtime, then media upload and a document message, image message or template header. Never for OTPs or secrets. Load when generating any document or image to send."
---

# wa-rs-documents

> **Verified against wa-rs 91431ae (2026-09-24).** On another revision, trust
> the code over this page (see `skills/README.md`).

Enable the `typst` feature of `wa-rs`; the crate is `wa_rs::typst`.

```toml
wa-rs = { git = "https://github.com/vaam-apps/wa-rs", rev = "…", features = ["typst"] }
```

## Render and send an order receipt

```rust
use wa_rs::client::messages::{Document, MediaSource, OutboundMessage};
use wa_rs::core::recipient::Recipient;
use wa_rs::typst::{ReceiptInput, Renderer, Template};

async fn send_receipt(
    client: &wa_rs::Client, pnid: &str, to: Recipient, input: ReceiptInput, today: time::Date,
) -> anyhow::Result<()> {
    // CPU-bound, synchronous (tens to hundreds of ms): keep it off the async workers.
    let pdf = tokio::task::spawn_blocking(move || {
        Renderer::new().with_today(today).render_pdf(&Template::receipt(), &input)
    })
    .await??;                                        // JoinError, then RenderError

    let media_id = client.media(pnid).upload(pdf.bytes, pdf.mime_type, &pdf.filename).await?;
    let doc = Document::new(MediaSource::id(media_id))
        .filename(format!("receipt-{}.pdf", order_no()))
        .caption("Thanks for your order!");
    client.messages(pnid).send(&OutboundMessage::new(to, doc)).await?;
    Ok(())
}
```

- `RenderedDocument { bytes, mime_type, filename }`: `mime_type` is
  `"application/pdf"` or `"image/png"`, `filename` is `{template}.pdf`,
  `{template}.png` or `{template}-{page}.png`. Rename it for the customer
  with `Document::filename`.
- `RenderError` converts into `wa_rs::Error::Other` with `?` (the typed error
  survives: `downcast_ref::<RenderError>()` on the inner `anyhow::Error`).
- Outside the 24-hour window a free-form document is refused: send it as a
  **template header** instead:
  `TemplateMessage::new("order_receipt", "en_US").header(Parameter::document_id(media_id, Some("receipt.pdf".into())))`.
- Limits: documents up to 100 MB, images (PNG/JPEG) 5 MB; checked before
  upload. Uploaded media ids live 30 days.

## Determinism: always set `with_today`

`Renderer::new()` has **no clock**: a template calling `datetime.today()`
fails to compile, and the PDF carries no creation date. `with_today(date)`
fixes both. There is deliberately no fallback to the system clock (output
would change day to day; a server in another time zone would misdate it).
Pass the date in the customer's or shop's time zone. Same template + input +
date ⇒ byte-identical output on every machine (bundled fonts only).

## Built-in templates

| Template | Input | Output |
| --- | --- | --- |
| `Template::invoice()` | `InvoiceInput` (`invoice_number`, `issue_date`, `due_date`, `seller`/`buyer: Party`, `currency`, `items: Vec<LineItem>`, `subtotal`, `adjustments: Vec<SummaryLine>`, `total`, `payment_terms`, `notes`) | A4 PDF |
| `Template::receipt()` | `ReceiptInput` (`merchant_name`, `order_number`, `order_date`, `customer_name`, `currency`, `items`, `subtotal`, `adjustments`, `total`, `payment_method`, `delivery_address`, `estimated_delivery`, `support_contact`) | A5 PDF |
| `Template::voucher()` | `VoucherInput` (`merchant_name`, `headline`, `description`, `code`, `valid_until`, `terms`, `accent_color` `#RRGGBB`) | 400×210 pt image header: render to **PNG** |

- **Money and dates are pre-formatted strings** (`"1,234.50"`, `"24 Sep
  2026"`), computed by your order system. The template does no arithmetic,
  so a total can never disagree with the order record through float
  rounding, and locale formatting stays with you.
- `Option` fields and empty lists are omitted from the layout.
- `LineItem { description, quantity: u32, unit_price, amount }`,
  `Party { name, address: Vec<String>, tax_id, email, phone }`,
  `SummaryLine { label, amount }` (discounts, shipping, tax).

## PNG for image messages and headers

```rust
let png = Renderer::new().with_today(today).render_png(&Template::voucher(), &voucher, 192.0)?;
let media_id = client.media(pnid).upload(png.bytes, png.mime_type, &png.filename).await?;
let t = TemplateMessage::new("autumn_sale", "en_US").header(Parameter::image_id(media_id));
```

150–200 ppi suits image headers. `render_png` renders the first page;
`render_png_pages` every page. `ppi` must be finite and positive
(`RenderError::InvalidPpi`); a page over `MAX_PNG_PIXELS` (40 M) is refused
(`ImageTooLarge`) before allocating. For a template's **creation** example the
header needs a Resumable Upload handle, not a media id (`wa-rs-templates-otp`).

## Custom templates and the sandbox

```rust
let t = Template::from_source("packing_slip", r#"#let data = json(bytes(sys.inputs.data))
#set page(width: 105mm, height: 148mm)
= Packing slip #data.order
"#);
let pdf = Renderer::new().with_today(today).render_pdf(&t, &serde_json::json!({ "order": "860198" }))?;
```

- Input: any `Serialize` value, read in Typst with
  `json(bytes(sys.inputs.data))`. Values placed with `#data.field` are text,
  not markup (an input of `*x* #panic()` prints literally) — unless the
  template passes input to `eval`: don't.
- **No file system, no network, no packages**: `@preview` imports and file
  reads fail with `RenderError::Compile` (diagnostics carry line/column).
  Everything shown must come from the input or the source.
- **Fonts**: only the bundled families — `"Libertinus Serif"` (default),
  `"New Computer Modern"`, `"New Computer Modern Math"`, `"DejaVu Sans Mono"`.
  Any other family silently falls back.
- `name` becomes the output filename stem and appears in errors.
- Compile diagnostics can quote input values: log them with the same care as
  the input.

## Never render OTPs or secrets

Authentication codes, PINs, passwords, access tokens and single-use bearer
secrets (a gift-card secret that is spendable on its own) must never be
rendered into a PDF or image: media is stored by Meta and on the phone,
forwarded and screenshotted, and cannot be taken back. OTPs go only through
authentication templates (`wa-rs-templates-otp`). A voucher's `code` is a
promotional code meant to be shared — nothing more.

## Not provided

Tax/total computation, number and date formatting, localization of labels,
e-invoicing formats (Factur-X, UBL), PDF signing, storage of the rendered
file (keep your own copy if you need it after 30 days).
