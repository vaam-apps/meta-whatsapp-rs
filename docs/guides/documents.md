# Documents: invoices, receipts and vouchers

**Goal:** turn an order into a PDF invoice or receipt, or a voucher into an
image, and send it over WhatsApp as a document, an image, or a template
header.

Example: [`invoice_document.rs`](../../crates/meta-whatsapp-rs/examples/invoice_document.rs)
(`cargo run -p meta-whatsapp-rs --example invoice_document --features typst`). Agent
skill: [`meta-whatsapp-rs-documents`](../../skills/meta-whatsapp-rs-documents/SKILL.md).

```text
order record ─► InvoiceInput (pre-formatted strings) ─► Renderer (Typst, sandboxed) ─► PDF bytes
             ─► media(pnid).upload ─► media id (30 days) ─► document message, or template header
```

## 1. On Meta's side

- A **document message** is free-form: it reaches the customer only inside
  the 24-hour customer service window. Outside it, send a **utility
  template with a document header** (create it once, section 4).
- A voucher goes out as the **image header** of a marketing template.
- Limits: documents up to 100 MB; images JPEG or PNG up to 5 MB. Uploaded
  media ids live 30 days.

Meta's pages:
[business-phone-numbers/media](https://developers.facebook.com/documentation/business-messaging/whatsapp/business-phone-numbers/media),
[messages/document-messages](https://developers.facebook.com/documentation/business-messaging/whatsapp/messages/document-messages),
[templates/template-media](https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/template-media).

## 2. Render

Enable the `typst` feature. Rendering is CPU-bound and synchronous (tens to
hundreds of milliseconds): keep it off the async workers.

```rust
use meta_whatsapp_rs::client::messages::Document;
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::typst::{InvoiceInput, Renderer, Template};

async fn send_invoice(client: &Client, pnid: PhoneNumberId, to: Recipient, invoice: InvoiceInput,
                      today: time::Date) -> anyhow::Result<MessageId> {
    let number = invoice.invoice_number.clone();
    let pdf = tokio::task::spawn_blocking(move || {
        Renderer::new().with_today(today).render_pdf(&Template::invoice(), &invoice)
    })
    .await??; // the task, then the RenderError

    let filename = format!("{number}.pdf");
    let media_id = client.media(pnid.clone()).upload(pdf.bytes, pdf.mime_type, &filename).await?;
    let document = Document::new(media_id).filename(filename).caption(format!("Invoice {number}"));
    let sent = client.messages(pnid).send(&OutboundMessage::new(to, document)).await?;
    sent.message_id().cloned().ok_or_else(|| anyhow::anyhow!("accepted without a message id"))
}
```

- The output is a `RenderedDocument { bytes, mime_type, filename }`; the
  default filename is the template's name (`invoice.pdf`): name it for the
  customer with `Document::filename`.
- `upload` checks the MIME type and size before sending anything.
- In a function returning `meta_whatsapp_rs::Result`, `?` turns a `RenderError` into
  `Error::Other`; `downcast_ref::<RenderError>()` on the inner error gets
  it back.

## 3. The built-in templates

| Template | Input | Output |
| --- | --- | --- |
| `Template::invoice()` | `InvoiceInput`: number, dates, `seller`/`buyer` (`Party`), `currency`, `items` (`LineItem`), `subtotal`, `adjustments` (`SummaryLine`), `total`, terms, notes | A4 PDF |
| `Template::receipt()` | `ReceiptInput`: merchant, order number and date, customer, items, totals, `payment_method`, `delivery_address`, estimated delivery, support contact | A5 PDF |
| `Template::voucher()` | `VoucherInput`: merchant, `headline`, `description`, `code`, `valid_until`, `terms`, `accent_color` (`#RRGGBB`, dark: the text is white) | image header, render to PNG |

**Money and dates are strings you already formatted** (`"1,234.50"`,
`"24 Sep 2026"`). The templates print, never compute, so a PDF can never
disagree with your order record through rounding, and locale formatting
stays with you. `Option` fields and empty lists are left out of the layout.
Sample inputs live in `crates/wa-typst/tests/fixtures/`.

## 4. Outside the window: a document header

Create a utility template with a document header once (its example is a
Resumable Upload handle of a sample PDF), then send the rendered invoice's
media id as the header parameter:

```rust
use meta_whatsapp_rs::client::templates::{TemplateCategory, TemplateComponent, TemplateDefinition};

let handle = client.media(pnid.clone()).resumable_upload(&app_id, "sample.pdf", "application/pdf", sample_pdf).await?;
client.templates(waba_id).create(&TemplateDefinition::new("invoice_ready", "en_US", TemplateCategory::Utility)
    .component(TemplateComponent::header_document(handle.as_str()))
    .component(TemplateComponent::body_positional("Your invoice {{1}} is attached.", ["INV-2026-0042"])))
    .await?;

// at send time
let invoice_ready = TemplateMessage::new("invoice_ready", "en_US")
    .header(Parameter::document_id(media_id, Some("INV-2026-0042.pdf".to_owned())))
    .body([Parameter::text("INV-2026-0042")]);
client.messages(pnid).send(&OutboundMessage::template(to, invoice_ready)).await?;
```

## 5. Vouchers as image headers

```rust
use meta_whatsapp_rs::typst::VoucherInput;

let png = Renderer::new().with_today(today).render_png(&Template::voucher(), &voucher, 192.0)?;
let media_id = client.media(pnid.clone()).upload(png.bytes, png.mime_type, &png.filename).await?;
let sale = TemplateMessage::new("autumn_sale", "en_US").header(Parameter::image_id(media_id));
```

150–200 ppi suits image headers. `render_png` renders the first page,
`render_png_pages` every page. `ppi` must be finite and positive, and a
page over `MAX_PNG_PIXELS` (40 million) is refused before anything is
allocated.

## 6. Your own templates

```rust
let slip = Template::from_source("packing_slip", r#"#let data = json(bytes(sys.inputs.data))
#set page(width: 105mm, height: 148mm)
= Packing slip #data.order
"#);
let pdf = Renderer::new().with_today(today).render_pdf(&slip, &serde_json::json!({ "order": "860198" }))?;
```

- The input is any `Serialize` value, read in Typst with
  `json(bytes(sys.inputs.data))`. Values placed with `#data.field` are text,
  not markup: an input of `*x* #panic()` prints as written. Never pass input
  to `eval`.
- Fonts: only the bundled families (`"Libertinus Serif"`, the default,
  `"New Computer Modern"`, `"New Computer Modern Math"`,
  `"DejaVu Sans Mono"`); any other family silently falls back.
- The template's name becomes the output filename and appears in errors.
  Compile diagnostics can quote input values: log them as carefully as the
  input.

## 7. Determinism and the sandbox

- `Renderer::new()` has **no clock**. A template calling `datetime.today()`
  fails, and the PDF carries no creation date, until you set
  `with_today(date)`. There is deliberately no fallback to the system clock
  ([open question](../../OPEN_QUESTIONS.md#product-details) 25): pass the
  date in the shop's or customer's time zone.
- Same template, input and date: byte-identical output on every machine
  (bundled fonts only). Store the input, not only the PDF, and you can
  reproduce any document.
- No file system, no network, no packages: `@preview` imports and file
  reads fail with `RenderError::Compile`. Everything shown comes from the
  input or the source.

## Never render codes or secrets

One-time passcodes, PINs, passwords, access tokens and single-use bearer
values (a gift card secret spendable on its own) must never go into a PDF or
an image: media is stored by Meta and on the phone, forwarded and
screenshotted, and cannot be taken back. OTPs go only through
authentication templates ([otp-login.md](otp-login.md)). A voucher's `code`
is a promotional code meant to be shared.

## Pitfalls

- Rendering on the async runtime: it blocks a worker for the whole render.
- Forgetting `with_today`: a template that prints today's date fails.
- Sending a document message outside the window: use the template header.
- Relying on Meta to keep the file: media ids expire after 30 days; keep
  your own copy.

## Not provided

Tax and total computation, number and date formatting, label translation,
e-invoicing formats (Factur-X, UBL), PDF signing, and storage of the
rendered files.
