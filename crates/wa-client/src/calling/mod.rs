//! Calling API signalling: call settings, call permissions, business-initiated calls (connect, pre-accept, accept, reject, terminate).
//!
//! Docs: `calling/*`, `reference/whatsapp-business-phone-number/calling-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use serde::{Deserialize, Serialize};
use wa_core::Result;
use wa_core::ids::{CallId, PhoneNumberId, WaId};

use crate::Client;

/// Entry point, see [`Client::calling`].
#[derive(Debug, Clone)]
pub struct Calling {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Calling`] API for `phone_number_id`.
    pub fn calling(&self, phone_number_id: impl Into<PhoneNumberId>) -> Calling {
        Calling {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl Calling {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get call settings for this phone number.
    pub async fn get_settings(&self) -> Result<CallSettingsResponse> {
        self.client
            .get(&format!("{}/whatsapp_call_settings", self.phone_number_id))
            .context("get call settings response")
            .send::<CallSettingsResponse>()
            .await
    }

    /// Update call settings for this phone number.
    pub async fn update_settings(&self, settings: &UpdateCallSettingsRequest) -> Result<()> {
        self.client
            .post(&format!("{}/whatsapp_call_settings", self.phone_number_id))
            .json(settings)
            .context("update call settings response")
            .send_success()
            .await
    }

    /// Connect to initiate a business-initiated call with SDP offer.
    pub async fn connect(&self, wa_id: &WaId, sdp: &str) -> Result<ConnectResponse> {
        let req = ConnectRequest {
            messaging_product: "whatsapp".to_string(),
            to: wa_id.to_string(),
            custom_payload: Some(CustomPayload {
                method: "WEBRTC_ANSWER".to_string(),
                sdp: sdp.to_string(),
            }),
        };

        self.client
            .post(&format!("{}/calls", self.phone_number_id))
            .json(&req)
            .context("connect call response")
            .send::<ConnectResponse>()
            .await
    }

    /// Accept a call by providing SDP answer.
    pub async fn accept(&self, call_id: &CallId, sdp: &str) -> Result<()> {
        let req = AcceptRequest {
            messaging_product: "whatsapp".to_string(),
            custom_payload: Some(CustomPayload {
                method: "WEBRTC_ANSWER".to_string(),
                sdp: sdp.to_string(),
            }),
        };

        self.client
            .post(&format!("{}/accept", call_id))
            .json(&req)
            .context("accept call response")
            .send_success()
            .await
    }

    /// Reject a call.
    pub async fn reject(&self, call_id: &CallId) -> Result<()> {
        let req = RejectRequest {
            messaging_product: "whatsapp".to_string(),
        };

        self.client
            .post(&format!("{}/reject", call_id))
            .json(&req)
            .context("reject call response")
            .send_success()
            .await
    }

    /// Terminate a call.
    pub async fn terminate(&self, call_id: &CallId) -> Result<()> {
        let req = TerminateRequest {
            messaging_product: "whatsapp".to_string(),
        };

        self.client
            .post(&format!("{}/terminate", call_id))
            .json(&req)
            .context("terminate call response")
            .send_success()
            .await
    }

    /// Pre-accept a call (acknowledge receipt before processing).
    pub async fn pre_accept(&self, call_id: &CallId) -> Result<()> {
        let req = PreAcceptRequest {
            messaging_product: "whatsapp".to_string(),
        };

        self.client
            .post(&format!("{}/pre_accept", call_id))
            .json(&req)
            .context("pre-accept call response")
            .send_success()
            .await
    }
}

/// Call settings response.
#[derive(Debug, Clone, Deserialize)]
pub struct CallSettingsResponse {
    /// Array of call settings (typically contains one element).
    pub data: Vec<CallSettings>,
}

/// Call settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CallSettings {
    /// Whether calling is enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_enabled: Option<bool>,
    /// Call icon visibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_icon_visible: Option<bool>,
    /// Call icon display type (e.g., "VISIBLE" or "HIDDEN").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_icon_display: Option<String>,
}

/// Request to update call settings.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateCallSettingsRequest {
    /// Enable or disable calling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_enabled: Option<bool>,
    /// Make call icon visible or hidden.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_icon_visible: Option<bool>,
    /// Call icon display type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_icon_display: Option<String>,
}

/// Request to connect (initiate) a call.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectRequest {
    /// Messaging product.
    pub messaging_product: String,
    /// Recipient WhatsApp ID.
    pub to: String,
    /// Custom payload with SDP.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_payload: Option<CustomPayload>,
}

/// Custom payload containing SDP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomPayload {
    /// Method (e.g., "WEBRTC_OFFER" or "WEBRTC_ANSWER").
    pub method: String,
    /// SDP string (opaque to us).
    pub sdp: String,
}

/// Response from connecting a call.
#[derive(Debug, Clone, Deserialize)]
pub struct ConnectResponse {
    /// Call ID.
    pub call_id: CallId,
}

/// Request to accept a call.
#[derive(Debug, Clone, Serialize)]
pub struct AcceptRequest {
    /// Messaging product.
    pub messaging_product: String,
    /// Custom payload with SDP answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_payload: Option<CustomPayload>,
}

/// Request to reject a call.
#[derive(Debug, Clone, Serialize)]
pub struct RejectRequest {
    /// Messaging product.
    pub messaging_product: String,
}

/// Request to terminate a call.
#[derive(Debug, Clone, Serialize)]
pub struct TerminateRequest {
    /// Messaging product.
    pub messaging_product: String,
}

/// Request to pre-accept a call.
#[derive(Debug, Clone, Serialize)]
pub struct PreAcceptRequest {
    /// Messaging product.
    pub messaging_product: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Client, RetryPolicy};
    use http::Method;
    use serde_json::json;
    use wa_core::testing::ScriptedTransport;

    #[tokio::test]
    async fn get_call_settings() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "data": [
                    {
                        "call_enabled": true,
                        "call_icon_visible": true
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

        let response = client.calling("123").get_settings().await.unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/123/whatsapp_call_settings");

        assert_eq!(response.data.len(), 1);
        assert!(response.data[0].call_enabled.unwrap());
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_call_settings() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let settings = UpdateCallSettingsRequest {
            call_enabled: Some(true),
            call_icon_visible: Some(true),
            call_icon_display: None,
        };

        client
            .calling("123")
            .update_settings(&settings)
            .await
            .unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/123/whatsapp_call_settings");

        assert_eq!(t.remaining(), 0);
    }
}
