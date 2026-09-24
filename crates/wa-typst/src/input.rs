//! Typed inputs for the built-in templates.
//!
//! Amounts are **pre-formatted strings** (`"1,234.50"`, `"12,50"`), never
//! floats and never computed by the template. The template prints exactly what
//! the caller computed, so a total can never disagree with the order record
//! because of float rounding or a template doing arithmetic, and locale and
//! currency-exponent formatting stay with the caller, who knows both. Dates
//! are pre-formatted strings for the same reason.
//!
//! Every field is printed by its template (a test mutates each one and
//! checks the rendered image changes). `Option` fields and empty lists are
//! left out of the layout, not printed as blanks.
//!
//! Never put OTP codes, PINs, passwords or access tokens in any of these:
//! rendered media is stored and forwarded outside your control.

use serde::{Deserialize, Serialize};

/// A business or person on a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Party {
    /// Legal or display name.
    pub name: String,
    /// Postal address, one line per entry. May be empty.
    pub address: Vec<String>,
    /// VAT / tax registration number.
    pub tax_id: Option<String>,
    /// Email address.
    pub email: Option<String>,
    /// Phone number, as it should be printed.
    pub phone: Option<String>,
}

/// One product or service line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineItem {
    /// What was sold.
    pub description: String,
    /// How many.
    pub quantity: u32,
    /// Price of one unit, formatted.
    pub unit_price: String,
    /// `quantity × unit_price` after any line discount, formatted.
    pub amount: String,
}

/// A labelled amount between subtotal and total: a tax, a discount, shipping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryLine {
    /// E.g. `"VAT 19%"`, `"Discount SUMMER10"`, `"Shipping"`.
    pub label: String,
    /// Formatted amount, with its sign if it reduces the total (`"-5.00"`).
    pub amount: String,
}

/// Input of [`Template::invoice`](crate::Template::invoice).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvoiceInput {
    /// Invoice number, printed in the title and the PDF metadata.
    pub invoice_number: String,
    /// Issue date, formatted.
    pub issue_date: String,
    /// Payment due date, formatted.
    pub due_date: Option<String>,
    /// Who issues the invoice.
    pub seller: Party,
    /// Who is billed.
    pub buyer: Party,
    /// Currency code or symbol printed with every amount column header and the
    /// total, e.g. `"EUR"`.
    pub currency: String,
    /// Invoice lines.
    pub items: Vec<LineItem>,
    /// Sum of the line amounts, formatted.
    pub subtotal: String,
    /// Taxes, discounts, fees between subtotal and total, in print order.
    pub adjustments: Vec<SummaryLine>,
    /// Amount due, formatted.
    pub total: String,
    /// Payment terms or instructions (bank details, payment link text).
    pub payment_terms: Option<String>,
    /// Free text printed at the bottom.
    pub notes: Option<String>,
}

/// Input of [`Template::receipt`](crate::Template::receipt): an order
/// confirmation or payment receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptInput {
    /// Shop name printed in the header.
    pub merchant_name: String,
    /// Order number.
    pub order_number: String,
    /// Order date, formatted.
    pub order_date: String,
    /// Who ordered.
    pub customer_name: String,
    /// Currency code or symbol, e.g. `"EUR"`.
    pub currency: String,
    /// Ordered items.
    pub items: Vec<LineItem>,
    /// Sum of the line amounts, formatted.
    pub subtotal: String,
    /// Shipping, taxes, discounts between subtotal and total, in print order.
    pub adjustments: Vec<SummaryLine>,
    /// Amount paid, formatted.
    pub total: String,
    /// How it was paid, e.g. `"Visa •••• 4242"`. Never a full card number.
    pub payment_method: String,
    /// Delivery address, one line per entry. Empty for pickup or digital goods.
    pub delivery_address: Vec<String>,
    /// Estimated delivery, formatted, e.g. `"Thu 1 Oct – Sat 3 Oct"`.
    pub estimated_delivery: Option<String>,
    /// Where to get help, e.g. an email address or "reply to this chat".
    pub support_contact: Option<String>,
}

/// Input of [`Template::voucher`](crate::Template::voucher): a coupon or gift
/// voucher image for a marketing template header.
///
/// `code` is a promotional code meant to be shown and shared. A bearer value
/// that grants access or money on its own (a single-use gift card secret, a
/// login code) does not belong in an image that is forwarded and stored
/// outside your control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoucherInput {
    /// Shop name.
    pub merchant_name: String,
    /// The offer in a few words, e.g. `"20% OFF"`.
    pub headline: String,
    /// One line of detail, e.g. `"on your next order over €50"`.
    pub description: String,
    /// Code to enter at checkout.
    pub code: String,
    /// Expiry, formatted, e.g. `"31 Oct 2026"`.
    pub valid_until: Option<String>,
    /// Fine print.
    pub terms: Option<String>,
    /// Background colour as `#RRGGBB`; the text is white, so pick a dark one.
    /// Anything typst's `rgb()` does not accept is a
    /// [`RenderError::Compile`](crate::RenderError::Compile).
    pub accent_color: String,
}
