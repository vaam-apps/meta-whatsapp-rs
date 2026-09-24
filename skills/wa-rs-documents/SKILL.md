---
name: wa-rs-documents
description: "Invoices, receipts and vouchers for WhatsApp with wa-rs's Typst renderer (feature typst) - the built-in templates and their typed inputs (pre-formatted money and dates), Renderer::with_today for deterministic output, rendering off the async runtime, PDF for document messages and template document headers, PNG for image headers, custom Typst templates and the sandbox (bundled fonts, no files, no packages), then upload and send. Never for OTP codes or secrets. Load when generating any PDF or image to send over WhatsApp."
---

# wa-rs-documents

> **Verified against wa-rs 3a3db05aa425c1737d8bb9239206036dbc81969f (2026-09-24).** On another revision, trust the code over this page.

Reference code: [examples/documents.rs](examples/documents.rs), compiled
and tested by wa-rs's own gate (it renders a real receipt PDF). Runnable
program: [`invoice_document.rs`](https://github.com/vaam-apps/wa-rs/blob/main/crates/wa-rs/examples/invoice_document.rs).

## When to use

An order receipt, an invoice, a voucher image, a packing slip — rendered
by your backend and sent over WhatsApp. Enable the `typst` feature; the
module is `wa_rs::typst`.

## Render, upload, send

```rust
let filename = format!("receipt-{}.pdf", receipt.order_number);
// CPU-bound and synchronous (tens to hundreds of ms): keep it off the runtime.
let pdf = tokio::task::spawn_blocking(move || {
    Renderer::new()
        .with_today(today)
        .render_pdf(&Template::receipt(), &receipt)
})
.await??; // JoinError, then RenderError
let media_id = client
    .media(phone_number_id.clone())
    .upload(pdf.bytes, pdf.mime_type, &filename)
    .await?; // PDF up to 100 MB; the id lives 30 days
let document = Document::new(media_id)
    .filename(filename)
    .caption("Thanks for your order!");
```

A document message is free-form: inside the 24-hour window only. Outside
it, send an approved utility template with a document header:

```rust
TemplateMessage::new("order_receipt", "en_US")
    .header(Parameter::document_id(media_id, Some(filename)))
```

## Built-in templates

| Template | Input | Output |
| --- | --- | --- |
| `Template::invoice()` | `InvoiceInput` (number, dates, `seller`/`buyer: Party`, currency, `items: Vec<LineItem>`, subtotal, `adjustments: Vec<SummaryLine>`, total, terms, notes) | A4 PDF |
| `Template::receipt()` | `ReceiptInput` (merchant, order number and date, customer, items, totals, payment method, delivery address, estimated delivery, support contact) | A5 PDF |
| `Template::voucher()` | `VoucherInput` (merchant, headline, description, `code`, validity, terms, `accent_color` `#RRGGBB`) | 400×210 pt, render as PNG |

**Money and dates are pre-formatted strings** (`"1,234.50"`,
`"24 Sep 2026"`) from your order system: the template prints, it never
computes, so a total can never disagree with the order record. `Option`
fields and empty lists are left out of the layout.

## Images for headers

```rust
Renderer::new()
    .with_today(today)
    .render_png(&Template::voucher(), voucher, 192.0)
```

150–200 ppi suits image headers (images up to 5 MB). `render_png` renders
the first page, `render_png_pages` every page. A bad `ppi` is
`RenderError::InvalidPpi`; a page over `MAX_PNG_PIXELS` is
`RenderError::ImageTooLarge`, refused before allocating. Send it with
`Parameter::image_id(media_id)` in a template header; a template's
**creation** example needs a Resumable Upload handle instead (`wa-rs-media`).

## Your own template

```rust
let template = Template::from_source(
    "packing_slip",
    "#let data = json(bytes(sys.inputs.data))\n#set page(width: 105mm, height: 148mm)\n= Packing slip #data.order\n",
);
Renderer::new()
    .with_today(today)
    .render_pdf(&template, &serde_json::json!({ "order": order }))
```

Input is any `Serialize` value, read in Typst with
`json(bytes(sys.inputs.data))`; values placed with `#data.field` are
text, not markup — unless the template passes them to `eval`: don't. The
sandbox has no file system, network or packages (`@preview` imports fail
with `RenderError::Compile`, whose diagnostics carry line and column) and
only the bundled fonts: Libertinus Serif (default), New Computer Modern,
New Computer Modern Math, DejaVu Sans Mono; any other family silently
falls back.

## Pitfalls

- **Always `with_today(date)`**: `Renderer::new()` has no clock, so a
  template calling `datetime.today()` fails to compile, and the PDF carries
  no creation date. Pass the date in the shop's or customer's time zone.
  Same template, input and date ⇒ byte-identical output everywhere.
- `RenderError` converts into `wa_rs::Error::Other` with `?`; the typed
  error survives (`downcast_ref::<RenderError>()` on the inner
  `anyhow::Error`).
- Compile diagnostics can quote input values: log them as carefully as
  the input.
- **Never render OTP codes, PINs, passwords or tokens** into a document or
  image: media is stored by Meta and on the phone, forwarded and
  screenshotted. A voucher's `code` is a promotional code meant to be
  shared, nothing more.

## What wa-rs does not do

- No tax or total computation, number or date formatting, label
  localization, e-invoicing formats (Factur-X, UBL) or PDF signing.
- No storage of rendered files: keep your own copy if you need it after
  the media id's 30 days.

## Related skills

`wa-rs-media`, `wa-rs-send-messages`, `wa-rs-send-templates`,
`wa-rs-commerce` (the orders), `wa-rs-marketing` (voucher campaigns),
`wa-rs-otp-login` (codes go there, never here).
