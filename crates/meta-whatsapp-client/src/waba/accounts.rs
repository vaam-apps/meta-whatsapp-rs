//! Creating a WABA under a business portfolio
//! (`reference/business/whatsapp-business-accounts-api`, `POST`).

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{FundingId, WabaId};
use meta_whatsapp_core::open_enum;
use serde::{Deserialize, Serialize};

use super::Business;

open_enum! {
    #![case_insensitive]
    /// The type of a WhatsApp Business Account (`WhatsAppBusinessType` in
    /// `reference/business/whatsapp-business-accounts-api`). Any other value
    /// is kept verbatim.
    pub enum WabaBusinessType {
        /// An enterprise.
        Enterprise => "ENTERPRISE",
        /// A small or medium business.
        Smb => "SMB",
    }
}

/// Body of `POST /{BUSINESS_ID}/whatsapp_business_accounts`
/// (`WhatsAppBusinessAccountCreationRequest`): only `name` is required.
///
/// The reference also lists `on_behalf_of_business_id`, for the
/// On-Behalf-Of ownership model, which Meta has deprecated and no longer
/// allows (`solution-providers/partner-initiated-waba-creation`, § On-Behalf-Of
/// model): it is not offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct NewWaba {
    /// The account's name (Meta: unique within the business portfolio).
    pub name: String,
    /// The primary funding source: a credit card or an extended credit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_funding_id: Option<FundingId>,
    /// A purchase order number, for billing reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purchase_order_number: Option<String>,
    /// The billing currency, an ISO 4217 code (three uppercase letters,
    /// e.g. `USD`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// The time zone id (an integer in the reference).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone_id: Option<u32>,
    /// Enterprise or SMB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub business_type: Option<WabaBusinessType>,
}

impl NewWaba {
    /// A WABA named `name`, every other field Meta's default.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            primary_funding_id: None,
            purchase_order_number: None,
            currency: None,
            timezone_id: None,
            business_type: None,
        }
    }

    /// Fund it from this payment credential.
    #[must_use]
    pub fn primary_funding_id(mut self, id: impl Into<FundingId>) -> Self {
        self.primary_funding_id = Some(id.into());
        self
    }

    /// A purchase order number.
    #[must_use]
    pub fn purchase_order_number(mut self, number: impl Into<String>) -> Self {
        self.purchase_order_number = Some(number.into());
        self
    }

    /// Bill in this ISO 4217 currency.
    #[must_use]
    pub fn currency(mut self, currency: impl Into<String>) -> Self {
        self.currency = Some(currency.into());
        self
    }

    /// The time zone id.
    #[must_use]
    pub fn timezone_id(mut self, timezone_id: u32) -> Self {
        self.timezone_id = Some(timezone_id);
        self
    }

    /// Enterprise or SMB.
    #[must_use]
    pub fn business_type(mut self, business_type: WabaBusinessType) -> Self {
        self.business_type = Some(business_type);
        self
    }

    /// Check what the reference states: a name, and an ISO 4217 currency.
    ///
    /// # Errors
    ///
    /// A [`ValidationError`] on `name`, `currency` or
    /// `purchase_order_number` (blank when set).
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        if self.name.trim().is_empty() {
            return Err(ValidationError::new("name", "required"));
        }
        if let Some(currency) = &self.currency {
            check_currency("currency", currency)?;
        }
        if self
            .purchase_order_number
            .as_deref()
            .is_some_and(|n| n.trim().is_empty())
        {
            return Err(ValidationError::new(
                "purchase_order_number",
                "must not be blank when set",
            ));
        }
        Ok(())
    }
}

/// An ISO 4217 alphabetic code: three ASCII uppercase letters.
pub(super) fn check_currency(
    field: &'static str,
    currency: &str,
) -> std::result::Result<(), ValidationError> {
    if currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_uppercase()) {
        Ok(())
    } else {
        Err(ValidationError::new(
            field,
            "must be an ISO 4217 code: three uppercase letters, e.g. USD",
        ))
    }
}

/// The answer to [`Business::create_whatsapp_business_account`]
/// (`WhatsAppBusinessAccountCreationResponse`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreatedWaba {
    /// The new WABA's id.
    pub id: WabaId,
    /// The payment account it bills to.
    pub payment_account_id: String,
}

impl Business {
    /// `POST /{BUSINESS_ID}/whatsapp_business_accounts`: create a WABA
    /// under this business portfolio (`reference/business/whatsapp-business-accounts-api`).
    /// Meta keeps it pending until verification completes.
    ///
    /// Checked first ([`NewWaba::validate`]: a name, an ISO 4217 currency),
    /// with no request on a refusal. **Never replayed** on a timeout or a
    /// transient error: a second attempt could create a second account;
    /// only an error that proves Meta refused the first
    /// ([`Error::may_resend`](meta_whatsapp_core::Error::may_resend)) is
    /// resent ([`crate::RetryPolicy`]). After a timeout, look for the
    /// account (`owned_whatsapp_business_accounts`) before trying again.
    pub async fn create_whatsapp_business_account(&self, waba: &NewWaba) -> Result<CreatedWaba> {
        waba.validate()?;
        self.client
            .post_at(&[self.business_id.as_str(), "whatsapp_business_accounts"])
            .json(waba)
            .context("create WhatsApp Business Account response")
            .send()
            .await
    }
}

#[cfg(test)]
mod tests {
    use http::Method;
    use meta_whatsapp_core::ErrorKind;
    use meta_whatsapp_core::error::TransportError;
    use meta_whatsapp_core::ids::BusinessId;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::{Client, RetryPolicy};

    fn client(t: &ScriptedTransport, retry: RetryPolicy) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(retry)
            .build()
            .unwrap()
    }

    fn retrying() -> RetryPolicy {
        RetryPolicy {
            max_retries: 3,
            base_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
        }
    }

    /// Every field of `WhatsAppBusinessAccountCreationRequest` but the
    /// deprecated `on_behalf_of_business_id`, with the reference's names
    /// and types (`timezone_id` an integer); the answer has the
    /// `WhatsAppBusinessAccountCreationResponse` fields (values ours: the
    /// page prints no example).
    #[tokio::test]
    async fn create_sends_the_documented_body_and_parses_the_answer() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"id": "106526625562206", "payment_account_id": "390100000000000"}),
        );
        let created = client(&t, RetryPolicy::NONE)
            .business(BusinessId::new("805021500648488").unwrap())
            .create_whatsapp_business_account(
                &NewWaba::new("Lucky Shrub")
                    .primary_funding_id("2950000000000001")
                    .purchase_order_number("PO-1")
                    .currency("USD")
                    .timezone_id(1)
                    .business_type(WabaBusinessType::Smb),
            )
            .await
            .unwrap();
        assert_eq!(created.id.as_str(), "106526625562206");
        assert_eq!(created.payment_account_id, "390100000000000");
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            "/v25.0/805021500648488/whatsapp_business_accounts"
        );
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({
                "name": "Lucky Shrub",
                "primary_funding_id": "2950000000000001",
                "purchase_order_number": "PO-1",
                "currency": "USD",
                "timezone_id": 1,
                "business_type": "SMB"
            }))
        );
        assert_eq!(t.remaining(), 0);

        // Only the name by default.
        t.push_json(200, json!({"id": "1", "payment_account_id": "2"}));
        client(&t, RetryPolicy::NONE)
            .business(BusinessId::new("805021500648488").unwrap())
            .create_whatsapp_business_account(&NewWaba::new("Lucky Shrub"))
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().json(),
            Some(json!({"name": "Lucky Shrub"}))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn create_is_checked_before_any_request() {
        let t = ScriptedTransport::new();
        let b = client(&t, RetryPolicy::NONE).business(BusinessId::new("1").unwrap());
        for (waba, field) in [
            (NewWaba::new(" "), "name"),
            (NewWaba::new("x").currency("usd"), "currency"),
            (NewWaba::new("x").currency("US"), "currency"),
            (NewWaba::new("x").currency("USDX"), "currency"),
            (NewWaba::new("x").currency("U$D"), "currency"),
            (
                NewWaba::new("x").purchase_order_number(""),
                "purchase_order_number",
            ),
        ] {
            let err = b.create_whatsapp_business_account(&waba).await.unwrap_err();
            assert!(
                matches!(&err, meta_whatsapp_core::Error::Validation(v) if v.field == field),
                "{field}: {err:?}"
            );
        }
        assert!(t.requests().is_empty());
    }

    /// The decisive test of the creation's retry rule: a timeout and a
    /// transient 500 are never replayed (one request each); a throttling
    /// answer, which proves Meta did nothing, is.
    #[tokio::test]
    async fn a_creation_is_never_replayed_after_a_timeout() {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        let b = client(&t, retrying()).business(BusinessId::new("805021500648488").unwrap());
        let err = b
            .create_whatsapp_business_account(&NewWaba::new("Lucky Shrub"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, meta_whatsapp_core::Error::Transport(_)),
            "{err:?}"
        );
        assert_eq!(
            t.requests().len(),
            1,
            "a timed-out creation is not replayed"
        );

        let t = ScriptedTransport::new();
        t.push_json(
            500,
            json!({"error": {"message": "An unexpected error occurred. Please retry your request", "type": "GraphMethodException", "code": 2, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn", "is_transient": true}}),
        );
        let err = client(&t, retrying())
            .business(BusinessId::new("805021500648488").unwrap())
            .create_whatsapp_business_account(&NewWaba::new("Lucky Shrub"))
            .await
            .unwrap_err();
        assert!(err.is_retryable());
        assert_eq!(t.requests().len(), 1, "a transient error is not replayed");

        // The page's 429 example: throttled, so refused before processing.
        let t = ScriptedTransport::new();
        t.push_json(
            429,
            json!({"error": {"message": "Application request limit reached", "type": "OAuthException", "code": 4, "error_subcode": 2446079, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn", "is_transient": true}}),
        );
        t.push_json(200, json!({"id": "1", "payment_account_id": "2"}));
        client(&t, retrying())
            .business(BusinessId::new("805021500648488").unwrap())
            .create_whatsapp_business_account(&NewWaba::new("Lucky Shrub"))
            .await
            .unwrap();
        assert_eq!(t.requests().len(), 2);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn the_pages_error_examples_keep_their_kind() {
        let t = ScriptedTransport::new();
        // The 422 example.
        t.push_json(
            422,
            json!({"error": {"message": "Cannot create WhatsApp Business Account: payment method is invalid", "type": "GraphMethodException", "code": 100, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
        );
        // The 403 example.
        t.push_json(
            403,
            json!({"error": {"message": "Your app doesn't have permission to create WhatsApp Business Accounts", "type": "OAuthException", "code": 200, "error_subcode": 1349174, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn", "error_user_title": "Permission Denied", "error_user_msg": "Your app doesn't have permission to access this resource"}}),
        );
        let b = client(&t, RetryPolicy::NONE).business(BusinessId::new("1").unwrap());
        let invalid = b
            .create_whatsapp_business_account(&NewWaba::new("x"))
            .await
            .unwrap_err();
        assert_eq!(invalid.kind(), ErrorKind::InvalidParameter);
        let denied = b
            .create_whatsapp_business_account(&NewWaba::new("x"))
            .await
            .unwrap_err();
        assert_eq!(denied.kind(), ErrorKind::Permission);
        assert_eq!(t.remaining(), 0);
    }

    #[test]
    fn business_type_keeps_an_unknown_value() {
        let parsed: WabaBusinessType = serde_json::from_value(json!("ENTERPRISE")).unwrap();
        assert_eq!(parsed, WabaBusinessType::Enterprise);
        let other: WabaBusinessType = serde_json::from_value(json!("NEW_KIND")).unwrap();
        assert_eq!(serde_json::to_value(&other).unwrap(), json!("NEW_KIND"));
    }
}
