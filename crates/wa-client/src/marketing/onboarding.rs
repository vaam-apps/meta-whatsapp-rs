//! MM API onboarding: eligibility and status (WABA and business portfolio),
//! the partner Intent API, and the Cloud API marketing switch.
//!
//! Sources: `marketing-messages/onboarding`,
//! `marketing-messages/onboard-business-customers`,
//! `reference/business/whatsapp-business-partner-onboarding-to-mm-lite-api`,
//! `marketing-messages/send-marketing-messages` ("Disable marketing messages
//! on Cloud API"), `support/error-codes` (MM API section).

use serde::{Deserialize, Serialize};
use wa_core::error::snippet;
use wa_core::ids::{BusinessId, WabaId};
use wa_core::paging::Page;
use wa_core::{Error, Result};

use super::wire_enum;
use crate::Client;

/// MM API settings and status of one WhatsApp Business Account. See
/// [`Client::marketing_account`].
#[derive(Debug, Clone)]
pub struct MarketingAccount {
    client: Client,
    waba_id: WabaId,
}

/// MM API onboarding of one business portfolio. See
/// [`Client::marketing_business`].
#[derive(Debug, Clone)]
pub struct MarketingBusiness {
    client: Client,
    business_id: BusinessId,
}

impl Client {
    /// [`MarketingAccount`] API for `waba_id`: onboarding status and the
    /// switch that blocks marketing templates on Cloud API.
    pub fn marketing_account(&self, waba_id: impl Into<WabaId>) -> MarketingAccount {
        MarketingAccount {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }

    /// [`MarketingBusiness`] API for a business portfolio: Terms of Service
    /// status, eligible client WABAs, and (for partners) the onboarding
    /// Intent API.
    pub fn marketing_business(&self, business_id: impl Into<BusinessId>) -> MarketingBusiness {
        MarketingBusiness {
            client: self.clone(),
            business_id: business_id.into(),
        }
    }
}

wire_enum! {
    /// `marketing_messages_onboarding_status` of a WABA.
    ///
    /// Only these two values are spelled out in Meta's pages; the reference
    /// they point to for the full list does not document the field, so
    /// anything else arrives as `Unknown`.
    pub enum OnboardingStatus {
        /// Can be onboarded.
        Eligible = "ELIGIBLE",
        /// Already onboarded.
        Onboarded = "ONBOARDED",
    }
}

wire_enum! {
    /// Where a business portfolio is with the MM API Terms of Service.
    pub enum TermsStatus {
        /// No request and no signature yet.
        NotStarted = "NOT_STARTED",
        /// An onboarding request was sent to the business's admins.
        RequestSent = "REQUEST_SENT",
        /// The Terms of Service are signed (Meta's spelling: `TERM_OF_…`).
        TermsSigned = "TERM_OF_SERVICE_SIGNED",
    }
}

/// `marketing_messages_onboarding_status` of a business portfolio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessOnboardingStatus {
    /// Terms of Service state.
    pub status: TermsStatus,
    /// Date of the last change, as Meta returns it (`2025-10-07`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
}

/// `owner_business_info` of a WABA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerBusinessInfo {
    /// Business portfolio id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<BusinessId>,
    /// Business portfolio name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// MM API Terms of Service state of that portfolio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marketing_messages_onboarding_status: Option<BusinessOnboardingStatus>,
}

/// A client WABA listed by [`MarketingBusiness::client_wabas_with_status`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientWaba {
    /// WABA id.
    pub id: WabaId,
    /// WABA name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Time zone id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone_id: Option<String>,
    /// Template namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_namespace: Option<String>,
}

/// Result of the partner Intent API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingRequest {
    /// Id of the onboarding request, for tracking.
    pub request_id: String,
}

impl MarketingAccount {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// MM API eligibility of the WABA
    /// (`GET /{WABA_ID}?fields=marketing_messages_onboarding_status`).
    /// `None` when Meta returns no value.
    pub async fn onboarding_status(&self) -> Result<Option<OnboardingStatus>> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            marketing_messages_onboarding_status: Option<OnboardingStatus>,
        }
        let r: Response = self
            .client
            .get(self.waba_id.as_str())
            .query("fields", "marketing_messages_onboarding_status")
            .context("marketing onboarding status response")
            .send()
            .await?;
        Ok(r.marketing_messages_onboarding_status)
    }

    /// The owning business portfolio and its Terms of Service state
    /// (`GET /{WABA_ID}?fields=owner_business_info`).
    pub async fn owner_business_info(&self) -> Result<Option<OwnerBusinessInfo>> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            owner_business_info: Option<OwnerBusinessInfo>,
        }
        let r: Response = self
            .client
            .get(self.waba_id.as_str())
            .query("fields", "owner_business_info")
            .context("owner business info response")
            .send()
            .await?;
        Ok(r.owner_business_info)
    }

    /// Whether marketing templates are blocked on Cloud API `/messages`
    /// (`GET /{WABA_ID}?fields=disable_marketing_messages_on_cloud_api`).
    /// `None` when Meta omits the field; Meta documents `false` as the
    /// default.
    pub async fn cloud_api_marketing_disabled(&self) -> Result<Option<bool>> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            disable_marketing_messages_on_cloud_api: Option<bool>,
        }
        let r: Response = self
            .client
            .get(self.waba_id.as_str())
            .query("fields", "disable_marketing_messages_on_cloud_api")
            .context("cloud api marketing setting response")
            .send()
            .await?;
        Ok(r.disable_marketing_messages_on_cloud_api)
    }

    /// Block (`true`) or allow (`false`, the default) marketing templates on
    /// Cloud API `/messages` (`POST /{WABA_ID}`).
    ///
    /// Only takes effect once the WABA is fully onboarded. While blocked,
    /// `/messages` rejects marketing templates with `131063`
    /// ([`ErrorKind::MarketingNotAllowed`](wa_core::ErrorKind::MarketingNotAllowed)),
    /// and so does the `/marketing_messages` fallback to Cloud API for a WABA
    /// whose Terms of Service are unsigned. Replayed on transient errors:
    /// setting the same value twice is harmless.
    ///
    /// Meta's example answers `{"id": …}`; a Graph node update may also
    /// answer `{"success": …}`, so an explicit `"success": false` is an
    /// error rather than a silent no-op.
    pub async fn set_cloud_api_marketing_disabled(&self, disabled: bool) -> Result<()> {
        const CONTEXT: &str = "cloud api marketing setting response";
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            success: Option<bool>,
        }
        let response = self
            .client
            .post(self.waba_id.as_str())
            .json(&serde_json::json!({ "disable_marketing_messages_on_cloud_api": disabled }))
            .idempotent(true)
            .context(CONTEXT)
            .send_raw()
            .await?;
        let parsed: Response = serde_json::from_slice(&response.body)
            .map_err(|e| Error::decode(CONTEXT, e, &response.body))?;
        if parsed.success == Some(false) {
            return Err(Error::Http {
                status: response.status.as_u16(),
                body_snippet: snippet(&response.body),
            });
        }
        Ok(())
    }
}

impl MarketingBusiness {
    /// The id this API is scoped to.
    pub fn id(&self) -> &BusinessId {
        &self.business_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Terms of Service / intent request state of the portfolio
    /// (`GET /{BUSINESS_ID}?fields=marketing_messages_onboarding_status`,
    /// needs `business_management`).
    pub async fn onboarding_status(&self) -> Result<Option<BusinessOnboardingStatus>> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            marketing_messages_onboarding_status: Option<BusinessOnboardingStatus>,
        }
        let r: Response = self
            .client
            .get(self.business_id.as_str())
            .query("fields", "marketing_messages_onboarding_status")
            .context("business marketing onboarding status response")
            .send()
            .await?;
        Ok(r.marketing_messages_onboarding_status)
    }

    /// Client WABAs shared with this (partner) portfolio whose
    /// `marketing_messages_onboarding_status` is one of `statuses`
    /// (`GET /{BUSINESS_ID}/client_whatsapp_business_accounts?filtering=…`).
    /// Pass `[OnboardingStatus::Eligible]` to find who can be onboarded.
    pub async fn client_wabas_with_status(
        &self,
        statuses: &[OnboardingStatus],
        after: Option<&str>,
    ) -> Result<Page<ClientWaba>> {
        let filtering = serde_json::json!([{
            "field": "marketing_messages_onboarding_status",
            "operator": "IN",
            "value": statuses,
        }]);
        self.client
            .get(&format!(
                "{}/client_whatsapp_business_accounts",
                self.business_id
            ))
            .query_json("filtering", &filtering)
            .query_opt("after", after)
            .context("client whatsapp business accounts response")
            .send()
            .await
    }

    /// Partner Intent API (`POST /{END_BUSINESS_ID}/onboard_partners_to_mm_lite`):
    /// email this end business's admins an invitation to onboard, and set an
    /// OBO migration intent on its on-behalf-of WABAs that lack one.
    ///
    /// Needs a system user token with advanced `whatsapp_business_messaging`
    /// and `whatsapp_business_management`. `solution_id` links a
    /// multi-partner solution; the reference passes it as a query parameter
    /// (the guide shows it in a JSON body — Graph reads either). If any
    /// partner already invited this business, Meta answers `1752041`
    /// ([`ErrorKind::DuplicateOnboarding`](wa_core::ErrorKind::DuplicateOnboarding)):
    /// nothing more to do, all eligible WABAs are included. Onboarding
    /// completion arrives as an `account_update` webhook
    /// (`MM_LITE_TERMS_SIGNED`).
    ///
    /// Not retried on timeouts. The reference says a repeated call returns
    /// the pending request's id; the guide says it fails with "Your business
    /// has already sent this request". Until the two agree, a lost response
    /// is surfaced rather than replayed into a possible error.
    pub async fn request_onboarding(&self, solution_id: Option<&str>) -> Result<OnboardingRequest> {
        self.client
            .post(&format!("{}/onboard_partners_to_mm_lite", self.business_id))
            .query_opt("solution_id", solution_id)
            .context("mm api onboarding request response")
            .send()
            .await
    }
}
