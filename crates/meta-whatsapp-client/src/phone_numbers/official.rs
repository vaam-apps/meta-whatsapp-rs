//! Official Business Account (the blue check): the status and the request
//! (`official-business-accounts`,
//! `reference/whatsapp-business-phone-number/whatsapp-business-account-official-business-account-status-api`).
//!
//! # Where Meta's pages disagree
//!
//! - **Reading the status**: the guide reads the `official_business_account`
//!   field of the number (`GET /{Phone-Number-ID}?fields=official_business_account`,
//!   with an example), the reference an edge
//!   (`GET /{Phone-Number-ID}/official_business_account`, without one).
//!   This client follows the guide's example.
//! - **Status values**: the guide's example answers `NOT_STARTED`, which
//!   the reference's list (`PENDING`, `APPROVED`, `REJECTED`,
//!   `UNDER_REVIEW`, `EXPIRED`, `CANCELLED`) lacks. [`ObaStatus`] has
//!   both, and keeps any other value in `Other`.
//! - **Withdrawing or resubmitting**: the reference says the request
//!   endpoint also withdraws a pending application, but documents no field
//!   that asks for it; that is not offered.

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::PhoneNumberId;
use serde::{Deserialize, Serialize};

use super::PhoneNumber;
use crate::request::decode_json;
use crate::templates::macros::string_enum;

/// Fewest supporting links an application may carry, when it carries any.
pub const MIN_SUPPORTING_LINKS: usize = 5;
/// Most supporting links an application may carry.
pub const MAX_SUPPORTING_LINKS: usize = 10;

string_enum! {
    /// Where a number's Official Business Account application stands
    /// (`oba_status`).
    pub enum ObaStatus {
        /// No application yet (the guide's example).
        NotStarted => "NOT_STARTED",
        /// Submitted.
        Pending => "PENDING",
        /// Under review.
        UnderReview => "UNDER_REVIEW",
        /// Granted: the number shows the blue check.
        Approved => "APPROVED",
        /// Refused; a new request is possible after 30 days.
        Rejected => "REJECTED",
        /// Expired.
        Expired => "EXPIRED",
        /// Cancelled.
        Cancelled => "CANCELLED",
    }
}

/// A number's Official Business Account status.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct OfficialBusinessAccount {
    /// The phone number id (in the reference's schema; the guide's example
    /// carries it next to this object instead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<PhoneNumberId>,
    /// Where the application stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oba_status: Option<ObaStatus>,
    /// Meta's explanation of the status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
}

/// An Official Business Account application
/// (`POST /{Phone-Number-ID}/official_business_account`,
/// `OfficialBusinessAccountUpdateRequest`). `None` fields are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ObaApplication {
    /// The business's official website.
    pub business_website_url: String,
    /// The country the business mainly operates in.
    pub primary_country_of_operation: String,
    /// The business's main language.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_language: Option<String>,
    /// The parent business or brand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_business_or_brand: Option<String>,
    /// Links showing the business is notable: 5 to 10 of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supporting_links: Option<Vec<String>>,
    /// Anything else in support of the application.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_supporting_information: Option<String>,
}

impl ObaApplication {
    /// An application with the two required fields.
    pub fn new(
        business_website_url: impl Into<String>,
        primary_country_of_operation: impl Into<String>,
    ) -> Self {
        Self {
            business_website_url: business_website_url.into(),
            primary_country_of_operation: primary_country_of_operation.into(),
            primary_language: None,
            parent_business_or_brand: None,
            supporting_links: None,
            additional_supporting_information: None,
        }
    }

    /// The business's main language.
    #[must_use]
    pub fn primary_language(mut self, language: impl Into<String>) -> Self {
        self.primary_language = Some(language.into());
        self
    }

    /// The parent business or brand.
    #[must_use]
    pub fn parent_business_or_brand(mut self, parent: impl Into<String>) -> Self {
        self.parent_business_or_brand = Some(parent.into());
        self
    }

    /// Links showing the business is notable (5 to 10).
    #[must_use]
    pub fn supporting_links<I, S>(mut self, links: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.supporting_links = Some(links.into_iter().map(Into::into).collect());
        self
    }

    /// Anything else in support of the application.
    #[must_use]
    pub fn additional_supporting_information(mut self, information: impl Into<String>) -> Self {
        self.additional_supporting_information = Some(information.into());
        self
    }

    /// Check what the reference states: the two required fields are not
    /// empty, and supporting links, when given, number 5 to 10.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        if self.business_website_url.trim().is_empty() {
            return Err(ValidationError::new(
                "business_website_url",
                "must not be empty",
            ));
        }
        if self.primary_country_of_operation.trim().is_empty() {
            return Err(ValidationError::new(
                "primary_country_of_operation",
                "must not be empty",
            ));
        }
        if let Some(links) = &self.supporting_links
            && !(MIN_SUPPORTING_LINKS..=MAX_SUPPORTING_LINKS).contains(&links.len())
        {
            return Err(ValidationError::new(
                "supporting_links",
                "must hold 5-10 links",
            ));
        }
        Ok(())
    }
}

/// Meta's answer to an application (`OfficialBusinessAccountUpdateResponse`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ObaApplicationResponse {
    /// Always `true` here: `false` is returned as an error.
    pub success: bool,
    /// Meta's description of the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The status after the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_status: Option<OfficialBusinessAccount>,
    /// An id to follow the application with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracking_id: Option<String>,
}

#[derive(Deserialize)]
struct ObaEnvelope {
    official_business_account: OfficialBusinessAccount,
}

impl PhoneNumber {
    /// The number's Official Business Account status
    /// (`GET /{Phone-Number-ID}?fields=official_business_account`, the
    /// guide's form; the reference's edge is not used, see
    /// [`crate::phone_numbers`]).
    pub async fn official_business_account(&self) -> Result<OfficialBusinessAccount> {
        let env: ObaEnvelope = self
            .client
            .get_at(&[self.phone_number_id.as_str()])
            .query("fields", "official_business_account")
            .context("official business account status")
            .send()
            .await?;
        Ok(env.official_business_account)
    }

    /// `POST /{Phone-Number-ID}/official_business_account`: apply for
    /// Official Business Account status for this number.
    ///
    /// This is an application to Meta for review, not a legal act. It is
    /// explicit and never automatic: this crate never calls it for you. It
    /// is not replayed after a timeout (a `POST`): read
    /// [`Self::official_business_account`] before applying again. Meta
    /// refuses a new request within 30 days of a rejection, and grants the
    /// status only to numbers that meet its criteria (a verified business
    /// portfolio, two-step verification on, an approved display name, 30
    /// days on the platform).
    ///
    /// The application is checked first ([`ObaApplication::validate`]). A
    /// response with `success: false` is an error.
    pub async fn request_official_business_account(
        &self,
        application: &ObaApplication,
    ) -> Result<ObaApplicationResponse> {
        application.validate()?;
        let context = "official business account request response";
        let resp = self
            .client
            .post_at(&[self.phone_number_id.as_str(), "official_business_account"])
            .json(application)
            .context(context)
            .send_raw()
            .await?;
        let body: ObaApplicationResponse = decode_json(context, &resp.body)?;
        if body.success {
            Ok(body)
        } else {
            Err(meta_whatsapp_core::Error::Http {
                status: resp.status.as_u16(),
                body_snippet: meta_whatsapp_core::error::snippet(&resp.body),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use http::Method;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::{Client, RetryPolicy};

    const ID: &str = "106540352242922";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn links() -> Vec<String> {
        (1..=5)
            .map(|i| format!("https://news.example/{i}"))
            .collect()
    }

    #[tokio::test]
    async fn status_reads_the_field_and_parses_the_guide_example() {
        // official-business-accounts, "Getting OBA status via API".
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"official_business_account": {"oba_status": "NOT_STARTED"}, "id": ID}),
        );
        let status = client(&t)
            .phone_number(ID)
            .official_business_account()
            .await
            .unwrap();
        assert_eq!(status.oba_status, Some(ObaStatus::NotStarted));
        assert_eq!(status.status_message, None);
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/106540352242922");
        assert_eq!(
            req.query("fields").as_deref(),
            Some("official_business_account")
        );
        assert_eq!(req.url.query_pairs().count(), 1);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn status_keeps_reference_and_unknown_values() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"official_business_account": {"id": ID, "oba_status": "UNDER_REVIEW", "status_message": "In review"}, "id": ID}),
        );
        t.push_json(
            200,
            json!({"official_business_account": {"oba_status": "ON_HOLD"}, "id": ID}),
        );
        t.push_json(200, json!({"id": ID}));
        let pn = client(&t).phone_number(ID);
        let a = pn.official_business_account().await.unwrap();
        assert_eq!(a.oba_status, Some(ObaStatus::UnderReview));
        assert_eq!(a.status_message.as_deref(), Some("In review"));
        assert_eq!(a.id, Some(PhoneNumberId::new(ID)));
        let b = pn.official_business_account().await.unwrap();
        assert_eq!(b.oba_status, Some(ObaStatus::Other("ON_HOLD".into())));
        // No `official_business_account` in the answer: a decode error, not
        // an empty status.
        let err = pn.official_business_account().await.unwrap_err();
        assert!(
            matches!(err, meta_whatsapp_core::Error::Decode { .. }),
            "{err:?}"
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn request_sends_every_field_of_the_application() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "success": true,
                "message": "Application submitted",
                "updated_status": {"id": ID, "oba_status": "PENDING", "status_message": "Submitted"},
                "tracking_id": "T-1"
            }),
        );
        let application = ObaApplication::new("https://www.luckyshrub.example", "US")
            .primary_language("en")
            .parent_business_or_brand("Lucky Shrub Group")
            .supporting_links(links())
            .additional_supporting_information("Founded in 1999.");
        let resp = client(&t)
            .phone_number(ID)
            .request_official_business_account(&application)
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            "/v25.0/106540352242922/official_business_account"
        );
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({
                "business_website_url": "https://www.luckyshrub.example",
                "primary_country_of_operation": "US",
                "primary_language": "en",
                "parent_business_or_brand": "Lucky Shrub Group",
                "supporting_links": [
                    "https://news.example/1", "https://news.example/2",
                    "https://news.example/3", "https://news.example/4",
                    "https://news.example/5"
                ],
                "additional_supporting_information": "Founded in 1999."
            }))
        );
        assert!(resp.success);
        assert_eq!(resp.tracking_id.as_deref(), Some("T-1"));
        assert_eq!(
            resp.updated_status.and_then(|s| s.oba_status),
            Some(ObaStatus::Pending)
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn request_with_only_the_required_fields() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true, "message": "ok"}));
        client(&t)
            .phone_number(ID)
            .request_official_business_account(&ObaApplication::new(
                "https://www.luckyshrub.example",
                "US",
            ))
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().json(),
            Some(json!({
                "business_website_url": "https://www.luckyshrub.example",
                "primary_country_of_operation": "US"
            }))
        );
        assert_eq!(t.remaining(), 0);

        t.push_json(200, json!({"success": false, "message": "not eligible"}));
        assert!(
            client(&t)
                .phone_number(ID)
                .request_official_business_account(&ObaApplication::new(
                    "https://www.luckyshrub.example",
                    "US",
                ))
                .await
                .is_err()
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn request_is_validated_first_and_never_replayed() {
        let t = ScriptedTransport::new();
        let pn = client(&t).phone_number(ID);
        for (application, field) in [
            (ObaApplication::new(" ", "US"), "business_website_url"),
            (
                ObaApplication::new("https://x.example", ""),
                "primary_country_of_operation",
            ),
            (
                ObaApplication::new("https://x.example", "US").supporting_links(["a"; 4]),
                "supporting_links",
            ),
            (
                ObaApplication::new("https://x.example", "US").supporting_links(["a"; 11]),
                "supporting_links",
            ),
        ] {
            let err = pn
                .request_official_business_account(&application)
                .await
                .unwrap_err();
            assert!(
                matches!(&err, meta_whatsapp_core::Error::Validation(v) if v.field == field),
                "{field}: {err:?}"
            );
        }
        assert!(t.requests().is_empty());
        assert!(
            ObaApplication::new("https://x.example", "US")
                .supporting_links(["a"; 10])
                .validate()
                .is_ok()
        );

        let t = ScriptedTransport::new();
        t.push_error(|| meta_whatsapp_core::error::TransportError::Timeout);
        let c = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy {
                max_retries: 3,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            })
            .build()
            .unwrap();
        assert!(
            c.phone_number(ID)
                .request_official_business_account(&ObaApplication::new("https://x.example", "US"))
                .await
                .is_err()
        );
        assert_eq!(t.requests().len(), 1, "an application is never replayed");
    }
}
