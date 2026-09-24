//! `POST /{PHONE_NUMBER_ID}/marketing_messages`.
//!
//! Sources: `marketing-messages/send-marketing-messages` (body, options,
//! BSUID addressing, response), `reference/whatsapp-business-phone-number/marketing-messages-api-for-whatsapp`
//! (schema, `message_status`), `marketing-messages/pricing` (per-message
//! max-price multiplier).

use serde::Serialize;
use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::PhoneNumberId;
use wa_core::recipient::Recipient;

use super::wire_enum;
use crate::Client;
use crate::templates::TemplateMessage;

/// Marketing Messages API for one business phone number. See
/// [`Client::marketing`].
#[derive(Debug, Clone)]
pub struct Marketing {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Marketing`] API for `phone_number_id`: send marketing templates
    /// through `/marketing_messages`. The number must also be registered on
    /// Cloud API.
    pub fn marketing(&self, phone_number_id: impl Into<PhoneNumberId>) -> Marketing {
        Marketing {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

wire_enum! {
    /// What happens when the business has not finished MM API onboarding.
    pub enum ProductPolicy {
        /// Deliver through Cloud API instead (Meta's default).
        CloudApiFallback = "CLOUD_API_FALLBACK",
        /// Fail rather than fall back to Cloud API.
        Strict = "STRICT",
    }
}

/// The optional, MM-API-only parts of a send.
///
/// There is no per-message time-to-live: for marketing templates the TTL is
/// a property of the *template* (set when creating or editing it, 12 hours
/// to 30 days), not of the send.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarketingOptions {
    /// `product_policy`: whether to fall back to Cloud API when MM API
    /// onboarding is incomplete. `None` = Meta's default
    /// ([`ProductPolicy::CloudApiFallback`]).
    pub product_policy: Option<ProductPolicy>,
    /// `message_activity_sharing`: share this message's activity (reads,
    /// clicks) with Meta for optimization. `None` = the WABA's setting.
    pub message_activity_sharing: Option<bool>,
    /// `bid_spec.per_message_bid_multiplier` (max price beta): scales the
    /// template's max price for this message only; `1.5` = +50 %. Must be
    /// positive. Meta marks it subject to change during the beta and
    /// recommends adjusting the template's max price for bulk changes.
    pub per_message_bid_multiplier: Option<f64>,
}

impl MarketingOptions {
    /// No options: Meta's defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Never fall back to Cloud API ([`ProductPolicy::Strict`]).
    #[must_use]
    pub fn strict(mut self) -> Self {
        self.product_policy = Some(ProductPolicy::Strict);
        self
    }

    /// Set `product_policy`.
    #[must_use]
    pub fn product_policy(mut self, policy: ProductPolicy) -> Self {
        self.product_policy = Some(policy);
        self
    }

    /// Set `message_activity_sharing` for this message.
    #[must_use]
    pub fn message_activity_sharing(mut self, share: bool) -> Self {
        self.message_activity_sharing = Some(share);
        self
    }

    /// Set the per-message max-price multiplier.
    #[must_use]
    pub fn bid_multiplier(mut self, multiplier: f64) -> Self {
        self.per_message_bid_multiplier = Some(multiplier);
        self
    }

    fn validate(&self) -> Result<(), ValidationError> {
        if let Some(m) = self.per_message_bid_multiplier
            && !(m.is_finite() && m > 0.0)
        {
            return Err(ValidationError::new(
                "bid_spec.per_message_bid_multiplier",
                "must be a positive number",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct BidSpec {
    per_message_bid_multiplier: f64,
}

#[derive(Serialize)]
struct MarketingMessageBody<'a> {
    messaging_product: &'static str,
    #[serde(flatten)]
    recipient: &'a Recipient,
    #[serde(rename = "type")]
    kind: &'static str,
    template: &'a TemplateMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    product_policy: Option<&'a ProductPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_activity_sharing: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bid_spec: Option<BidSpec>,
}

// A marketing send answers exactly like a Cloud API send; one set of types
// serves both.
pub use crate::messages::{MessageStatus, SendResponse, SentContact, SentMessage};

impl Marketing {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Send a marketing template (`POST /{PHONE_NUMBER_ID}/marketing_messages`).
    ///
    /// Only approved `MARKETING` templates are accepted: anything else fails
    /// with [`ErrorKind::MarketingNotAllowed`](wa_core::ErrorKind::MarketingNotAllowed)
    /// (`131055`, `134100`). A template created or unused for 7+ days needs
    /// ~10 minutes of ad sync first
    /// ([`ErrorKind::TemplateSyncing`](wa_core::ErrorKind::TemplateSyncing), `134101`).
    ///
    /// `to` may be a phone number, a BSUID or both (the phone number wins).
    /// Sending by BSUID alone turns off delivery optimization for that
    /// message, and a template with max pricing cannot go to a BSUID
    /// (`131062`). A [`Recipient::Group`] is sent as
    /// `recipient_type: "group"` without a local check: the guide mentions
    /// group ids in `to`, but the reference only documents `individual`, so
    /// Meta's answer is the authority.
    ///
    /// Not retried on timeouts: the message may already be on its way.
    pub async fn send(
        &self,
        to: &Recipient,
        template: &TemplateMessage,
        options: &MarketingOptions,
    ) -> Result<SendResponse> {
        if template.name.trim().is_empty() {
            return Err(ValidationError::new("template.name", "is required").into());
        }
        options.validate()?;
        let body = MarketingMessageBody {
            messaging_product: "whatsapp",
            recipient: to,
            kind: "template",
            template,
            product_policy: options.product_policy.as_ref(),
            message_activity_sharing: options.message_activity_sharing,
            bid_spec: options
                .per_message_bid_multiplier
                .map(|per_message_bid_multiplier| BidSpec {
                    per_message_bid_multiplier,
                }),
        };
        self.client
            .post_at(&[self.phone_number_id.as_str(), "marketing_messages"])
            .json(&body)
            .context("send marketing message response")
            .send()
            .await
    }
}
