//! Commerce settings of a business phone number: whether the shopping cart
//! is enabled and whether the catalog storefront is visible.
//!
//! Docs:
//! - `catalogs/set-commerce-settings` — enable/disable cart, show/hide
//!   catalog, get settings.
//! - `reference/whatsapp-business-phone-number/commerce-settings-api` —
//!   `GET`/`POST /{phone-number-id}/whatsapp_commerce_settings`.
//! - `catalogs/catalogs-overview`, `catalogs/upload-inventory` — context
//!   only. They document no WhatsApp Graph endpoint: a catalog is created in
//!   Commerce Manager or through Meta's Commerce API (outside the WhatsApp
//!   docs) and connected to the WABA in WhatsApp Manager, so there is nothing
//!   to wrap here. Product, catalog and carousel *messages* belong to
//!   `crate::messages`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use serde::Deserialize;
use wa_core::ids::PhoneNumberId;
use wa_core::{Error, Result};

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

    fn path(&self) -> String {
        format!("{}/whatsapp_commerce_settings", self.phone_number_id)
    }

    /// `GET /{phone-number-id}/whatsapp_commerce_settings`
    /// (`catalogs/set-commerce-settings#get-commerce-settings`).
    ///
    /// Meta answers `{"data": [settings]}`; this returns the single element
    /// and fails with a decode error if the array is empty.
    pub async fn settings(&self) -> Result<CommerceSettings> {
        let resp: DataList<CommerceSettings> = self
            .client
            .get(&self.path())
            .context("commerce settings response")
            .send()
            .await?;
        resp.data.into_iter().next().ok_or_else(|| {
            Error::decode(
                "commerce settings response",
                serde::de::Error::custom("`data` is empty"),
                b"",
            )
        })
    }

    /// `POST /{phone-number-id}/whatsapp_commerce_settings` with the set
    /// fields of `update` as query parameters (the endpoint takes no body).
    ///
    /// Marked idempotent: it sets fields to values, so a replay after a
    /// timeout cannot duplicate an effect.
    pub async fn update_settings(&self, update: &CommerceSettingsUpdate) -> Result<()> {
        self.client
            .post(&self.path())
            .query_opt("is_cart_enabled", update.is_cart_enabled)
            .query_opt("is_catalog_visible", update.is_catalog_visible)
            .idempotent(true)
            .context("update commerce settings response")
            .send_success()
            .await
    }

    /// Enable (`true`) or disable the shopping cart
    /// (`catalogs/set-commerce-settings#enable-or-disable-cart`). Meta's
    /// default is enabled.
    pub async fn set_cart_enabled(&self, enabled: bool) -> Result<()> {
        self.update_settings(&CommerceSettingsUpdate {
            is_cart_enabled: Some(enabled),
            is_catalog_visible: None,
        })
        .await
    }

    /// Show (`true`) or hide the catalog storefront icon
    /// (`catalogs/set-commerce-settings#enable-or-disable-catalog`). Meta's
    /// default is hidden. Hiding it makes existing catalog links show an
    /// "Invalid catalog link" warning.
    pub async fn set_catalog_visible(&self, visible: bool) -> Result<()> {
        self.update_settings(&CommerceSettingsUpdate {
            is_cart_enabled: None,
            is_catalog_visible: Some(visible),
        })
        .await
    }
}

#[derive(Deserialize)]
struct DataList<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

/// Commerce settings of a phone number, as returned by
/// `GET /{phone-number-id}/whatsapp_commerce_settings`.
///
/// Every field is optional in Meta's reference schema, so none is assumed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CommerceSettings {
    /// Id of the commerce settings object (not the phone number id).
    #[serde(default)]
    pub id: Option<String>,
    /// Whether the shopping cart is enabled.
    #[serde(default)]
    pub is_cart_enabled: Option<bool>,
    /// Whether the catalog storefront icon is shown.
    #[serde(default)]
    pub is_catalog_visible: Option<bool>,
}

/// Fields to change with [`Commerce::update_settings`]. `None` leaves a
/// setting as it is (the parameter is not sent).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CommerceSettingsUpdate {
    /// `is_cart_enabled`.
    pub is_cart_enabled: Option<bool>,
    /// `is_catalog_visible`.
    pub is_catalog_visible: Option<bool>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use http::Method;
    use serde_json::json;
    use wa_core::ErrorKind;
    use wa_core::error::TransportError;
    use wa_core::testing::{RecordedBody, ScriptedTransport};

    use super::*;
    use crate::RetryPolicy;

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn get_settings_parses_docs_example() {
        let t = ScriptedTransport::new();
        // catalogs/set-commerce-settings "Sample response".
        t.push_json(
            200,
            json!({"data": [{"is_cart_enabled": true, "is_catalog_visible": true, "id": "727705352028726"}]}),
        );
        let s = client(&t)
            .commerce("106850078877666")
            .settings()
            .await
            .unwrap();
        assert_eq!(
            s,
            CommerceSettings {
                id: Some("727705352028726".into()),
                is_cart_enabled: Some(true),
                is_catalog_visible: Some(true),
            }
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(
            req.path(),
            "/v25.0/106850078877666/whatsapp_commerce_settings"
        );
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn empty_data_is_a_decode_error_not_a_default() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"data": []}));
        let err = client(&t).commerce("1").settings().await.unwrap_err();
        assert!(matches!(err, Error::Decode { .. }), "{err:?}");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn set_cart_enabled_matches_docs_request() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .commerce("106850078877666")
            .set_cart_enabled(true)
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.url.as_str(),
            "https://graph.facebook.com/v25.0/106850078877666/whatsapp_commerce_settings?is_cart_enabled=true"
        );
        assert_eq!(req.body, RecordedBody::Empty);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn set_catalog_visible_matches_docs_request() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .commerce("106850078877666")
            .set_catalog_visible(false)
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.url.as_str(),
            "https://graph.facebook.com/v25.0/106850078877666/whatsapp_commerce_settings?is_catalog_visible=false"
        );
        assert_eq!(req.body, RecordedBody::Empty);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_settings_sends_both_parameters() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .commerce("1")
            .update_settings(&CommerceSettingsUpdate {
                is_cart_enabled: Some(false),
                is_catalog_visible: Some(true),
            })
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.query("is_cart_enabled").as_deref(), Some("false"));
        assert_eq!(req.query("is_catalog_visible").as_deref(), Some("true"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_is_replayed_after_a_timeout_because_it_is_idempotent() {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        t.push_json(200, json!({"success": true}));
        let c = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy {
                max_retries: 1,
                base_delay: Duration::ZERO,
                max_delay: Duration::ZERO,
            })
            .build()
            .unwrap();
        c.commerce("1").set_cart_enabled(true).await.unwrap();
        assert_eq!(t.requests().len(), 2);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn graph_error_is_classified() {
        let t = ScriptedTransport::new();
        t.push_json(
            403,
            json!({"error": {"message": "(#200) Permissions error", "type": "OAuthException", "code": 200}}),
        );
        let err = client(&t)
            .commerce("1")
            .set_cart_enabled(true)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert_eq!(t.remaining(), 0);
    }
}
