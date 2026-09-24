//! Render an invoice PDF with Typst, upload it, and send it as a document
//! message.
//!
//! The built-in `invoice` template takes an [`InvoiceInput`] whose amounts
//! and dates are strings you already formatted: the template prints, it
//! never computes, so the PDF cannot disagree with your order record.
//! Rendering is CPU-bound and synchronous, hence `spawn_blocking`.
//!
//! A document message is free-form: it is only delivered inside the 24-hour
//! customer service window. Outside it, send an approved utility template
//! with a document header instead (shown in a comment below).
//!
//! | Variable | Required | What |
//! | --- | --- | --- |
//! | `WA_TOKEN` | yes | a system user access token with `whatsapp_business_messaging` |
//! | `WA_PHONE_NUMBER_ID` | yes | the business phone number id that uploads and sends |
//! | `WA_TO` | yes | the recipient, E.164 with `+`, e.g. `+16505551234` |
//!
//! ```text
//! WA_TOKEN=… WA_PHONE_NUMBER_ID=… WA_TO=+16505551234 \
//!   cargo run -p wa-rs --example invoice_document --features typst
//! ```

use anyhow::Context as _;
use time::OffsetDateTime;
use wa_rs::client::messages::Document;
use wa_rs::prelude::*;
use wa_rs::typst::{InvoiceInput, LineItem, Party, Renderer, SummaryLine, Template};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // RUST_LOG=info (or debug) shows what the library logs.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let client = wa_rs::client(env("WA_TOKEN")?)?;
    let phone_number_id = PhoneNumberId::new(env("WA_PHONE_NUMBER_ID")?);
    let to = Recipient::phone(env("WA_TO")?);

    let invoice = sample_invoice();
    let filename = format!("{}.pdf", invoice.invoice_number);
    let caption = format!("Invoice {}", invoice.invoice_number);
    let pdf = tokio::task::spawn_blocking(move || {
        Renderer::new()
            .with_today(OffsetDateTime::now_utc().date()) // the PDF's creation date
            .render_pdf(&Template::invoice(), &invoice)
    })
    .await??;
    println!("rendered {} bytes", pdf.bytes.len());

    // Type and size are checked before anything is sent. The id lives 30 days.
    let media_id = client
        .media(phone_number_id.clone())
        .upload(pdf.bytes, pdf.mime_type, &filename)
        .await?;
    let document = Document::new(media_id).filename(filename).caption(caption);
    let sent = client
        .messages(phone_number_id)
        .send(&OutboundMessage::new(to, document))
        .await?;
    // Outside the service window, a utility template with a document header:
    // OutboundMessage::template(to, TemplateMessage::new("invoice_ready", "en_US")
    //     .header(Parameter::document_id(media_id, Some(filename))))
    println!("document accepted as {:?}", sent.message_id());
    Ok(())
}

/// What your order system would produce. Amounts are formatted strings.
fn sample_invoice() -> InvoiceInput {
    InvoiceInput {
        invoice_number: "INV-2026-0042".to_owned(),
        issue_date: "24 September 2026".to_owned(),
        due_date: Some("8 October 2026".to_owned()),
        seller: Party {
            name: "Example Boutique GmbH".to_owned(),
            address: vec!["Musterstraße 1".to_owned(), "10115 Berlin".to_owned()],
            tax_id: Some("DE000000000".to_owned()),
            email: Some("billing@shop.example".to_owned()),
            phone: None,
        },
        buyer: Party {
            name: "Alex Doe".to_owned(),
            address: vec!["1 Example Street".to_owned(), "75002 Paris".to_owned()],
            tax_id: None,
            email: Some("alex@example.com".to_owned()),
            phone: None,
        },
        currency: "EUR".to_owned(),
        items: vec![
            LineItem {
                description: "Linen shirt, navy, size M".to_owned(),
                quantity: 2,
                unit_price: "49.90".to_owned(),
                amount: "99.80".to_owned(),
            },
            LineItem {
                description: "Leather belt, brown".to_owned(),
                quantity: 1,
                unit_price: "35.00".to_owned(),
                amount: "35.00".to_owned(),
            },
        ],
        subtotal: "134.80".to_owned(),
        adjustments: vec![SummaryLine {
            label: "VAT 19% (included)".to_owned(),
            amount: "21.52".to_owned(),
        }],
        total: "134.80".to_owned(),
        payment_terms: Some("Paid by card.".to_owned()),
        notes: Some("Thank you for your order.".to_owned()),
    }
}

fn env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("set {name} (see the example's header)"))
}
