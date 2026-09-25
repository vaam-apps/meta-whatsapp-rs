//! Reference code for the `meta-whatsapp-rs-documents` skill: a receipt rendered to
//! PDF with Typst (feature `typst`), uploaded and sent as a document; a
//! voucher rendered to PNG for a template header; a custom template.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::messages::{Document, OutboundMessage};
use meta_whatsapp_rs::core::ids::MediaId;
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::typst::{
    LineItem, ReceiptInput, RenderError, RenderedDocument, Renderer, Template, VoucherInput,
};

/// Render (off the async workers), upload, send.
pub async fn send_receipt(
    client: &Client,
    phone_number_id: PhoneNumberId,
    to: Recipient,
    receipt: ReceiptInput,
    today: time::Date, // in the shop's or customer's time zone
) -> anyhow::Result<SendResponse> {
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
    let sent = client
        .messages(phone_number_id)
        .send(&OutboundMessage::new(to, document)) // free-form: inside the 24-hour window
        .await?;
    Ok(sent)
}

/// Outside the window: the same PDF as a utility template's document header.
pub fn receipt_template(media_id: MediaId, filename: String) -> TemplateMessage {
    TemplateMessage::new("order_receipt", "en_US")
        .header(Parameter::document_id(media_id, Some(filename)))
}

/// A voucher is an image: PNG at 150–200 ppi for an image header.
pub fn voucher_png(
    voucher: &VoucherInput,
    today: time::Date,
) -> Result<RenderedDocument, RenderError> {
    Renderer::new()
        .with_today(today)
        .render_png(&Template::voucher(), voucher, 192.0)
}

/// Your own template: input is any `Serialize` value, read with
/// `json(bytes(sys.inputs.data))`; no files, no packages, bundled fonts only.
pub fn packing_slip(order: &str, today: time::Date) -> Result<RenderedDocument, RenderError> {
    let template = Template::from_source(
        "packing_slip",
        "#let data = json(bytes(sys.inputs.data))\n#set page(width: 105mm, height: 148mm)\n= Packing slip #data.order\n",
    );
    Renderer::new()
        .with_today(today)
        .render_pdf(&template, &serde_json::json!({ "order": order }))
}

/// Money and dates arrive formatted: the template prints, it never computes.
pub fn sample_receipt() -> ReceiptInput {
    ReceiptInput {
        merchant_name: "Example Boutique".into(),
        order_number: "860198".into(),
        order_date: "24 Sep 2026".into(),
        customer_name: "Alex Doe".into(),
        currency: "EUR".into(),
        items: vec![LineItem {
            description: "Linen shirt, navy, size M".into(),
            quantity: 2,
            unit_price: "49.90".into(),
            amount: "99.80".into(),
        }],
        subtotal: "99.80".into(),
        adjustments: vec![],
        total: "99.80".into(),
        payment_method: "Card".into(),
        delivery_address: vec!["1 Example Street".into(), "75002 Paris".into()],
        estimated_delivery: Some("26 Sep 2026".into()),
        support_contact: None,
    }
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use serde_json::json;
    use time::macros::date;

    use super::*;

    #[tokio::test]
    async fn render_upload_send() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"id": "1037543291543636"}));
        transport.push_json(200, json!({"messages": [{"id": "wamid.DOC"}]}));
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .build()
            .unwrap();
        send_receipt(
            &client,
            "106540352242922".into(),
            Recipient::phone("+16505551234"),
            sample_receipt(),
            date!(2026 - 09 - 24),
        )
        .await
        .unwrap();
        let requests = transport.requests();
        let (filename, content_type, pdf) = requests[0].multipart_field("file").unwrap();
        assert_eq!(filename, Some("receipt-860198.pdf"));
        assert_eq!(content_type, Some("application/pdf"));
        assert!(pdf.starts_with(b"%PDF"));
        assert_eq!(
            requests[1].json().unwrap()["document"]["filename"],
            "receipt-860198.pdf"
        );
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn no_date_no_today() {
        let today = Template::from_source("t", "#datetime.today().display()");
        assert!(Renderer::new().render_pdf(&today, &json!({})).is_err());
        assert!(packing_slip("860198", date!(2026 - 09 - 24)).is_ok());
    }
}
