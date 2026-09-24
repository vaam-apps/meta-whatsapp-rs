//! Business profile of a phone number: about, address, description, email,
//! vertical, websites, profile picture.
//!
//! Docs:
//! - `business-profiles` — get and update through the phone number, field
//!   notes (address limit, updatable verticals).
//! - `reference/whatsapp-business-phone-number/whatsapp-business-profile-api`
//!   — `GET`/`POST /{phone-number-id}/whatsapp_business_profile`.
//! - `reference/whatsapp-business-profile/whatsapp-business-profile-node-api`
//!   — `GET`/`POST /{whatsapp-business-profile-id}`, reached with
//!   [`Client::business_profile_node`].
//!
//! Where the reference schema and the guide's example disagree, the example
//! wins (see the `meta-docs` skill): the phone-number `GET` returns profile
//! fields flat in `data[0]` in the example but under
//! `data[0].business_profile` in the schema, so both shapes are accepted.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use serde::{Deserialize, Serialize};
use wa_core::error::{ValidationError, snippet};
use wa_core::ids::{PhoneNumberId, UploadHandle};
use wa_core::{Error, Result};

use crate::Client;
use crate::request::decode_json;

/// Maximum length of `address`, in characters (`business-profiles`, "Field
/// notes").
pub const ADDRESS_MAX_CHARS: usize = 256;

/// Entry point, see [`Client::business_profile`].
#[derive(Debug, Clone)]
pub struct BusinessProfile {
    client: Client,
    phone_number_id: PhoneNumberId,
}

/// A business profile addressed by its own id, see
/// [`Client::business_profile_node`].
#[derive(Debug, Clone)]
pub struct BusinessProfileNode {
    client: Client,
    profile_id: String,
}

impl Client {
    /// [`BusinessProfile`] API for `phone_number_id`.
    pub fn business_profile(&self, phone_number_id: impl Into<PhoneNumberId>) -> BusinessProfile {
        BusinessProfile {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }

    /// [`BusinessProfileNode`] API for a WhatsApp Business Profile id (not
    /// a phone number id). `wa-core` has no newtype for this id yet, hence
    /// the plain string.
    pub fn business_profile_node(&self, profile_id: impl Into<String>) -> BusinessProfileNode {
        BusinessProfileNode {
            client: self.clone(),
            profile_id: profile_id.into(),
        }
    }
}

impl BusinessProfile {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Path segments; the id stays one segment whatever it contains.
    fn segments(&self) -> [&str; 2] {
        [self.phone_number_id.as_str(), "whatsapp_business_profile"]
    }

    /// `GET /{phone-number-id}/whatsapp_business_profile`
    /// (`business-profiles#get-your-profile-via-the-api`).
    ///
    /// `fields` selects what to return; empty sends no `fields` parameter
    /// and Meta returns its defaults (every field this endpoint knows).
    /// [`ProfileField::Id`] and [`ProfileField::VerifiedName`] exist only
    /// on the profile node and are rejected here.
    pub async fn get(&self, fields: &[ProfileField]) -> Result<Profile> {
        const CONTEXT: &str = "get business profile response";
        if let Some(f) = fields.iter().find(|f| f.node_only()) {
            return Err(ValidationError::new(
                "fields",
                format!(
                    "`{}` is only available on the profile node (Client::business_profile_node)",
                    f.as_str()
                ),
            )
            .into());
        }
        let resp: DataList<serde_json::Value> = self
            .client
            .get_at(&self.segments())
            .query_opt("fields", fields_param(fields))
            .context(CONTEXT)
            .send()
            .await?;
        let entry = resp.data.into_iter().next().ok_or_else(|| {
            Error::decode(CONTEXT, serde::de::Error::custom("`data` is empty"), b"")
        })?;
        profile_from_entry(entry).map_err(|e| Error::decode(CONTEXT, e, b"(data[0])"))
    }

    /// `POST /{phone-number-id}/whatsapp_business_profile`
    /// (`business-profiles#update-your-profile-via-the-api`). Only the set
    /// fields of `update` are sent.
    ///
    /// Marked idempotent: it sets fields to values.
    pub async fn update(&self, update: &ProfileUpdate) -> Result<()> {
        update.validate()?;
        self.client
            .post_at(&self.segments())
            .json(&update.wire())
            .idempotent(true)
            .context("update business profile response")
            .send_success()
            .await
    }
}

impl BusinessProfileNode {
    /// The profile id this API is scoped to.
    pub fn id(&self) -> &str {
        &self.profile_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The id is the whole path, so it goes through the segment builder: an
    /// id containing `/` stays one (encoded) segment instead of addressing
    /// another object, and an empty one fails before any request.
    fn segments(&self) -> [&str; 1] {
        [self.profile_id.as_str()]
    }

    /// `GET /{whatsapp-business-profile-id}` (profile node API). `fields`
    /// works as in [`BusinessProfile::get`], and may include
    /// [`ProfileField::Id`] and [`ProfileField::VerifiedName`].
    pub async fn get(&self, fields: &[ProfileField]) -> Result<Profile> {
        self.client
            .get_at(&self.segments())
            .query_opt("fields", fields_param(fields))
            .context("get business profile node response")
            .send()
            .await
    }

    /// `POST /{whatsapp-business-profile-id}` (profile node API). Returns
    /// the profile id Meta echoes; a `"success": false` answer is an
    /// [`Error::Http`].
    ///
    /// Marked idempotent: it sets fields to values.
    pub async fn update(&self, update: &ProfileUpdate) -> Result<ProfileNodeUpdated> {
        const CONTEXT: &str = "update business profile node response";
        update.validate()?;
        let resp = self
            .client
            .post_at(&self.segments())
            .json(&update.wire())
            .idempotent(true)
            .context(CONTEXT)
            .send_raw()
            .await?;
        let updated: ProfileNodeUpdated = decode_json(CONTEXT, &resp.body)?;
        if updated.success == Some(false) {
            return Err(Error::Http {
                status: resp.status.as_u16(),
                body_snippet: snippet(&resp.body),
            });
        }
        Ok(updated)
    }
}

fn fields_param(fields: &[ProfileField]) -> Option<String> {
    if fields.is_empty() {
        return None;
    }
    Some(
        fields
            .iter()
            .map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Accept `data[0]` flat (guide example) or wrapped in `business_profile`
/// (reference schema). Decided by the key, not by trial parsing: an
/// untagged fallback would turn a malformed wrapped profile into an empty
/// flat one.
fn profile_from_entry(entry: serde_json::Value) -> serde_json::Result<Profile> {
    match entry {
        serde_json::Value::Object(mut map) => match map.remove("business_profile") {
            Some(inner) => serde_json::from_value(inner),
            None => serde_json::from_value(serde_json::Value::Object(map)),
        },
        other => serde_json::from_value(other),
    }
}

#[derive(Deserialize)]
struct DataList<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
}

/// A field that can be requested with `?fields=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProfileField {
    /// `messaging_product`.
    MessagingProduct,
    /// `about`.
    About,
    /// `address`.
    Address,
    /// `description`.
    Description,
    /// `email`.
    Email,
    /// `profile_picture_url`.
    ProfilePictureUrl,
    /// `websites`.
    Websites,
    /// `vertical`.
    Vertical,
    /// `id` — profile node only.
    Id,
    /// `verified_name` — profile node only.
    VerifiedName,
}

impl ProfileField {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MessagingProduct => "messaging_product",
            Self::About => "about",
            Self::Address => "address",
            Self::Description => "description",
            Self::Email => "email",
            Self::ProfilePictureUrl => "profile_picture_url",
            Self::Websites => "websites",
            Self::Vertical => "vertical",
            Self::Id => "id",
            Self::VerifiedName => "verified_name",
        }
    }

    fn node_only(self) -> bool {
        matches!(self, Self::Id | Self::VerifiedName)
    }
}

/// Industry of the business (`WhatsAppBusinessVertical`). The union of the
/// values in both reference pages; an unlisted value round-trips through
/// [`Vertical::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Vertical {
    /// `ALCOHOL`.
    Alcohol,
    /// `APPAREL`.
    Apparel,
    /// `AUTO`.
    Auto,
    /// `BEAUTY`.
    Beauty,
    /// `EDU`.
    Edu,
    /// `ENTERTAIN`.
    Entertain,
    /// `EVENT_PLAN`.
    EventPlan,
    /// `FINANCE`.
    Finance,
    /// `GOVT`.
    Govt,
    /// `GROCERY`.
    Grocery,
    /// `HEALTH`.
    Health,
    /// `HOTEL`.
    Hotel,
    /// `MATRIMONY_SERVICE` (profile node reference only).
    MatrimonyService,
    /// `NONPROFIT`.
    Nonprofit,
    /// `NOT_A_BIZ` — can be read, cannot be set.
    NotABiz,
    /// `ONLINE_GAMBLING`.
    OnlineGambling,
    /// `OTHER` — a real vertical, not a catch-all.
    Other,
    /// `OTC_DRUGS`.
    OtcDrugs,
    /// `PHYSICAL_GAMBLING`.
    PhysicalGambling,
    /// `PROF_SERVICES`.
    ProfServices,
    /// `RESTAURANT`.
    Restaurant,
    /// `RETAIL`.
    Retail,
    /// `TRAVEL`.
    Travel,
    /// `UNDEFINED` — can be read, cannot be set.
    Undefined,
    /// A value this crate does not know yet, kept verbatim.
    #[serde(untagged)]
    Unknown(String),
}

/// A business profile as returned by either `GET` (`WhatsAppBusinessProfile`
/// / `WhatsAppBusinessProfileNode`). Every field is optional because
/// `?fields=` decides what comes back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Profile {
    /// Profile id (profile node only).
    #[serde(default)]
    pub id: Option<String>,
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// "About" text.
    #[serde(default)]
    pub about: Option<String>,
    /// Free-form address.
    #[serde(default)]
    pub address: Option<String>,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// Contact email.
    #[serde(default)]
    pub email: Option<String>,
    /// Profile picture URL.
    #[serde(default)]
    pub profile_picture_url: Option<String>,
    /// Websites.
    #[serde(default)]
    pub websites: Vec<String>,
    /// Industry.
    #[serde(default)]
    pub vertical: Option<Vertical>,
    /// Verified business name (profile node only).
    #[serde(default)]
    pub verified_name: Option<String>,
}

/// Fields to change with [`BusinessProfile::update`] or
/// [`BusinessProfileNode::update`] (`WhatsAppBusinessProfileUpdateRequest`,
/// minus `messaging_product`, which is always `whatsapp` and added for you).
/// `None` fields are not sent and stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProfileUpdate {
    /// "About" text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    /// Free-form address, at most 256 characters; not checked against any
    /// geographic database.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Contact email.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Handle (`h`) of a picture uploaded with the Resumable Upload API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_picture_handle: Option<UploadHandle>,
    /// Websites. Sent as a JSON array, as the reference schema types it
    /// (the guide's example sends a JSON-encoded string instead).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websites: Option<Vec<String>>,
    /// Industry; `UNDEFINED` and `NOT_A_BIZ` cannot be set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical: Option<Vertical>,
}

impl ProfileUpdate {
    fn validate(&self) -> Result<()> {
        if let Some(address) = &self.address {
            let len = address.chars().count();
            if len > ADDRESS_MAX_CHARS {
                return Err(ValidationError::new(
                    "address",
                    format!("must be at most {ADDRESS_MAX_CHARS} characters, got {len}"),
                )
                .into());
            }
        }
        if let Some(v @ (Vertical::Undefined | Vertical::NotABiz)) = &self.vertical {
            return Err(ValidationError::new(
                "vertical",
                format!("{v:?} cannot be set (business-profiles, field notes)"),
            )
            .into());
        }
        Ok(())
    }

    fn wire(&self) -> WireUpdate<'_> {
        WireUpdate {
            messaging_product: "whatsapp",
            update: self,
        }
    }
}

#[derive(Serialize)]
struct WireUpdate<'a> {
    messaging_product: &'static str,
    #[serde(flatten)]
    update: &'a ProfileUpdate,
}

/// Answer of [`BusinessProfileNode::update`]
/// (`WhatsAppBusinessProfileUpdateResponse`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProfileNodeUpdated {
    /// The updated profile's id.
    #[serde(default)]
    pub id: Option<String>,
    /// `true` on success.
    #[serde(default)]
    pub success: Option<bool>,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use http::Method;
    use serde_json::json;
    use wa_core::ErrorKind;
    use wa_core::error::TransportError;
    use wa_core::testing::ScriptedTransport;

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

    fn validation_field(err: &Error) -> &str {
        match err {
            Error::Validation(v) => &v.field,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    fn lucky_shrub() -> Profile {
        Profile {
            messaging_product: Some("whatsapp".into()),
            about: Some("Succulent specialists!".into()),
            address: Some("1 Hacker Way, Menlo Park, CA 94025".into()),
            description: Some("At Lucky Shrub, we specialize in providing a...".into()),
            email: Some("lucky@luckyshrub.com".into()),
            profile_picture_url: Some("https://pps.whatsapp.net/v/t61.24...".into()),
            websites: vec!["https://www.luckyshrub.com/".into()],
            vertical: Some(Vertical::Retail),
            ..Profile::default()
        }
    }

    #[tokio::test]
    async fn get_matches_docs_example() {
        use ProfileField as F;
        let t = ScriptedTransport::new();
        // business-profiles "Get your profile via the API" example response.
        t.push_json(
            200,
            json!({
                "data": [{
                    "about": "Succulent specialists!",
                    "address": "1 Hacker Way, Menlo Park, CA 94025",
                    "description": "At Lucky Shrub, we specialize in providing a...",
                    "email": "lucky@luckyshrub.com",
                    "profile_picture_url": "https://pps.whatsapp.net/v/t61.24...",
                    "websites": ["https://www.luckyshrub.com/"],
                    "vertical": "RETAIL",
                    "messaging_product": "whatsapp"
                }]
            }),
        );
        let profile = client(&t)
            .business_profile("106540352242922")
            .get(&[
                F::About,
                F::Address,
                F::Description,
                F::Email,
                F::ProfilePictureUrl,
                F::Websites,
                F::Vertical,
            ])
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(
            req.path(),
            "/v25.0/106540352242922/whatsapp_business_profile"
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.query("fields").as_deref(),
            Some("about,address,description,email,profile_picture_url,websites,vertical")
        );
        assert_eq!(profile, lucky_shrub());
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn get_accepts_the_reference_schema_wrapper_and_default_fields() {
        let t = ScriptedTransport::new();
        // Reference schema: data[].business_profile.
        t.push_json(
            200,
            json!({"data": [{"business_profile": {"messaging_product": "whatsapp", "about": "Hi", "vertical": "SOMETHING_NEW"}}]}),
        );
        let profile = client(&t).business_profile("1").get(&[]).await.unwrap();
        assert_eq!(t.last_request().unwrap().url.query(), None);
        assert_eq!(profile.about.as_deref(), Some("Hi"));
        assert_eq!(
            profile.vertical,
            Some(Vertical::Unknown("SOMETHING_NEW".into()))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn malformed_wrapped_profile_is_an_error_not_an_empty_profile() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"business_profile": {"websites": "not-an-array"}}]}),
        );
        let err = client(&t).business_profile("1").get(&[]).await.unwrap_err();
        assert!(matches!(err, Error::Decode { .. }), "{err:?}");
        t.push_json(200, json!({"data": []}));
        let err = client(&t).business_profile("1").get(&[]).await.unwrap_err();
        assert!(matches!(err, Error::Decode { .. }), "{err:?}");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_matches_docs_example() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .business_profile("106540352242922")
            .update(&ProfileUpdate {
                about: Some("Succulent specialists!".into()),
                address: Some("1 Hacker Way, Menlo Park, CA 94025".into()),
                description: Some("At Lucky Shrub, we specialize in providing a diverse range of high-quality succulents to suit your needs. From rare and exotic varieties to timeless classics, our collection has something for everyone.".into()),
                email: Some("lucky@luckyshrub.com".into()),
                profile_picture_handle: Some("4::aW...".into()),
                websites: Some(vec!["https://www.luckyshrub.com".into()]),
                vertical: Some(Vertical::Retail),
            })
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            "/v25.0/106540352242922/whatsapp_business_profile"
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({
                "about": "Succulent specialists!",
                "address": "1 Hacker Way, Menlo Park, CA 94025",
                "description": "At Lucky Shrub, we specialize in providing a diverse range of high-quality succulents to suit your needs. From rare and exotic varieties to timeless classics, our collection has something for everyone.",
                "email": "lucky@luckyshrub.com",
                "messaging_product": "whatsapp",
                "profile_picture_handle": "4::aW...",
                "websites": ["https://www.luckyshrub.com"],
                "vertical": "RETAIL"
            }))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_sends_only_set_fields_and_is_replayed_after_timeout() {
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
        c.business_profile("1")
            .update(&ProfileUpdate {
                about: Some("x".into()),
                ..ProfileUpdate::default()
            })
            .await
            .unwrap();
        let reqs = t.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(
            reqs[1].json(),
            Some(json!({"messaging_product": "whatsapp", "about": "x"}))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_success_false_is_an_error() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": false}));
        let err = client(&t)
            .business_profile("1")
            .update(&ProfileUpdate::default())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Http { status: 200, .. }), "{err:?}");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn graph_error_is_classified() {
        let t = ScriptedTransport::new();
        // ...whatsapp-business-profile-api 401 example.
        t.push_json(
            401,
            json!({"error": {"message": "Invalid OAuth access token", "type": "OAuthException", "code": 190, "error_subcode": 463, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
        );
        let err = client(&t).business_profile("1").get(&[]).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Authentication);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn address_over_256_chars_is_rejected_locally() {
        let t = ScriptedTransport::new();
        let ok = ProfileUpdate {
            address: Some("ü".repeat(256)),
            ..ProfileUpdate::default()
        };
        assert!(ok.validate().is_ok());
        let err = client(&t)
            .business_profile("1")
            .update(&ProfileUpdate {
                address: Some("a".repeat(257)),
                ..ProfileUpdate::default()
            })
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "address");
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn undefined_and_not_a_biz_verticals_cannot_be_set() {
        let t = ScriptedTransport::new();
        for v in [Vertical::Undefined, Vertical::NotABiz] {
            let err = client(&t)
                .business_profile("1")
                .update(&ProfileUpdate {
                    vertical: Some(v),
                    ..ProfileUpdate::default()
                })
                .await
                .unwrap_err();
            assert_eq!(validation_field(&err), "vertical");
            let err = client(&t)
                .business_profile_node("5")
                .update(&ProfileUpdate {
                    vertical: Some(Vertical::NotABiz),
                    ..ProfileUpdate::default()
                })
                .await
                .unwrap_err();
            assert_eq!(validation_field(&err), "vertical");
        }
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn node_only_fields_are_rejected_on_the_phone_number_edge() {
        let t = ScriptedTransport::new();
        for f in [ProfileField::Id, ProfileField::VerifiedName] {
            let err = client(&t)
                .business_profile("1")
                .get(&[ProfileField::About, f])
                .await
                .unwrap_err();
            assert_eq!(validation_field(&err), "fields");
        }
        assert!(t.requests().is_empty());
    }

    #[test]
    fn every_documented_vertical_round_trips() {
        for (s, v) in [
            ("ALCOHOL", Vertical::Alcohol),
            ("APPAREL", Vertical::Apparel),
            ("AUTO", Vertical::Auto),
            ("BEAUTY", Vertical::Beauty),
            ("EDU", Vertical::Edu),
            ("ENTERTAIN", Vertical::Entertain),
            ("EVENT_PLAN", Vertical::EventPlan),
            ("FINANCE", Vertical::Finance),
            ("GOVT", Vertical::Govt),
            ("GROCERY", Vertical::Grocery),
            ("HEALTH", Vertical::Health),
            ("HOTEL", Vertical::Hotel),
            ("MATRIMONY_SERVICE", Vertical::MatrimonyService),
            ("NONPROFIT", Vertical::Nonprofit),
            ("NOT_A_BIZ", Vertical::NotABiz),
            ("ONLINE_GAMBLING", Vertical::OnlineGambling),
            ("OTHER", Vertical::Other),
            ("OTC_DRUGS", Vertical::OtcDrugs),
            ("PHYSICAL_GAMBLING", Vertical::PhysicalGambling),
            ("PROF_SERVICES", Vertical::ProfServices),
            ("RESTAURANT", Vertical::Restaurant),
            ("RETAIL", Vertical::Retail),
            ("TRAVEL", Vertical::Travel),
            ("UNDEFINED", Vertical::Undefined),
        ] {
            assert_eq!(serde_json::to_value(&v).unwrap(), json!(s));
            assert_eq!(serde_json::from_value::<Vertical>(json!(s)).unwrap(), v);
        }
        let unknown = Vertical::Unknown("NEW_ONE".into());
        assert_eq!(serde_json::to_value(&unknown).unwrap(), json!("NEW_ONE"));
    }

    #[tokio::test]
    async fn node_get_and_update() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"id": "5550001", "verified_name": "Lucky Shrub", "about": "Succulent specialists!", "vertical": "MATRIMONY_SERVICE"}),
        );
        t.push_json(200, json!({"id": "5550001", "success": true}));
        let node = client(&t).business_profile_node("5550001");
        let profile = node
            .get(&[
                ProfileField::Id,
                ProfileField::VerifiedName,
                ProfileField::About,
            ])
            .await
            .unwrap();
        assert_eq!(profile.id.as_deref(), Some("5550001"));
        assert_eq!(profile.verified_name.as_deref(), Some("Lucky Shrub"));
        assert_eq!(profile.vertical, Some(Vertical::MatrimonyService));
        let updated = node
            .update(&ProfileUpdate {
                email: Some("lucky@luckyshrub.com".into()),
                ..ProfileUpdate::default()
            })
            .await
            .unwrap();
        assert_eq!(updated.id.as_deref(), Some("5550001"));
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::GET);
        assert_eq!(reqs[0].path(), "/v25.0/5550001");
        assert_eq!(
            reqs[0].query("fields").as_deref(),
            Some("id,verified_name,about")
        );
        assert_eq!(reqs[0].bearer(), Some("TOKEN"));
        assert_eq!(reqs[1].method, Method::POST);
        assert_eq!(reqs[1].path(), "/v25.0/5550001");
        assert_eq!(
            reqs[1].json(),
            Some(json!({"messaging_product": "whatsapp", "email": "lucky@luckyshrub.com"}))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn node_update_success_false_is_an_error() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": false}));
        let err = client(&t)
            .business_profile_node("5")
            .update(&ProfileUpdate::default())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Http { status: 200, .. }), "{err:?}");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn node_id_cannot_leave_its_path_segment() {
        let t = ScriptedTransport::new();
        for bad in ["", ".."] {
            let err = client(&t)
                .business_profile_node(bad)
                .get(&[])
                .await
                .unwrap_err();
            assert_eq!(validation_field(&err), "path", "{bad:?}");
            let err = client(&t)
                .business_profile_node(bad)
                .update(&ProfileUpdate::default())
                .await
                .unwrap_err();
            assert_eq!(validation_field(&err), "path", "{bad:?}");
        }
        assert!(t.requests().is_empty());
        // A `/` stays inside the one segment: it cannot address the
        // phone-number edge (or anything else) with this token.
        t.push_json(200, json!({"id": "5"}));
        client(&t)
            .business_profile_node("5/whatsapp_business_profile")
            .get(&[])
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().path(),
            "/v25.0/5%2Fwhatsapp_business_profile"
        );
        assert_eq!(t.remaining(), 0);
    }
}
