//! [`Template`]: a Typst source plus the name its output files are called by.

use std::borrow::Cow;

/// A Typst source to render.
///
/// The built-in templates are compiled into the binary; each documents the
/// input type it reads. A template reads its input with
/// `json(bytes(sys.inputs.data))`. It cannot import packages or read files:
/// everything it shows must come from the input or the source itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    name: Cow<'static, str>,
    source: Cow<'static, str>,
}

impl Template {
    /// Invoice (A4). Input: [`InvoiceInput`](crate::InvoiceInput).
    pub fn invoice() -> Self {
        Self::builtin("invoice", include_str!("../templates/invoice.typ"))
    }

    /// Order confirmation / receipt (A5). Input:
    /// [`ReceiptInput`](crate::ReceiptInput).
    pub fn receipt() -> Self {
        Self::builtin("receipt", include_str!("../templates/receipt.typ"))
    }

    /// Coupon or gift voucher, sized for an image header (400 × 210 pt,
    /// ≈ 1.91:1). Render it to PNG. Input: [`VoucherInput`](crate::VoucherInput).
    pub fn voucher() -> Self {
        Self::builtin("voucher", include_str!("../templates/voucher.typ"))
    }

    /// A template from your own Typst source.
    ///
    /// `name` becomes the stem of the output filename (`{name}.pdf`,
    /// `{name}.png`, `{name}-{page}.png`) and appears in errors; it is used
    /// verbatim, so keep it to something a WhatsApp user may see as a file
    /// name. The source is not checked here: errors surface as
    /// [`RenderError::Compile`](crate::RenderError::Compile) at render time.
    pub fn from_source(name: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            name: Cow::Owned(name.into()),
            source: Cow::Owned(source.into()),
        }
    }

    const fn builtin(name: &'static str, source: &'static str) -> Self {
        Self {
            name: Cow::Borrowed(name),
            source: Cow::Borrowed(source),
        }
    }

    /// The template's name: `"invoice"`, `"receipt"`, `"voucher"`, or the
    /// name given to [`Template::from_source`].
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The Typst source.
    pub fn source(&self) -> &str {
        &self.source
    }
}
