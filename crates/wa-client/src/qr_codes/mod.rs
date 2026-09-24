//! QR codes and short links with prefilled messages.
//!
//! Docs: `qr-codes`, `reference/whatsapp-business-phone-number/whatsapp-business-qr-code-*`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use serde::{Deserialize, Serialize};
use wa_core::Result;
use wa_core::ids::PhoneNumberId;
use wa_core::paging::Page;

use crate::Client;

/// Entry point, see [`Client::qr_codes`].
#[derive(Debug, Clone)]
pub struct QrCodes {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`QrCodes`] API for `phone_number_id`.
    pub fn qr_codes(&self, phone_number_id: impl Into<PhoneNumberId>) -> QrCodes {
        QrCodes {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl QrCodes {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Create a new QR code with an optional prefilled message and image format.
    pub async fn create(&self, req: &CreateQrCodeRequest) -> Result<QrCodeResponse> {
        self.client
            .post(&format!("{}/message_qrdls", self.phone_number_id))
            .json(req)
            .context("create QR code response")
            .send::<QrCodeResponse>()
            .await
    }

    /// List all QR codes for this phone number.
    pub async fn list(&self) -> Result<Page<QrCodeListItem>> {
        self.client
            .get(&format!("{}/message_qrdls", self.phone_number_id))
            .context("list QR codes response")
            .send::<Page<QrCodeListItem>>()
            .await
    }

    /// Stream all QR codes for this phone number.
    pub fn list_stream(&self) -> Result<impl futures::Stream<Item = Result<QrCodeListItem>>> {
        Ok(self
            .client
            .get(&format!("{}/message_qrdls", self.phone_number_id))
            .paginate::<QrCodeListItem>())
    }

    /// Get a specific QR code by code.
    pub async fn get(&self, code: &str) -> Result<GetQrCodeResponse> {
        self.client
            .get(&format!("{}/message_qrdls/{}", self.phone_number_id, code))
            .context("get QR code response")
            .send::<GetQrCodeResponse>()
            .await
    }

    /// Update a QR code (change its prefilled message).
    pub async fn update(&self, code: &str, message: &str) -> Result<QrCodeResponse> {
        let req = UpdateQrCodeRequest {
            code: code.to_string(),
            prefilled_message: message.to_string(),
        };
        self.client
            .post(&format!("{}/message_qrdls", self.phone_number_id))
            .json(&req)
            .context("update QR code response")
            .send::<QrCodeResponse>()
            .await
    }

    /// Delete a QR code.
    pub async fn delete(&self, code: &str) -> Result<()> {
        self.client
            .delete(&format!("{}/message_qrdls/{}", self.phone_number_id, code))
            .context("delete QR code response")
            .send_success()
            .await
    }
}

/// Request to create a QR code.
#[derive(Debug, Clone, Serialize)]
pub struct CreateQrCodeRequest {
    /// Prefilled message text (up to 140 characters).
    pub prefilled_message: String,
    /// Image format: `SVG` or `PNG`.
    pub generate_qr_image: String,
}

/// QR code response (create or update).
#[derive(Debug, Clone, Deserialize)]
pub struct QrCodeResponse {
    /// QR code identifier.
    pub code: String,
    /// Prefilled message.
    pub prefilled_message: String,
    /// Deep link URL.
    pub deep_link_url: String,
    /// QR image URL.
    pub qr_image_url: String,
}

/// QR code list item.
#[derive(Debug, Clone, Deserialize)]
pub struct QrCodeListItem {
    /// QR code identifier.
    pub code: String,
    /// Prefilled message.
    pub prefilled_message: String,
    /// Deep link URL.
    pub deep_link_url: String,
}

/// Response for getting a single QR code (wrapped in data array).
#[derive(Debug, Clone, Deserialize)]
pub struct GetQrCodeResponse {
    /// Array containing the QR code.
    pub data: Vec<QrCodeListItem>,
}

/// Request to update a QR code.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateQrCodeRequest {
    /// QR code to update.
    pub code: String,
    /// New prefilled message.
    pub prefilled_message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Client, RetryPolicy};
    use http::Method;
    use serde_json::json;
    use wa_core::testing::ScriptedTransport;

    #[tokio::test]
    async fn create_qr_code() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "code": "4O4YGZEG3RIVE1",
                "prefilled_message": "Cyber Monday",
                "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1",
                "qr_image_url": "https://scontent-iad3-2.xx.fbcdn.net/..."
            }),
        );
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let req = CreateQrCodeRequest {
            prefilled_message: "Cyber Monday".to_string(),
            generate_qr_image: "SVG".to_string(),
        };

        let response = client.qr_codes("123").create(&req).await.unwrap();

        let http_req = t.last_request().unwrap();
        assert_eq!(http_req.method, Method::POST);
        assert_eq!(http_req.path(), "/v25.0/123/message_qrdls");
        assert_eq!(
            http_req.json(),
            Some(json!({
                "prefilled_message": "Cyber Monday",
                "generate_qr_image": "SVG"
            }))
        );

        assert_eq!(response.code, "4O4YGZEG3RIVE1");
        assert_eq!(response.prefilled_message, "Cyber Monday");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn list_qr_codes() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "data": [
                    {
                        "code": "4O4YGZEG3RIVE1",
                        "prefilled_message": "Cyber Monday",
                        "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1"
                    }
                ],
                "paging": {}
            }),
        );
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let response = client.qr_codes("123").list().await.unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/123/message_qrdls");

        assert_eq!(response.data.len(), 1);
        assert_eq!(response.data[0].code, "4O4YGZEG3RIVE1");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn get_qr_code() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "data": [
                    {
                        "code": "4O4YGZEG3RIVE1",
                        "prefilled_message": "Cyber Monday",
                        "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1"
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

        let response = client.qr_codes("123").get("4O4YGZEG3RIVE1").await.unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/123/message_qrdls/4O4YGZEG3RIVE1");

        assert_eq!(response.data.len(), 1);
        assert_eq!(response.data[0].code, "4O4YGZEG3RIVE1");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_qr_code() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "code": "4O4YGZEG3RIVE1",
                "prefilled_message": "Cyber Tuesday",
                "deep_link_url": "https://wa.me/message/4O4YGZEG3RIVE1",
                "qr_image_url": "https://scontent-iad3-2.xx.fbcdn.net/..."
            }),
        );
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let response = client
            .qr_codes("123")
            .update("4O4YGZEG3RIVE1", "Cyber Tuesday")
            .await
            .unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/123/message_qrdls");
        assert_eq!(
            req.json(),
            Some(json!({
                "code": "4O4YGZEG3RIVE1",
                "prefilled_message": "Cyber Tuesday"
            }))
        );

        assert_eq!(response.prefilled_message, "Cyber Tuesday");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn delete_qr_code() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        client
            .qr_codes("123")
            .delete("4O4YGZEG3RIVE1")
            .await
            .unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::DELETE);
        assert_eq!(req.path(), "/v25.0/123/message_qrdls/4O4YGZEG3RIVE1");

        assert_eq!(t.remaining(), 0);
    }
}
