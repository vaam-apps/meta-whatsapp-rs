//! QR codes and short links (docs/design/server.md, section 4.2; roadmap
//! M5g), as services an API adapter calls, with the tenant's WABA token,
//! over the library's `client::qr_codes`:
//!
//! | Operation | Graph calls |
//! | --- | --- |
//! | [`QrCodes::create`] | `POST /{pn}/message_qrdls` |
//! | [`QrCodes::list`] | `GET /{pn}/message_qrdls?fields=…&limit=…[&after=…]` |
//! | [`QrCodes::get`] | `GET /{pn}/message_qrdls?fields=…&code=…&limit=25` |
//! | [`QrCodes::update`] | the same lookup, then `POST /{pn}/message_qrdls` (with `code`) |
//! | [`QrCodes::delete`] | the same lookup, then `DELETE /{pn}/message_qrdls/{code}` |
//!
//! Pages: `qr-codes`,
//! `reference/whatsapp-business-phone-number/whatsapp-business-qr-code-management-api`,
//! `…/whatsapp-business-qr-code-api`. Limits, checked before any request:
//! a prefilled message is 1 to 140 characters, a code 14 ASCII letters or
//! digits, a page 1 to 25 codes (the 2,000-codes-per-number cap is Meta's
//! count).
//!
//! **A QR code is its number's, or it does not exist** (the design's
//! M5.2; D144). The services open only on a number of the [`Caller`]'s own
//! tenant (step 4 of the authorization order, [`crate::authz`]), but a
//! code is Meta's id, named under the number's path, and Meta's pages do
//! not say that reading, changing or deleting `/{pn}/message_qrdls/{code}`
//! checks the code against the number. A code is therefore looked for in
//! the **number's own list** first (the list's `code` filter, which the
//! reference documents: "only return this code (if it exists)"), and
//! acted on only when that list holds exactly that code; another number's
//! code, or none, is `404 not_found`, without Meta's code or text, and
//! nothing is read back, changed or deleted. The lookup is one Graph read,
//! and a read of one code costs no more (the list holds what a read
//! would): a refusal of another number's code is made by the number's own
//! list, not without a request to Meta, which only another tenant's
//! number is (step 4 never reaches Meta).
//!
//! A creation's and a change's failures answer and log no text of Meta's
//! (`details` included): it may quote the prefilled message, and a message's
//! text is in no log.
//!
//! A creation is the caller's `Idempotency-Key` to repeat; a deletion is
//! the caller's audit event ([`crate::audit::tenant_audit`]:
//! `qr_code_deleted`).

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::qr_codes::{
    CreateQrCode, ListQrCodes, QR_CODE_ID_LEN, QrCode, QrCodeFields, QrImageFormat, UpdateQrCode,
};
use meta_whatsapp_rs::core::ids::QrCodeId;

pub use meta_whatsapp_rs::client::qr_codes::{LIST_LIMIT_MAX, PREFILLED_MESSAGE_MAX_CHARS};

use crate::audit::{Subject, tenant_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::error::ServiceError;
use crate::observe::Observer;

/// The audit action of a deletion (docs/design/server.md, D58).
const DELETED: &str = "qr_code_deleted";

// ─── The request's checks ────────────────────────────────────────────────

/// A prefilled message: 1 to [`PREFILLED_MESSAGE_MAX_CHARS`] characters.
fn message(text: &str) -> Result<(), ServiceError> {
    let len = text.chars().count();
    if (1..=PREFILLED_MESSAGE_MAX_CHARS).contains(&len) {
        Ok(())
    } else {
        Err(ServiceError::invalid("prefilled_message"))
    }
}

/// An image format: `PNG` or `SVG`, as Meta writes them.
///
/// # Errors
///
/// `422 invalid_request` on `field`.
pub fn image_format(field: &'static str, format: &str) -> Result<QrImageFormat, ServiceError> {
    match format {
        "PNG" => Ok(QrImageFormat::Png),
        "SVG" => Ok(QrImageFormat::Svg),
        _ => Err(ServiceError::invalid(field)),
    }
}

/// A code from a path or a query: [`QR_CODE_ID_LEN`] ASCII letters or
/// digits, else `422 invalid_request` on `field`.
///
/// # Errors
///
/// As above.
pub fn code_id(field: &'static str, code: &str) -> Result<QrCodeId, ServiceError> {
    if code.len() == QR_CODE_ID_LEN && code.bytes().all(|b| b.is_ascii_alphanumeric()) {
        Ok(QrCodeId::new(code))
    } else {
        Err(ServiceError::invalid(field))
    }
}

/// The library's creation for `prefilled_message` and an optional image
/// format (`PNG` or `SVG`), checked.
///
/// # Errors
///
/// `422 invalid_request` on `prefilled_message` or `generate_qr_image`.
pub fn new_qr_code(
    prefilled_message: String,
    generate_qr_image: Option<&str>,
) -> Result<CreateQrCode, ServiceError> {
    message(&prefilled_message)?;
    let mut created = CreateQrCode::new(prefilled_message);
    if let Some(format) = generate_qr_image {
        created = created.with_image(image_format("generate_qr_image", format)?);
    }
    Ok(created)
}

/// What a page of codes is asked with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeQuery {
    /// Page size, 1 to [`LIST_LIMIT_MAX`] (default: Meta's).
    pub limit: Option<u32>,
    /// Meta's cursor from the previous page.
    pub after: Option<String>,
    /// Ask for an image of this format (`PNG` or `SVG`).
    pub image_format: Option<String>,
}

/// A QR code, as Meta describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    /// Its 14-character id.
    pub code: String,
    /// The text prefilled in the user's composer.
    pub prefilled_message: String,
    /// The `https://wa.me/message/<code>` short link.
    pub deep_link_url: String,
    /// The image's download URL, when an image format was asked for.
    pub qr_image_url: Option<String>,
}

impl From<QrCode> for Code {
    fn from(code: QrCode) -> Self {
        let QrCode {
            code,
            prefilled_message,
            deep_link_url,
            qr_image_url,
            ..
        } = code;
        Self {
            code: code.into_inner(),
            prefilled_message,
            deep_link_url,
            qr_image_url,
        }
    }
}

/// A page of a number's codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodePage {
    /// The codes, newest first.
    pub codes: Vec<Code>,
    /// Meta's cursor for the next page, when Meta says one follows.
    pub next_after: Option<String>,
}

// ─── The operations ──────────────────────────────────────────────────────

/// The QR code operations on a number of the caller's tenant: an
/// [`OwnedNumber`] its [`Authorizer`] made from that tenant's binding, for
/// a [`Caller`] it made, both admitted ([`Self::new`]), with the number's
/// WABA token. Cheap: a few references.
pub struct QrCodes<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for QrCodes<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QrCodes")
            .field("caller", self.caller)
            .field("number", self.number)
            .finish_non_exhaustive()
    }
}

impl<'a> QrCodes<'a> {
    /// The operations on `number` for `caller`, once `authz` admits both
    /// ([`Authorizer::admit`], [`Authorizer::admit_number`]) and `number`
    /// was made from a binding of `caller`'s tenant. Failed Graph calls are
    /// reported to nobody until [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `caller` or `number` another [`Authorizer`] made: `403 forbidden`.
    /// `number` another tenant's: `404 not_found`, logged at `warn`.
    /// Either way, before anything is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        number: &'a OwnedNumber,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_number(number)?;
        if number.tenant() != caller.tenant() {
            tracing::warn!("refused the QR code services on another tenant's number");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
            number,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    async fn failed(&self, error: &Error) -> ServiceError {
        let api = self.number.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api.with_details(error)
    }

    /// [`Self::failed`] for a call that carries a prefilled message, without
    /// Meta's texts: they may quote it, and a message's text is in no log
    /// and no answer of the service ([`OwnedNumber::failed_withholding_texts`]).
    async fn failed_with_message(&self, error: &Error) -> ServiceError {
        let api = self
            .number
            .failed_withholding_texts(self.authz, error)
            .await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    fn qr(&self) -> meta_whatsapp_rs::client::qr_codes::QrCodes {
        self.number
            .client()
            .qr_codes(self.number.phone_number_id().clone())
    }

    /// The fields to ask for, and the format they name.
    fn fields(format: Option<&str>) -> Result<QrCodeFields, ServiceError> {
        Ok(QrCodeFields {
            image_format: format
                .map(|format| image_format("image_format", format))
                .transpose()?,
            creation_time: false,
        })
    }

    /// Create a code from `new` ([`new_qr_code`]): `POST
    /// /{pn}/message_qrdls`, not replayed on a timeout (a replay would
    /// create a second code).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details` (a timeout
    /// `may_have_been_sent`).
    pub async fn create(&self, new: &CreateQrCode) -> Result<Code, ServiceError> {
        match self.qr().create(new).await {
            Ok(code) => Ok(Code::from(code)),
            Err(error) => Err(self.failed_with_message(&error).await),
        }
    }

    /// A page of the number's codes, newest first.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `limit` or `image_format`, before any
    /// request. Meta's refusal or failure, with its `details`.
    pub async fn list(&self, query: CodeQuery) -> Result<CodePage, ServiceError> {
        let CodeQuery {
            limit,
            after,
            image_format,
        } = query;
        if limit.is_some_and(|limit| !(1..=LIST_LIMIT_MAX).contains(&limit)) {
            return Err(ServiceError::invalid("limit"));
        }
        let fields = Self::fields(image_format.as_deref())?;
        let listed = self
            .qr()
            .list(&ListQrCodes {
                fields,
                code: None,
                limit,
                after,
                before: None,
            })
            .await;
        match listed {
            Ok(page) => {
                let next_after = page.next_cursor().map(str::to_owned);
                Ok(CodePage {
                    codes: page.data.into_iter().map(Code::from).collect(),
                    next_after,
                })
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The code `id`, looked for in the number's own list (see the
    /// [module](self)); `None` when the list does not hold exactly it.
    async fn owned(&self, id: &QrCodeId, fields: QrCodeFields) -> Result<Option<QrCode>, Error> {
        let page = self
            .qr()
            .list(&ListQrCodes {
                fields,
                code: Some(id.clone()),
                limit: Some(LIST_LIMIT_MAX),
                after: None,
                before: None,
            })
            .await?;
        Ok(page.data.into_iter().find(|code| code.code == *id))
    }

    /// The code `id`, if it is the number's.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `code` or `image_format`, before any
    /// request. `404 not_found`, without Meta's code or text, for a code
    /// not the number's. Meta's failure, with its `details`.
    pub async fn get(&self, id: &str, image_format: Option<&str>) -> Result<Code, ServiceError> {
        let id = code_id("code", id)?;
        let fields = Self::fields(image_format)?;
        match self.owned(&id, fields).await {
            Ok(Some(code)) => Ok(Code::from(code)),
            Ok(None) => {
                tracing::warn!("refused a QR code that is not the number's");
                Err(ServiceError::not_found())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Change the prefilled message of the code `id` to
    /// `prefilled_message`, if it is the number's: `POST
    /// /{pn}/message_qrdls` with `code`, replayed on a timeout (the library
    /// marks it idempotent: it sets a value).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `code` or `prefilled_message`, before any
    /// request. `404 not_found` for a code not the number's, and nothing
    /// changed. Meta's refusal or failure, with its `details`.
    pub async fn update(&self, id: &str, prefilled_message: String) -> Result<Code, ServiceError> {
        let id = code_id("code", id)?;
        message(&prefilled_message)?;
        match self.owned(&id, QrCodeFields::default()).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                tracing::warn!("refused a QR code that is not the number's");
                return Err(ServiceError::not_found());
            }
            Err(error) => return Err(self.failed_with_message(&error).await),
        }
        let updated = self
            .qr()
            .update(&UpdateQrCode {
                code: id,
                prefilled_message,
            })
            .await;
        match updated {
            Ok(code) => Ok(Code::from(code)),
            Err(error) => Err(self.failed_with_message(&error).await),
        }
    }

    /// Delete the code `id`, if it is the number's: `DELETE
    /// /{pn}/message_qrdls/{code}`. People who scan it afterwards are told
    /// it expired. The caller's audit event `qr_code_deleted`.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `code`, before any request. `404
    /// not_found` for a code not the number's, and nothing deleted. Meta's
    /// refusal or failure, with its `details`.
    pub async fn delete(&self, id: &str) -> Result<(), ServiceError> {
        let id = code_id("code", id)?;
        match self.owned(&id, QrCodeFields::default()).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                tracing::warn!("refused a QR code that is not the number's");
                return Err(ServiceError::not_found());
            }
            Err(error) => return Err(self.failed(&error).await),
        }
        match self.qr().delete(&id).await {
            Ok(()) => {
                tenant_audit(
                    DELETED,
                    self.caller.key_id(),
                    &Subject {
                        tenant: Some(self.caller.tenant().as_str()),
                        waba_id: Some(self.number.waba_id().as_str()),
                        phone_number_id: Some(self.number.phone_number_id().as_str()),
                        ..Subject::default()
                    },
                );
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(error: &ServiceError) -> String {
        error.field().unwrap_or("?").to_owned()
    }

    /// Meta's limits, checked before any request: a message of 1 to 140
    /// characters (counted as characters, not bytes), a code of 14 ASCII
    /// letters or digits, a format `PNG` or `SVG`. Decisive: each clause.
    #[test]
    fn a_codes_limits_are_checked_before_any_request() {
        assert!(new_qr_code("Cyber Monday".to_owned(), Some("SVG")).is_ok());
        assert!(new_qr_code("é".repeat(140), None).is_ok(), "140 characters");
        for text in [String::new(), "x".repeat(141)] {
            assert_eq!(
                field(&new_qr_code(text, None).unwrap_err()),
                "prefilled_message"
            );
        }
        assert_eq!(
            field(&new_qr_code("hi".to_owned(), Some("png")).unwrap_err()),
            "generate_qr_image"
        );
        assert!(code_id("code", "4O4YGZEG3RIVE1").is_ok());
        for bad in [
            "",
            "4O4YGZEG3RIVE",
            "4O4YGZEG3RIVE12",
            "4O4YGZEG3RIV-1",
            "é4O4YGZEG3RIVE",
        ] {
            assert_eq!(field(&code_id("code", bad).unwrap_err()), "code", "{bad:?}");
        }
        assert!(QrCodes::fields(Some("PNG")).is_ok());
        assert_eq!(
            field(&QrCodes::fields(Some("GIF")).unwrap_err()),
            "image_format"
        );
    }
}
