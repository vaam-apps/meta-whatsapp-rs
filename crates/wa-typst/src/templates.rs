//! Built-in templates and template management.

use serde::{Deserialize, Serialize};

/// A Typst template for rendering.
#[derive(Clone)]
pub enum Template {
    /// Built-in invoice template.
    Invoice,
    /// Built-in receipt/order confirmation template.
    Receipt,
    /// Built-in voucher/coupon template.
    Voucher,
    /// Custom template from source.
    Custom {
        /// Template name (used for file id).
        name: String,
        /// Typst source code.
        source: String,
    },
}

impl Template {
    /// Get a built-in invoice template.
    pub fn invoice() -> Self {
        Self::Invoice
    }

    /// Get a built-in receipt template.
    pub fn receipt() -> Self {
        Self::Receipt
    }

    /// Get a built-in voucher template.
    pub fn voucher() -> Self {
        Self::Voucher
    }

    /// Create a custom template from source.
    pub fn from_source(name: impl Into<String>, source: impl Into<String>) -> Self {
        Self::Custom {
            name: name.into(),
            source: source.into(),
        }
    }

    /// Get the template name for identification.
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Invoice => "invoice",
            Self::Receipt => "receipt",
            Self::Voucher => "voucher",
            Self::Custom { name, .. } => name,
        }
    }

    /// Get the template source code.
    pub(crate) fn source(&self) -> &str {
        match self {
            Self::Invoice => include_str!("../templates/invoice.typ"),
            Self::Receipt => include_str!("../templates/receipt.typ"),
            Self::Voucher => include_str!("../templates/voucher.typ"),
            Self::Custom { source, .. } => source,
        }
    }
}

/// Input data for invoice rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub struct InvoiceInput {
    /// Seller/business name.
    pub seller_name: String,
    /// Seller contact information.
    pub seller_contact: Option<String>,
    /// Invoice number/id.
    pub invoice_number: String,
    /// Invoice date (ISO 8601 format).
    pub invoice_date: String,
    /// Buyer/customer name.
    pub buyer_name: String,
    /// Buyer contact information.
    pub buyer_contact: Option<String>,
    /// Line items.
    pub items: Vec<InvoiceLineItem>,
    /// Currency code (e.g., "USD", "EUR").
    pub currency: String,
    /// Subtotal as a string (e.g., "100.00").
    pub subtotal: String,
    /// Tax amount as a string (e.g., "10.00").
    pub tax: String,
    /// Total amount as a string (e.g., "110.00").
    pub total: String,
    /// Additional notes or terms.
    pub notes: Option<String>,
}

/// A line item in an invoice.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub struct InvoiceLineItem {
    /// Product or service description.
    pub description: String,
    /// Quantity.
    pub quantity: String,
    /// Unit price as a string.
    pub unit_price: String,
    /// Total for this line as a string.
    pub total: String,
}

/// Input data for receipt/order confirmation rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub struct ReceiptInput {
    /// Order/receipt id.
    pub order_id: String,
    /// Order date (ISO 8601 format).
    pub order_date: String,
    /// Order items.
    pub items: Vec<ReceiptLineItem>,
    /// Subtotal as a string.
    pub subtotal: String,
    /// Shipping cost as a string.
    pub shipping: String,
    /// Tax amount as a string.
    pub tax: String,
    /// Total amount as a string.
    pub total: String,
    /// Currency code.
    pub currency: String,
    /// Payment method (e.g., "Credit Card", `"PayPal"`).
    pub payment_method: String,
    /// Delivery address.
    pub delivery_address: String,
    /// Estimated delivery (e.g., "2-3 business days").
    pub delivery_eta: Option<String>,
}

/// A line item in a receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub struct ReceiptLineItem {
    /// Product name.
    pub name: String,
    /// Quantity.
    pub quantity: String,
    /// Unit price as a string.
    pub unit_price: String,
    /// Total for this line as a string.
    pub total: String,
}

/// Input data for voucher/coupon rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub struct VoucherInput {
    /// Voucher title (e.g., "Summer Sale").
    pub title: String,
    /// Discount text (e.g., "50% OFF").
    pub discount_text: String,
    /// Voucher code.
    pub code: String,
    /// Expiry date (ISO 8601 format).
    pub expiry_date: String,
    /// Brand color (hex format, e.g., "#FF6B6B").
    pub brand_color: String,
    /// Optional additional description.
    pub description: Option<String>,
}
