//! The business public key a Flow data endpoint's requests are encrypted to
//! (`/{PHONE_NUMBER_ID}/whatsapp_business_encryption`).
//!
//! Sources: `flows/guides/whatsapp-business-encryption` (guide) and
//! `reference/whatsapp-business-phone-number/business-encryption-api`
//! (reference). They disagree in two places; both are handled:
//!
//! - **Upload encoding.** The guide's example posts
//!   `application/x-www-form-urlencoded`, the reference says
//!   `multipart/form-data`. We follow the guide's concrete example
//!   (urlencoded); Graph accepts either for scalar fields.
//! - **Read shape.** The guide shows a flat object, the reference wraps it in
//!   `{"data": [...]}`. [`BusinessEncryption::get`] accepts both.

use serde::Deserialize;
use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::PhoneNumberId;

use super::types::wire_enum;
use crate::Client;

/// Business public key of one phone number. See [`Client::business_encryption`].
#[derive(Debug, Clone)]
pub struct BusinessEncryption {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`BusinessEncryption`] API for `phone_number_id`: upload (Meta signs
    /// it) and read the RSA public key WhatsApp encrypts Flow data endpoint
    /// requests to. Every phone number of a WABA needs its own upload.
    pub fn business_encryption(
        &self,
        phone_number_id: impl Into<PhoneNumberId>,
    ) -> BusinessEncryption {
        BusinessEncryption {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

wire_enum! {
    /// Whether Meta's signature over the stored public key checks out.
    pub enum PublicKeySignatureStatus {
        /// The stored key is signed and the signature verifies.
        Valid = "VALID",
        /// The signature does not match the key; upload the key again.
        Mismatch = "MISMATCH",
    }
}

/// The stored business public key.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BusinessPublicKey {
    /// The PEM public key, when one is stored.
    #[serde(default)]
    pub business_public_key: Option<String>,
    /// Signature state of that key.
    #[serde(default)]
    pub business_public_key_signature_status: Option<PublicKeySignatureStatus>,
}

/// The two documented response shapes of `GET …/whatsapp_business_encryption`.
#[derive(Deserialize)]
#[serde(untagged)]
enum GetResponse {
    Wrapped { data: Vec<BusinessPublicKey> },
    Flat(BusinessPublicKey),
}

impl BusinessEncryption {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Upload the business public key (`POST …/whatsapp_business_encryption`).
    /// Meta signs it on upload; it replaces any previous key.
    ///
    /// `public_key_pem` is a 2048-bit RSA public key in PEM
    /// (`-----BEGIN PUBLIC KEY-----`), e.g. from
    /// `openssl rsa -in private.pem -pubout` or, with the `flows-endpoint`
    /// feature, `FlowEndpointKey::public_key_pem`. Re-upload after
    /// re-registering the number or on `public-key-missing` /
    /// `public-key-signature-verification` alerts.
    ///
    /// Refuses anything that looks like a *private* key before a byte is
    /// sent: pasting the wrong PEM would hand the key to a third party.
    /// Replayed on transient errors: uploading the same key twice is
    /// harmless.
    pub async fn set_public_key(&self, public_key_pem: &str) -> Result<()> {
        validate_public_key_pem(public_key_pem)?;
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("business_public_key", public_key_pem)
            .finish();
        self.client
            .post(&format!(
                "{}/whatsapp_business_encryption",
                self.phone_number_id
            ))
            .bytes("application/x-www-form-urlencoded", body)
            .idempotent(true)
            .context("set business public key response")
            .send_success()
            .await
    }

    /// The stored key and its signature status
    /// (`GET …/whatsapp_business_encryption`). Both fields are `None` when no
    /// key has been uploaded.
    pub async fn get(&self) -> Result<BusinessPublicKey> {
        let response: GetResponse = self
            .client
            .get(&format!(
                "{}/whatsapp_business_encryption",
                self.phone_number_id
            ))
            .context("business public key response")
            .send()
            .await?;
        Ok(match response {
            GetResponse::Flat(key) => key,
            GetResponse::Wrapped { data } => data.into_iter().next().unwrap_or(BusinessPublicKey {
                business_public_key: None,
                business_public_key_signature_status: None,
            }),
        })
    }
}

fn validate_public_key_pem(pem: &str) -> Result<(), ValidationError> {
    const FIELD: &str = "business_public_key";
    let trimmed = pem.trim();
    if trimmed.is_empty() {
        return Err(ValidationError::new(FIELD, "is required"));
    }
    if trimmed.contains("PRIVATE KEY") {
        return Err(ValidationError::new(
            FIELD,
            "is a private key; upload only the public key (`openssl rsa -in private.pem -pubout`)",
        ));
    }
    if !(trimmed.starts_with("-----BEGIN ") && trimmed.contains("PUBLIC KEY-----")) {
        return Err(ValidationError::new(
            FIELD,
            "must be a PEM public key (-----BEGIN PUBLIC KEY-----)",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use http::Method;
    use serde_json::json;
    use wa_core::ErrorKind;
    use wa_core::testing::{RecordedBody, ScriptedTransport};

    use super::*;
    use crate::RetryPolicy;

    const PEM: &str =
        "-----BEGIN PUBLIC KEY-----\nAAA\nBBB\nCCC\nDDD\nEEE\nFFF\nGGG\n-----END PUBLIC KEY-----";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn uploads_the_key_urlencoded_like_the_guide() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .business_encryption("PHONE_NUMBER_ID")
            .set_public_key(PEM)
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            "/v25.0/PHONE_NUMBER_ID/whatsapp_business_encryption"
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        let RecordedBody::Bytes { content_type, data } = &req.body else {
            panic!("expected a buffered body, got {:?}", req.body);
        };
        assert_eq!(content_type, "application/x-www-form-urlencoded");
        let pairs: Vec<(String, String)> = url::form_urlencoded::parse(data)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            vec![("business_public_key".to_owned(), PEM.to_owned())]
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn refuses_private_keys_and_non_pem_before_sending() {
        let t = ScriptedTransport::new();
        let enc = client(&t).business_encryption("1");
        for bad in [
            "",
            "   ",
            "-----BEGIN PRIVATE KEY-----\nMII\n-----END PRIVATE KEY-----",
            "-----BEGIN RSA PRIVATE KEY-----\nMII\n-----END RSA PRIVATE KEY-----",
            "MIIBIjANBgkqh",
            // A key-pair bundle passes the "is a public key PEM" check; only
            // the private-key guard stops it.
            "-----BEGIN PUBLIC KEY-----\nMIIB\n-----END PUBLIC KEY-----\n\
             -----BEGIN PRIVATE KEY-----\nMIIE\n-----END PRIVATE KEY-----",
        ] {
            let err = enc.set_public_key(bad).await.unwrap_err();
            assert!(
                matches!(&err, wa_core::Error::Validation(v) if v.field == "business_public_key"),
                "{bad:?} -> {err}"
            );
        }
        assert!(t.requests().is_empty(), "nothing may leave the process");
    }

    #[tokio::test]
    async fn reads_the_guide_shape() {
        let t = ScriptedTransport::new();
        // Guide example, with its missing comma and unquoted enum fixed.
        t.push_json(
            200,
            json!({
                "business_public_key": "<2048_bit_RSA_key>",
                "business_public_key_signature_status": "VALID"
            }),
        );
        let key = client(&t).business_encryption("1").get().await.unwrap();
        assert_eq!(
            key.business_public_key.as_deref(),
            Some("<2048_bit_RSA_key>")
        );
        assert_eq!(
            key.business_public_key_signature_status,
            Some(PublicKeySignatureStatus::Valid)
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/1/whatsapp_business_encryption");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn reads_the_reference_shape_and_empty_data() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{
                "business_public_key": "-----BEGIN PUBLIC KEY-----\nAAA\n-----END PUBLIC KEY-----",
                "business_public_key_signature_status": "MISMATCH"
            }]}),
        );
        t.push_json(200, json!({"data": []}));
        let enc = client(&t).business_encryption("1");
        let key = enc.get().await.unwrap();
        assert_eq!(
            key.business_public_key_signature_status,
            Some(PublicKeySignatureStatus::Mismatch)
        );
        let none = enc.get().await.unwrap();
        assert_eq!(none.business_public_key, None);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn graph_errors_surface_with_their_kind() {
        let t = ScriptedTransport::new();
        // Reference example for the 403 response.
        t.push_json(
            403,
            json!({"error": {
                "message": "Your app doesn't have permission to modify encryption settings for this WhatsApp Business phone number",
                "type": "OAuthException",
                "code": 200,
                "error_subcode": 1349174,
                "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn",
                "error_user_title": "Permission Denied",
                "error_user_msg": "Your app doesn't have permission to modify this resource"
            }}),
        );
        let err = client(&t)
            .business_encryption("1")
            .set_public_key(PEM)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert_eq!(t.remaining(), 0);
    }
}
