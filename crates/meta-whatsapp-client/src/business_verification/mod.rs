//! Partner-led business verification: a Solution Partner submits an
//! onboarded customer's business for verification with the customer's
//! documents, follows its submissions, and reads the customer business's
//! verification status.
//!
//! Docs: `solution-providers/partner-led-business-verification` (only for
//! approved **Select Solution** and **Premier** Solution Partners), with
//! Meta's Marketing API reference pages for the two edges
//! (`ads-commerce/marketing-api/reference/business/self_certify_whatsapp_business`,
//! `…/self_certified_whatsapp_business_submissions`) and the `Business`
//! node (`ads-commerce/marketing-api/reference/business`,
//! `verification_status`).
//!
//! Doc paths are relative to `https://developers.facebook.com/documentation/`
//! (`business-messaging/whatsapp/` for the first; append `.md` for
//! Markdown).
//!
//! | Meta path | Method |
//! | --- | --- |
//! | `POST /{BUSINESS_PORTFOLIO_ID}/self_certify_whatsapp_business` | [`BusinessVerification::submit`] |
//! | `GET /{BUSINESS_PORTFOLIO_ID}/self_certified_whatsapp_business_submissions` | [`BusinessVerification::submissions`], [`BusinessVerification::submissions_stream`] |
//! | `GET /{BUSINESS_PORTFOLIO_ID}?fields=verification_status` | [`BusinessVerification::status`] |
//!
//! # Which token
//!
//! Every method authenticates with the token of the [`Client`] it was
//! built from. Meta requires:
//!
//! | Method | Business portfolio in the path | Token |
//! | --- | --- | --- |
//! | [`BusinessVerification::submit`], [`BusinessVerification::submissions`], [`BusinessVerification::submissions_stream`] | **yours** (the partner's) | the partner's **system user token**; its system user is an admin of your portfolio and granted your app `business_management` |
//! | [`BusinessVerification::status`] | the **customer's** | the customer's **business token** (from Embedded Signup) |
//!
//! ```no_run
//! # async fn demo(client: meta_whatsapp_client::Client, business_token: meta_whatsapp_core::secret::AccessToken, statement: bytes::Bytes) -> meta_whatsapp_core::Result<()> {
//! use meta_whatsapp_client::business_verification::VerificationDocument;
//! use meta_whatsapp_core::ids::BusinessId;
//! use meta_whatsapp_core::secret::AccessToken;
//!
//! let system = client.with_token(AccessToken::new("<SYSTEM_USER_TOKEN>"));
//! let partner = BusinessId::new("506914307656634");
//! let customer = BusinessId::new("2729063490586005");
//!
//! let document = VerificationDocument::from_file_name("bank_statement.pdf", statement)?;
//! let receipt = system
//!     .business_verification()
//!     .submit(&partner, &customer, &[document])
//!     .await?;
//! if receipt.attempts_left() == Some(0) {
//!     // The last one: if it is rejected, the customer verifies on their own.
//! }
//!
//! // Later, or on the webhook: the customer's business, with their token.
//! let business = client.with_token(business_token);
//! let verified = business
//!     .business_verification()
//!     .status(&customer)
//!     .await?
//!     .is_verified();
//! # Ok(()) }
//! ```
//!
//! # The outcome arrives by webhook
//!
//! Meta decides in about five minutes on average, sometimes hours, and
//! reports it in an `account_update` webhook with `event`
//! `PARTNER_CLIENT_CERTIFICATION_STATUS_UPDATE` (`meta_whatsapp_webhooks::fields::AccountUpdateValue::partner_client_certification_info`:
//! the customer's `client_business_id`, a `status`, `rejection_reasons`).
//! Subscribe your app to `account_update` and to the customer's WABA. The
//! page asks for a Direct Support ticket when no webhook has come after 24
//! hours. A customer who skipped the website in Embedded Signup triggers
//! `PARTNER_CLIENT_CERTIFICATION_NEEDED` first
//! (`embedded-signup/website-optional`): they cannot send messages until
//! their business is verified.
//!
//! # Limits (checked here where they can be)
//!
//! - One to [`MAX_DOCUMENTS`] documents per submission, each at most
//!   [`MAX_DOCUMENT_BYTES`], PDF, JPEG/JPG or PNG ([`DocumentType`]);
//!   checked before anything is sent, content included (a file whose
//!   bytes are not the type it claims is refused).
//! - [`MAX_SUBMISSIONS`] submissions per customer: after three rejections
//!   the customer must verify their business on their own. Meta counts
//!   them ([`SubmissionReceipt::verification_attempts`]); this module keeps
//!   no state, so it cannot refuse a fourth. A submission is never replayed
//!   on a transient error: a replay could spend one.
//!
//! # Where Meta's pages are loose (decided here)
//!
//! - The submissions example puts the customer filter inside `fields`
//!   (`?fields=end_business_id=<ID>`); the edge's reference lists
//!   `end_business_id` as a parameter of its own, which is what is sent.
//! - The page's example document path ends in `.txt`, a type it does not
//!   support; the list of supported types wins.
//! - The submission node's reference, where the page sends readers for the
//!   values of `verification_status` and `rejection_reasons`, is not served
//!   as Markdown. [`SubmissionStatus`] names the values `account_update`
//!   documents for the same submission's `status`, and keeps any other
//!   verbatim; [`RejectionReason`] names those `account_update` documents,
//!   which spell them with spaces (`LEGAL NAME NOT FOUND IN DOCUMENTS`)
//!   where this page spells one with underscores
//!   (`LEGAL_NAME_NOT_FOUND_IN_DOCUMENTS`): both are read.
//! - `submitted_time` and `update_time` are kept as Meta's strings: the
//!   page shows placeholders only, no format.
//! - "5 MB" is read as 5 MiB, the larger reading, as for media uploads.
//! - The pages' examples mix API versions (v17.0, v21.0); the client's
//!   configured version is used for all of them.
//! - Which documents Meta accepts as proof is a Help Center article
//!   ("Upload official documents to verify your business"), not an API
//!   rule: nothing here checks what a document says.

use std::fmt;

use bytes::Bytes;
use futures::Stream;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{BusinessId, VerificationSubmissionId};
use meta_whatsapp_core::paging::Page;
use meta_whatsapp_core::transport::Multipart;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

#[doc(no_inline)]
pub use crate::waba::{BusinessInfo, BusinessVerificationStatus};

use crate::request::{paginate_or_error, reject_cursors, send_checked};
use crate::templates::macros::string_enum;
use crate::{Client, GraphRequest};

/// At most this many documents per submission.
pub const MAX_DOCUMENTS: usize = 3;

/// At most this many bytes per document ("5 MB", read as 5 MiB).
pub const MAX_DOCUMENT_BYTES: usize = 5 * 1024 * 1024;

/// Submissions a partner may make for one customer: after three
/// rejections the customer must complete business verification on their
/// own (Meta's Help Center: "How to Verify Your Business on Meta").
pub const MAX_SUBMISSIONS: u32 = 3;

/// Entry point, see [`Client::business_verification`].
#[derive(Debug, Clone)]
pub struct BusinessVerification {
    client: Client,
}

impl Client {
    /// Partner-led business verification, authenticated with this client's
    /// token (see the [module docs](crate::business_verification) for which
    /// token each call needs).
    pub fn business_verification(&self) -> BusinessVerification {
        BusinessVerification {
            client: self.clone(),
        }
    }
}

/// The file types the page lists as supported for a business document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DocumentType {
    /// PDF (`application/pdf`).
    Pdf,
    /// JPEG or JPG (`image/jpeg`).
    Jpeg,
    /// PNG (`image/png`).
    Png,
}

impl DocumentType {
    /// The MIME type sent as the part's content type.
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    /// The type of a MIME type (`application/pdf`, `image/jpeg`,
    /// `image/jpg`, `image/png`; case and parameters ignored).
    pub fn from_mime_type(mime_type: &str) -> Result<Self, ValidationError> {
        let essence = mime_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        match essence.as_str() {
            "application/pdf" => Ok(Self::Pdf),
            "image/jpeg" | "image/jpg" => Ok(Self::Jpeg),
            "image/png" => Ok(Self::Png),
            _ => Err(unsupported()),
        }
    }

    /// The type of a file name's extension (`.pdf`, `.jpeg`, `.jpg`,
    /// `.png`, any case).
    pub fn from_file_name(file_name: &str) -> Result<Self, ValidationError> {
        let extension = file_name
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase());
        match extension.as_deref() {
            Some("pdf") => Ok(Self::Pdf),
            Some("jpeg" | "jpg") => Ok(Self::Jpeg),
            Some("png") => Ok(Self::Png),
            _ => Err(unsupported()),
        }
    }

    /// Whether `data` starts like a file of this type: `%PDF-` within the
    /// first 1024 bytes (where the PDF specification allows the header),
    /// the JPEG SOI marker, the PNG signature.
    pub fn matches(self, data: &[u8]) -> bool {
        match self {
            Self::Pdf => data
                .get(..data.len().min(1024))
                .is_some_and(|head| head.windows(5).any(|w| w == b"%PDF-")),
            Self::Jpeg => data.starts_with(&[0xFF, 0xD8, 0xFF]),
            Self::Png => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        }
    }
}

fn unsupported() -> ValidationError {
    ValidationError::new(
        "business_documents",
        "supported file types are PDF, JPEG, JPG and PNG",
    )
}

/// One business document to submit (`business_documents[]`): checked when
/// built against what the page documents (type, size) and against its own
/// content, so a file that cannot pass is refused before any request.
/// Whether Meta counts a submission it refuses for a document towards the
/// three is not documented; "5 MB" is read as 5 MiB ([`MAX_DOCUMENT_BYTES`]).
///
/// `Debug` shows the file name, type and size, never the content.
#[derive(Clone, PartialEq, Eq)]
pub struct VerificationDocument {
    file_name: String,
    document_type: DocumentType,
    data: Bytes,
}

impl VerificationDocument {
    /// A document of `document_type`. Refused when the file name is empty
    /// or has control characters, the data is empty, larger than
    /// [`MAX_DOCUMENT_BYTES`], or does not start like `document_type`
    /// ([`DocumentType::matches`]).
    pub fn new(
        file_name: impl Into<String>,
        document_type: DocumentType,
        data: impl Into<Bytes>,
    ) -> Result<Self, ValidationError> {
        let file_name = file_name.into();
        let data = data.into();
        if file_name.trim().is_empty() {
            return Err(ValidationError::new(
                "business_documents",
                "file name required",
            ));
        }
        if file_name.chars().any(char::is_control) {
            return Err(ValidationError::new(
                "business_documents",
                "file name has control characters",
            ));
        }
        if data.is_empty() {
            return Err(ValidationError::new("business_documents", "empty document"));
        }
        if data.len() > MAX_DOCUMENT_BYTES {
            return Err(ValidationError::new(
                "business_documents",
                "each document is at most 5 MB",
            ));
        }
        if !document_type.matches(&data) {
            return Err(ValidationError::new(
                "business_documents",
                "content is not the declared file type",
            ));
        }
        Ok(Self {
            file_name,
            document_type,
            data,
        })
    }

    /// A document typed by its file name's extension
    /// ([`DocumentType::from_file_name`]), then checked as by [`Self::new`].
    pub fn from_file_name(
        file_name: impl Into<String>,
        data: impl Into<Bytes>,
    ) -> Result<Self, ValidationError> {
        let file_name = file_name.into();
        let document_type = DocumentType::from_file_name(&file_name)?;
        Self::new(file_name, document_type, data)
    }

    /// The file name sent with the part.
    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    /// The file type.
    pub fn document_type(&self) -> DocumentType {
        self.document_type
    }

    /// The content.
    pub fn data(&self) -> &Bytes {
        &self.data
    }
}

impl fmt::Debug for VerificationDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerificationDocument")
            .field("file_name", &self.file_name)
            .field("document_type", &self.document_type)
            .field("len", &self.data.len())
            .finish()
    }
}

/// Response of `POST /{BUSINESS_PORTFOLIO_ID}/self_certify_whatsapp_business`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SubmissionReceipt {
    /// Meta's acknowledgement, e.g. "Your request has been received and
    /// will be reviewed shortly."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// How many submissions you have made for this customer, this one
    /// included. Read leniently (a number or a numeric string; anything
    /// else is `None`): the submission is spent by the time this is read,
    /// so an unexpected spelling must not turn it into an error that
    /// invites a second one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_attempts"
    )]
    pub verification_attempts: Option<u32>,
}

/// `verification_attempts` as a number or a numeric string; any other
/// value (null, a negative or fractional number, text) is `None`, never a
/// decode error.
fn lenient_attempts<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

impl SubmissionReceipt {
    /// Submissions left for this customer ([`MAX_SUBMISSIONS`] minus
    /// [`Self::verification_attempts`]); `None` when Meta did not say.
    pub fn attempts_left(&self) -> Option<u32> {
        self.verification_attempts
            .map(|used| MAX_SUBMISSIONS.saturating_sub(used))
    }
}

string_enum! {
    /// A submission's `verification_status`. The named values are those
    /// `account_update` documents for the same submission's `status`
    /// (`meta_whatsapp_webhooks::fields::CertificationStatus` there; see the
    /// [module docs](crate::business_verification) for why). Parsed
    /// case-insensitively; any other value is kept verbatim.
    pub enum SubmissionStatus {
        /// Reviewed and approved.
        Approved => "APPROVED",
        /// Discarded: technical issues, or no progress for a while.
        Discarded => "DISCARDED",
        /// Reviewed and rejected; see the rejection reasons.
        Failed => "FAILED",
        /// Pending review.
        Pending => "PENDING",
        /// Revoked.
        Revoked => "REVOKED",
    }
}

/// Why a submission was rejected, from a `rejection_reasons` string of a
/// [`VerificationSubmission`] or of the `account_update` webhook's
/// `partner_client_certification_info` ([`RejectionReason::parse`]).
///
/// The named values and their meaning are `account_update`'s.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RejectionReason {
    /// The country of the submitted address is not the one on the
    /// customer's business profile.
    AddressNotMatching,
    /// Not eligible for verification from partner-provided information:
    /// the customer can still verify with Meta directly.
    BusinessNotEligible,
    /// The legal name does not match the business profile's legal or
    /// business name.
    LegalNameNotMatching,
    /// The legal name was not found in the documents (not mentioned, or
    /// unreadable).
    LegalNameNotFoundInDocuments,
    /// The documents could not be processed (corrupted, password
    /// protected, unsupported format).
    MalformedDocuments,
    /// Not rejected (`NONE`, sent with every outcome).
    None,
    /// The website domain does not match the business profile's.
    WebsiteNotMatching,
    /// Any other value, verbatim.
    Other(String),
}

impl RejectionReason {
    /// Read one of Meta's `rejection_reasons` values, spelled with spaces
    /// (`LEGAL NAME NOT MATCHING`, `account_update`) or underscores
    /// (`LEGAL_NAME_NOT_FOUND_IN_DOCUMENTS`, the partner-led page), in any
    /// case; any other value is kept verbatim in [`Self::Other`]. The same
    /// as `reason.parse::<RejectionReason>()`.
    pub fn parse(reason: &str) -> Self {
        match reason
            .trim()
            .replace('_', " ")
            .to_ascii_uppercase()
            .as_str()
        {
            "ADDRESS NOT MATCHING" => Self::AddressNotMatching,
            "BUSINESS NOT ELIGIBLE" => Self::BusinessNotEligible,
            "LEGAL NAME NOT MATCHING" => Self::LegalNameNotMatching,
            "LEGAL NAME NOT FOUND IN DOCUMENTS" => Self::LegalNameNotFoundInDocuments,
            "MALFORMED DOCUMENTS" => Self::MalformedDocuments,
            "NONE" => Self::None,
            "WEBSITE NOT MATCHING" => Self::WebsiteNotMatching,
            _ => Self::Other(reason.to_owned()),
        }
    }

    /// The value in `account_update`'s spelling (with spaces), or the
    /// [`Self::Other`] value verbatim.
    pub fn as_str(&self) -> &str {
        match self {
            Self::AddressNotMatching => "ADDRESS NOT MATCHING",
            Self::BusinessNotEligible => "BUSINESS NOT ELIGIBLE",
            Self::LegalNameNotMatching => "LEGAL NAME NOT MATCHING",
            Self::LegalNameNotFoundInDocuments => "LEGAL NAME NOT FOUND IN DOCUMENTS",
            Self::MalformedDocuments => "MALFORMED DOCUMENTS",
            Self::None => "NONE",
            Self::WebsiteNotMatching => "WEBSITE NOT MATCHING",
            Self::Other(s) => s,
        }
    }

    /// Whether the customer can only verify on their own
    /// ([`Self::BusinessNotEligible`]).
    pub fn is_ineligible(&self) -> bool {
        matches!(self, Self::BusinessNotEligible)
    }
}

impl std::str::FromStr for RejectionReason {
    type Err = std::convert::Infallible;

    /// [`RejectionReason::parse`].
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(s))
    }
}

impl fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for RejectionReason {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RejectionReason {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Self::parse(&s))
    }
}

/// `submitted_info` of a submission.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SubmittedInfo {
    /// The customer's business vertical (absent from the rejected example).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_vertical: Option<String>,
}

/// One submission, as `self_certified_whatsapp_business_submissions` lists
/// it with its default fields.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct VerificationSubmission {
    /// The submission id.
    pub id: VerificationSubmissionId,
    /// Its status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_status: Option<SubmissionStatus>,
    /// Why it was rejected, verbatim (rejected submissions); read them with
    /// [`Self::reasons`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejection_reasons: Vec<String>,
    /// When it was submitted, as Meta sent it (format undocumented).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_time: Option<String>,
    /// When it last changed, as Meta sent it (format undocumented).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_time: Option<String>,
    /// The customer's business portfolio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_business_id: Option<BusinessId>,
    /// What was submitted (`{}` in the rejected example).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitted_info: Option<SubmittedInfo>,
}

impl VerificationSubmission {
    /// [`Self::rejection_reasons`], typed ([`RejectionReason::parse`]).
    pub fn reasons(&self) -> Vec<RejectionReason> {
        self.rejection_reasons
            .iter()
            .map(|r| RejectionReason::parse(r))
            .collect()
    }
}

/// Options of [`BusinessVerification::submissions`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListVerificationSubmissions {
    /// Only the submissions for this customer business
    /// (`end_business_id`); all your customers' when `None`.
    pub end_business_id: Option<BusinessId>,
    /// Cursor from a previous page's `paging.cursors.after`
    /// ([`Page::next_cursor`]); for [`BusinessVerification::submissions`]
    /// only, the stream manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl ListVerificationSubmissions {
    /// Every customer's submissions, first page.
    pub fn new() -> Self {
        Self::default()
    }

    /// Only this customer business's submissions.
    #[must_use]
    pub fn end_business_id(mut self, business_id: impl Into<BusinessId>) -> Self {
        self.end_business_id = Some(business_id.into());
        self
    }

    /// Continue after this cursor.
    #[must_use]
    pub fn after(mut self, cursor: impl Into<String>) -> Self {
        self.after = Some(cursor.into());
        self
    }

    /// Go back before this cursor.
    #[must_use]
    pub fn before(mut self, cursor: impl Into<String>) -> Self {
        self.before = Some(cursor.into());
        self
    }
}

fn required(field: &'static str, id: &BusinessId) -> Result<(), ValidationError> {
    if id.as_str().trim().is_empty() {
        return Err(ValidationError::new(field, "required"));
    }
    Ok(())
}

impl BusinessVerification {
    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `POST /{BUSINESS_PORTFOLIO_ID}/self_certify_whatsapp_business`
    /// (multipart: `end_business_id`, then one `business_documents[]` file
    /// part per document): submit the customer business `end_business_id`
    /// for verification. `business_id` is **your** business portfolio (both
    /// are [`BusinessId`]s: mind the order); **the partner's system user
    /// token**.
    ///
    /// Checks locally first: both ids set, one to [`MAX_DOCUMENTS`]
    /// documents (each already checked by [`VerificationDocument`]). Never
    /// replayed on a transient error (each submission counts towards
    /// [`MAX_SUBMISSIONS`]); a lost answer may have been received: read
    /// [`Self::submissions`] for the customer before submitting again.
    /// `{"success": false}` is an error.
    pub async fn submit(
        &self,
        business_id: &BusinessId,
        end_business_id: &BusinessId,
        documents: &[VerificationDocument],
    ) -> Result<SubmissionReceipt> {
        required("business_id", business_id)?;
        required("end_business_id", end_business_id)?;
        if documents.is_empty() {
            return Err(ValidationError::new("business_documents", "at least one document").into());
        }
        if documents.len() > MAX_DOCUMENTS {
            return Err(ValidationError::new("business_documents", "at most 3 documents").into());
        }
        let form = documents.iter().fold(
            Multipart::new().text("end_business_id", end_business_id.as_str()),
            |form, doc| {
                form.file(
                    "business_documents[]",
                    doc.file_name.as_str(),
                    doc.document_type.mime_type(),
                    doc.data.clone(),
                )
            },
        );
        let request = self
            .client
            .post_at(&[business_id.as_str(), "self_certify_whatsapp_business"])
            .multipart(form)
            .idempotent(false);
        send_checked(request, "business verification submission response").await
    }

    fn submissions_request(
        &self,
        business_id: &BusinessId,
        query: &ListVerificationSubmissions,
    ) -> Result<GraphRequest> {
        required("business_id", business_id)?;
        if let Some(end) = &query.end_business_id {
            required("end_business_id", end)?;
        }
        Ok(self
            .client
            .get_at(&[
                business_id.as_str(),
                "self_certified_whatsapp_business_submissions",
            ])
            .query_opt("end_business_id", query.end_business_id.as_ref())
            .context("business verification submissions"))
    }

    /// `GET /{BUSINESS_PORTFOLIO_ID}/self_certified_whatsapp_business_submissions`:
    /// one page of the submissions you made, for one customer
    /// ([`ListVerificationSubmissions::end_business_id`]) or all of them.
    /// `business_id` is **your** business portfolio; **the partner's system
    /// user token**.
    pub async fn submissions(
        &self,
        business_id: &BusinessId,
        query: &ListVerificationSubmissions,
    ) -> Result<Page<VerificationSubmission>> {
        self.submissions_request(business_id, query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every submission, following cursors. The stream manages them itself:
    /// a query with `after` or `before` set is refused (the stream's single
    /// item is that validation error), as is a missing id.
    pub fn submissions_stream(
        &self,
        business_id: &BusinessId,
        query: &ListVerificationSubmissions,
    ) -> impl Stream<Item = Result<VerificationSubmission>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.submissions_request(business_id, query)),
        )
    }

    /// `GET /{BUSINESS_PORTFOLIO_ID}?fields=verification_status`: the
    /// verification status of the **customer's** business portfolio, with
    /// **the customer's business token**. Read
    /// [`BusinessInfo::verification_status`] or [`BusinessInfo::is_verified`]
    /// (the same as `client.business(id).get(&["verification_status"])`).
    ///
    /// Meta's alternative, `business_verification_status` on the
    /// customer's WABA (same token), is
    /// [`WabaInfo::business_verification_status`](crate::waba::WabaInfo::business_verification_status)
    /// through [`Waba::get`](crate::waba::Waba::get).
    pub async fn status(&self, business_id: &BusinessId) -> Result<BusinessInfo> {
        required("business_id", business_id)?;
        self.client
            .get_at(&[business_id.as_str()])
            .query("fields", "verification_status")
            .context("business verification status")
            .send()
            .await
    }
}

#[cfg(test)]
mod tests;
