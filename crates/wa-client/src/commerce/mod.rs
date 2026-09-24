//! Commerce settings (cart, catalog visibility) and catalog connection to a WABA. Product *messages* live in `messages`.
//!
//! Docs: `catalogs/*`, `reference/whatsapp-business-phone-number/commerce-settings-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use serde::{Deserialize, Serialize};
use wa_core::Result;
use wa_core::ids::PhoneNumberId;

use crate::Client;

/// Entry point, see [`Client::commerce`].
#[derive(Debug, Clone)]
pub struct Commerce {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Commerce`] API for `phone_number_id`.
    pub fn commerce(&self, phone_number_id: impl Into<PhoneNumberId>) -> Commerce {
        Commerce {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl Commerce {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get commerce settings for this phone number.
    pub async fn get(&self) -> Result<CommerceSettingsResponse> {
        self.client
            .get(&format!(
                "{}/whatsapp_commerce_settings",
                self.phone_number_id
            ))
            .context("get commerce settings response")
            .send::<CommerceSettingsResponse>()
            .await
    }

    /// Update commerce settings for this phone number.
    pub async fn update(&self, settings: &UpdateCommerceSettingsRequest) -> Result<()> {
        let mut req = self.client.post(&format!(
            "{}/whatsapp_commerce_settings",
            self.phone_number_id
        ));

        if let Some(cart_enabled) = settings.is_cart_enabled {
            req = req.query("is_cart_enabled", cart_enabled.to_string().as_str());
        }

        if let Some(catalog_visible) = settings.is_catalog_visible {
            req = req.query("is_catalog_visible", catalog_visible.to_string().as_str());
        }

        req.context("update commerce settings response")
            .send_success()
            .await
    }
}

/// Commerce settings response.
#[derive(Debug, Clone, Deserialize)]
pub struct CommerceSettingsResponse {
    /// Array of commerce settings (typically contains one element).
    pub data: Vec<CommerceSettings>,
}

/// Commerce settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommerceSettings {
    /// Whether cart is enabled.
    pub is_cart_enabled: bool,
    /// Whether catalog is visible.
    pub is_catalog_visible: bool,
    /// ID of the phone number.
    pub id: String,
}

/// Request to update commerce settings.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateCommerceSettingsRequest {
    /// Enable or disable cart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_cart_enabled: Option<bool>,
    /// Show or hide catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_catalog_visible: Option<bool>,
}

impl UpdateCommerceSettingsRequest {
    /// Create a new update request.
    pub fn new() -> Self {
        Self {
            is_cart_enabled: None,
            is_catalog_visible: None,
        }
    }

    /// Set whether cart is enabled.
    pub fn with_cart_enabled(mut self, enabled: bool) -> Self {
        self.is_cart_enabled = Some(enabled);
        self
    }

    /// Set whether catalog is visible.
    pub fn with_catalog_visible(mut self, visible: bool) -> Self {
        self.is_catalog_visible = Some(visible);
        self
    }
}

impl Default for UpdateCommerceSettingsRequest {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Client, RetryPolicy};
    use http::Method;
    use serde_json::json;
    use wa_core::testing::ScriptedTransport;

    #[tokio::test]
    async fn get_commerce_settings() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "data": [
                    {
                        "is_cart_enabled": true,
                        "is_catalog_visible": true,
                        "id": "727705352028726"
                    }
                ]
            }),
        );
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let response = client.commerce("123").get().await.unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/123/whatsapp_commerce_settings");

        assert_eq!(response.data.len(), 1);
        assert!(response.data[0].is_cart_enabled);
        assert!(response.data[0].is_catalog_visible);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_commerce_settings() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let settings = UpdateCommerceSettingsRequest::new()
            .with_cart_enabled(true)
            .with_catalog_visible(true);

        client.commerce("123").update(&settings).await.unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/123/whatsapp_commerce_settings");
        assert_eq!(req.query("is_cart_enabled"), Some("true".to_string()));
        assert_eq!(req.query("is_catalog_visible"), Some("true".to_string()));

        assert_eq!(t.remaining(), 0);
    }
}
