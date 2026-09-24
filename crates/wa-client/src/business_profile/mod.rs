//! Business profile of a phone number: about, address, description, email, vertical, websites, profile picture.
//!
//! Docs: `business-profiles`, `reference/whatsapp-business-phone-number/whatsapp-business-profile-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use serde::{Deserialize, Serialize};
use wa_core::Result;
use wa_core::ids::PhoneNumberId;

use crate::Client;

/// Entry point, see [`Client::business_profile`].
#[derive(Debug, Clone)]
pub struct BusinessProfileApi {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`BusinessProfileApi`] API for `phone_number_id`.
    pub fn business_profile(
        &self,
        phone_number_id: impl Into<PhoneNumberId>,
    ) -> BusinessProfileApi {
        BusinessProfileApi {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl BusinessProfileApi {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get the business profile for this phone number.
    ///
    /// # Arguments
    ///
    /// * `fields` - Comma-separated list of fields to retrieve. If omitted,
    ///   defaults to: `messaging_product`, `about`, `address`, `description`,
    ///   `email`, `profile_picture_url`, `websites`, `vertical`.
    pub async fn get(&self, fields: Option<&str>) -> Result<ProfileResponse> {
        let mut req = self.client.get(&format!(
            "{}/whatsapp_business_profile",
            self.phone_number_id
        ));

        if let Some(f) = fields {
            req = req.query("fields", f);
        }

        req.context("get business profile response")
            .send::<ProfileResponse>()
            .await
    }

    /// Update the business profile for this phone number.
    pub async fn update(&self, profile: &UpdateProfileRequest) -> Result<()> {
        self.client
            .post(&format!(
                "{}/whatsapp_business_profile",
                self.phone_number_id
            ))
            .json(profile)
            .context("update business profile response")
            .send_success()
            .await
    }
}

/// Business profile information.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    /// The messaging service identifier.
    pub messaging_product: String,
    /// About section text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    /// Business address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Business description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Contact email.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// URL of the profile picture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_picture_url: Option<String>,
    /// Associated websites.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websites: Option<Vec<String>>,
    /// Business vertical/category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical: Option<Vertical>,
}

/// Business vertical categories.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Vertical {
    /// Other category.
    Other,
    /// Automotive.
    Auto,
    /// Beauty.
    Beauty,
    /// Apparel.
    Apparel,
    /// Education.
    Edu,
    /// Entertainment.
    Entertain,
    /// Event Planning.
    EventPlan,
    /// Finance.
    Finance,
    /// Grocery.
    Grocery,
    /// Government.
    Govt,
    /// Hotel.
    Hotel,
    /// Health.
    Health,
    /// Nonprofit.
    Nonprofit,
    /// Professional Services.
    #[serde(rename = "PROF_SERVICES")]
    ProfServices,
    /// Retail.
    Retail,
    /// Travel.
    Travel,
    /// Restaurant.
    Restaurant,
    /// Alcohol.
    Alcohol,
    /// Online Gambling.
    #[serde(rename = "ONLINE_GAMBLING")]
    OnlineGambling,
    /// Physical Gambling.
    #[serde(rename = "PHYSICAL_GAMBLING")]
    PhysicalGambling,
    /// OTC Drugs.
    #[serde(rename = "OTC_DRUGS")]
    OtcDrugs,
    /// Unknown vertical.
    #[serde(other)]
    Unknown,
}

/// Response wrapper for get profile.
#[derive(Debug, Deserialize)]
pub struct ProfileResponse {
    /// Array of profile data (typically contains one element).
    pub data: Vec<ProfileData>,
}

/// Profile data wrapper.
#[derive(Debug, Deserialize)]
pub struct ProfileData {
    /// Business profile.
    #[serde(flatten)]
    pub profile: Profile,
}

/// Request to update business profile.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateProfileRequest {
    /// The messaging service identifier.
    pub messaging_product: String,
    /// About section text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    /// Business address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Business description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Contact email.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Handle from Resumable Upload API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_picture_handle: Option<String>,
    /// Associated websites.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websites: Option<Vec<String>>,
    /// Business vertical/category.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical: Option<Vertical>,
}

impl UpdateProfileRequest {
    /// Build an update request.
    pub fn new() -> Self {
        Self {
            messaging_product: "whatsapp".to_string(),
            about: None,
            address: None,
            description: None,
            email: None,
            profile_picture_handle: None,
            websites: None,
            vertical: None,
        }
    }
}

impl Default for UpdateProfileRequest {
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
    async fn get_business_profile() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "data": [
                    {
                        "messaging_product": "whatsapp",
                        "about": "Succulent specialists!",
                        "address": "1 Hacker Way, Menlo Park, CA 94025",
                        "description": "At Lucky Shrub, we specialize in providing a...",
                        "email": "lucky@luckyshrub.com",
                        "profile_picture_url": "https://pps.whatsapp.net/v/t61.24...",
                        "websites": ["https://www.luckyshrub.com/"],
                        "vertical": "RETAIL"
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

        let response = client
            .business_profile("123")
            .get(Some("about,address,email,websites,vertical"))
            .await
            .unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/123/whatsapp_business_profile");
        assert_eq!(
            req.query("fields"),
            Some("about,address,email,websites,vertical".to_string())
        );
        assert_eq!(req.bearer(), Some("TOKEN"));

        assert_eq!(response.data.len(), 1);
        assert_eq!(response.data[0].profile.messaging_product, "whatsapp");
        assert_eq!(
            response.data[0].profile.about,
            Some("Succulent specialists!".to_string())
        );
        assert_eq!(response.data[0].profile.vertical, Some(Vertical::Retail));

        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_business_profile() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let update_req = UpdateProfileRequest {
            messaging_product: "whatsapp".to_string(),
            about: Some("Succulent specialists!".to_string()),
            address: Some("1 Hacker Way, Menlo Park, CA 94025".to_string()),
            description: Some("At Lucky Shrub, we specialize in...".to_string()),
            email: Some("lucky@luckyshrub.com".to_string()),
            profile_picture_handle: None,
            websites: Some(vec!["https://www.luckyshrub.com".to_string()]),
            vertical: Some(Vertical::Retail),
        };

        client
            .business_profile("123")
            .update(&update_req)
            .await
            .unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/123/whatsapp_business_profile");
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({
                "messaging_product": "whatsapp",
                "about": "Succulent specialists!",
                "address": "1 Hacker Way, Menlo Park, CA 94025",
                "description": "At Lucky Shrub, we specialize in...",
                "email": "lucky@luckyshrub.com",
                "websites": ["https://www.luckyshrub.com"],
                "vertical": "RETAIL"
            }))
        );

        assert_eq!(t.remaining(), 0);
    }
}
