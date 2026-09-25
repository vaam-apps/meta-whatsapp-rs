//! Flow **data endpoint** side: decrypt what WhatsApp sends your endpoint,
//! encrypt what you answer (feature `flows-endpoint`).
//!
//! Sources: `flows/guides/implementingyourflowendpoint` (payloads, the
//! data API 3.0 crypto recipe, HTTP 421 on decryption failure, signature
//! validation), `flows/guides/whatsapp-business-encryption` (2048-bit RSA
//! key), `flows/gettingstarted/*` (HTTP 432 on a bad signature).
//!
//! # Protocol (data API 3.0)
//!
//! Request body: `{encrypted_flow_data, encrypted_aes_key, initial_vector}`,
//! all base64. The WhatsApp client picks a random 128-bit AES key, wraps it
//! with your RSA public key (RSA-OAEP, SHA-256 for both the hash and MGF1,
//! no label), and encrypts the JSON payload with AES-128-GCM under a 128-bit
//! IV, appending the 16-byte tag. You answer with the base64 of the
//! AES-128-GCM encryption of your JSON response under the **same key** and
//! the **bit-flipped IV** (every byte XOR `0xFF`), tag appended, as
//! `text/plain`.
//!
//! # Why aws-lc-rs for RSA
//!
//! Your endpoint decrypts attacker-supplied RSA ciphertext by design (the
//! public key is public), which is the setting timing attacks on RSA
//! decryption need. The `rsa` crate is affected by RUSTSEC-2023-0071 (Marvin)
//! with no fixed release; aws-lc-rs decrypts in constant time. AES-GCM uses
//! the pure-Rust `aes-gcm` crate, instantiated for the 16-byte IV Meta uses
//! (aws-lc-rs' AEAD API only takes 12-byte nonces).
//!
//! # No error oracle
//!
//! [`FlowEndpointKey::decrypt_request`] reports every failure — bad base64,
//! wrong IV length, RSA failure, wrong AES key length, GCM tag mismatch, a
//! payload that is not a Flow request — as the single
//! [`CryptoError::Decrypt`], and the recommended answer to all of them is
//! the same [`EndpointStatus::DecryptionFailed`]. Nothing about *why* it
//! failed leaves the process.
//!
//! Timing is not uniform, and does not need to be. Framing checks (base64,
//! IV length, a body shorter than the tag) return before the RSA step, and a
//! wrapped key that fails OAEP returns before the AES-GCM step, so a caller
//! can tell "RSA not attempted", "OAEP rejected" and "OAEP accepted" apart.
//! The first depends only on the caller's own bytes. The second is not an
//! oracle for OAEP, unlike PKCS#1 v1.5: a mauled ciphertext decodes to valid
//! OAEP only if its 256-bit label hash matches, and aws-lc does the padding
//! check itself in constant time, so the *reason* for an OAEP failure (the
//! Manger-style "leading byte not zero") never shows. Anything past OAEP needs
//! the AES key, which only whoever encrypted the request has.
//!
//! # Errors are leaf types
//!
//! These functions do no I/O and return [`CryptoError`] (decryption, keys)
//! or [`WebhookError`](meta_whatsapp_core::error::WebhookError) (signatures) rather
//! than [`meta_whatsapp_core::Error`]: a handler has to map each to its own HTTP status
//! ([`EndpointStatus`]) and would otherwise have to match the root enum to
//! do it. Both convert into [`meta_whatsapp_core::Error`] with `?`.
//!
//! # A full handler
//!
//! Verify `X-Hub-Signature-256` **first**, on the raw body, before parsing
//! or decrypting anything: your public key is public, so anyone can encrypt
//! a well-formed `INIT` or `data_exchange` request to you, and only the
//! signature proves Meta sent it. Then decrypt, answer, seal.
//!
//! ```no_run
//! use serde_json::json;
//! use meta_whatsapp_client::flows::endpoint::{
//!     EncryptedFlowRequest, EndpointStatus, EndpointAction, FlowEndpointKey, FlowResponse,
//!     verify_request_signature,
//! };
//! use meta_whatsapp_core::secret::AppSecret;
//!
//! /// `signature`: the `X-Hub-Signature-256` header; `body`: the request
//! /// body exactly as received. Returns the HTTP status and the body to send.
//! fn handler(
//!     key: &FlowEndpointKey,
//!     app_secrets: &[AppSecret],
//!     signature: Option<&str>,
//!     body: &[u8],
//! ) -> Result<(u16, String), Box<dyn std::error::Error>> {
//!     if verify_request_signature(body, signature, app_secrets).is_err() {
//!         return Ok((EndpointStatus::SignatureMismatch.code(), String::new()));
//!     }
//!     let encrypted: EncryptedFlowRequest = serde_json::from_slice(body)?;
//!     let Ok((request, sealer)) = key.decrypt_request(&encrypted) else {
//!         // Tells the WhatsApp client to re-download the public key and retry.
//!         return Ok((EndpointStatus::DecryptionFailed.code(), String::new()));
//!     };
//!     let response = match &request.action {
//!         EndpointAction::Ping => FlowResponse::health_check(),
//!         _ if request.error_notification().is_some() => FlowResponse::acknowledge_error(),
//!         EndpointAction::Init => FlowResponse::next_screen("WELCOME", json!({"name": "Ada"})),
//!         EndpointAction::DataExchange => {
//!             FlowResponse::complete(request.flow_token.clone().unwrap_or_default())
//!         }
//!         _ => FlowResponse::next_screen("WELCOME", json!({})),
//!     };
//!     Ok((EndpointStatus::Ok.code(), sealer.seal(&response)?))
//! }
//! ```
//!
//! Uploaded media in a `data_exchange` request comes as a `cdn_url` to
//! download yourself: check its host first ([`FlowMedia`]).

mod media;
mod pem;
mod signature;

#[cfg(test)]
mod tests;

use std::fmt;

use aes_gcm::AesGcm;
use aes_gcm::aead::consts::U16;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::aes::Aes128;
use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der, PublicKeyX509Der};
use aws_lc_rs::rsa::{
    KeyPair, OAEP_SHA256_MGF1SHA256, OaepPrivateDecryptingKey, PrivateDecryptingKey,
};
use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde::{Deserialize, Serialize};
use meta_whatsapp_core::error::CryptoError;

pub use media::{FlowMedia, MediaEncryptionMetadata, decrypt_media};
pub use signature::{SIGNATURE_HEADER, verify_request_signature};

use super::types::wire_enum;
use pem::{PemPrivateKey, Scrubbed, scrub};

/// AES-128-GCM with the 128-bit IV WhatsApp uses.
type FlowCipher = AesGcm<Aes128, U16>;

const AES_KEY_LEN: usize = 16;
const IV_LEN: usize = 16;
const TAG_LEN: usize = 16;
/// Meta requires a 2048-bit RSA key (`flows/guides/whatsapp-business-encryption`);
/// any other size can never match a public key Meta accepted.
const RSA_KEY_BITS: usize = 2048;
const WRONG_SIZE: &str = "WhatsApp Flows requires a 2048-bit RSA key";

/// Turn aws-lc-rs' size and algorithm rejections into actionable messages;
/// `None` for "could not parse", which the caller words per format.
fn rejected(error: &aws_lc_rs::error::KeyRejected) -> Option<CryptoError> {
    match error.description_() {
        "TooSmall" | "TooLarge" => Some(CryptoError::InvalidKey(WRONG_SIZE)),
        "WrongAlgorithm" => Some(CryptoError::InvalidKey(
            "not an RSA key; WhatsApp Flows requires a 2048-bit RSA key",
        )),
        _ => None,
    }
}

/// Standard alphabet; padding optional on input (being strict about `=`
/// would only turn a harmless encoder difference into a 421), always
/// emitted on output like Meta's sample code.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// `Content-Type` of the endpoint's (base64 text) response.
pub const RESPONSE_CONTENT_TYPE: &str = "text/plain";

/// HTTP statuses a Flow data endpoint answers with.
///
/// Only the codes Meta's pages state are here: the endpoint error-code
/// reference (`flows/reference/error-codes`) was unavailable when this was
/// written, so anything it adds is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EndpointStatus {
    /// `200`: the body is the sealed response.
    Ok,
    /// `421`: the request could not be decrypted. The WhatsApp client
    /// re-downloads the business public key and retries, which recovers from
    /// a key rotation (`implementingyourflowendpoint`, "Handling decryption
    /// errors").
    DecryptionFailed,
    /// `432`: `X-Hub-Signature-256` did not verify (Meta's getting-started
    /// endpoint samples).
    SignatureMismatch,
}

impl EndpointStatus {
    /// The HTTP status code.
    pub const fn code(self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::DecryptionFailed => 421,
            Self::SignatureMismatch => 432,
        }
    }
}

/// The body WhatsApp posts to a Flow data endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedFlowRequest {
    /// Base64 AES-128-GCM ciphertext of the payload, 16-byte tag appended.
    pub encrypted_flow_data: String,
    /// Base64 RSA-OAEP-SHA256 encryption of the 128-bit AES key.
    pub encrypted_aes_key: String,
    /// Base64 128-bit IV.
    pub initial_vector: String,
}

wire_enum! {
    /// Why WhatsApp called the endpoint (`action` of a Flow endpoint
    /// request). Not the `flow_action` a Flow message or template button
    /// starts with: that is [`crate::common::FlowAction`].
    pub enum EndpointAction {
        /// Periodic health check; answer [`FlowResponse::health_check`].
        Ping = "ping",
        /// The user opened the Flow (the message was sent with
        /// `flow_action: data_exchange`).
        Init = "INIT",
        /// The user pressed back on a screen with `refresh_on_back`.
        Back = "BACK",
        /// A screen was submitted, or a component with `on-select-action`
        /// changed.
        DataExchange = "data_exchange",
    }
}

/// A decrypted Flow endpoint request.
///
/// One struct covers the three documented payloads: data exchange
/// (`INIT`/`BACK`/`data_exchange`), error notification (`data.error` set, see
/// [`FlowRequest::error_notification`]) and health check (`ping`, nothing
/// but `version` and `action`).
///
/// `Debug` hides `flow_token`, `flow_token_signature` and the media keys
/// (`encryption_key`, `hmac_key`) of uploaded files anywhere in `data`; the
/// rest of `data` is the user's form input and is shown as is. `Serialize`
/// is not redacted.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowRequest {
    /// Data API version, `3.0` today.
    pub version: String,
    /// What triggered the request.
    pub action: EndpointAction,
    /// Screen the request comes from; may be absent for `INIT` and `BACK`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
    /// Arbitrary key-value data from the screen; may be absent for `INIT`
    /// and `BACK`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// The token you generated when sending the Flow. Treat it like a
    /// session id: it is redacted from `Debug`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_token: Option<String>,
    /// Listed in Meta's field table without a description; passed through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_token_signature: Option<String>,
}

/// Keys whose values are secrets wherever they appear in a request's `data`:
/// the per-file AES and HMAC keys of media a `PhotoPicker` or
/// `DocumentPicker` uploaded (`flows/guides/media_upload`). They sit next to
/// the file's `cdn_url`, so a logged request would let anyone with log access
/// download and decrypt the user's photo or document for up to 20 days.
const SECRET_DATA_KEYS: [&str; 2] = ["encryption_key", "hmac_key"];

/// Replace every [`SECRET_DATA_KEYS`] value in `value`, at any depth. The
/// picker's media array lives under a component name the business chose, so
/// the keys cannot be found by a fixed path.
fn redact_secret_keys(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if SECRET_DATA_KEYS.contains(&key.as_str()) {
                    *item = serde_json::Value::String("[REDACTED]".to_owned());
                } else {
                    redact_secret_keys(item);
                }
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_secret_keys),
        _ => {}
    }
}

impl fmt::Debug for FlowRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted = |v: &Option<String>| v.as_ref().map(|_| "[REDACTED]");
        let mut data = self.data.clone();
        if let Some(data) = data.as_mut() {
            redact_secret_keys(data);
        }
        f.debug_struct("FlowRequest")
            .field("version", &self.version)
            .field("action", &self.action)
            .field("screen", &self.screen)
            .field("data", &data)
            .field("flow_token", &redacted(&self.flow_token))
            .field(
                "flow_token_signature",
                &redacted(&self.flow_token_signature),
            )
            .finish()
    }
}

/// The client's report that your previous response was invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowErrorNotification {
    /// Error key (`data.error`).
    pub error: String,
    /// Explanation (`data.error_message`).
    pub error_message: Option<String>,
}

impl FlowRequest {
    /// `Some` when this is an error notification: the WhatsApp client could
    /// not use your last response. Acknowledge it with
    /// [`FlowResponse::acknowledge_error`].
    pub fn error_notification(&self) -> Option<FlowErrorNotification> {
        let data = self.data.as_ref()?.as_object()?;
        let error = data.get("error")?.as_str()?.to_owned();
        let error_message = data
            .get("error_message")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        Some(FlowErrorNotification {
            error,
            error_message,
        })
    }
}

/// The payload you answer a Flow endpoint request with, before sealing.
///
/// `Debug` hides the `flow_token` of a completion response, for the same
/// reason [`FlowRequest`]'s does.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowResponse {
    /// Screen to show next; `SUCCESS` completes the Flow. Absent in health
    /// check and error acknowledgement responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
    /// Data for that screen (a JSON object).
    pub data: serde_json::Value,
}

impl fmt::Debug for FlowResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut data = self.data.clone();
        if let Some(token) = data.pointer_mut("/extension_message_response/params/flow_token") {
            *token = serde_json::Value::String("[REDACTED]".to_owned());
        }
        f.debug_struct("FlowResponse")
            .field("screen", &self.screen)
            .field("data", &data)
            .finish()
    }
}

impl FlowResponse {
    /// The reserved screen name that completes a Flow.
    pub const SUCCESS_SCREEN: &'static str = "SUCCESS";

    /// Answer to a `ping`: `{"data": {"status": "active"}}`.
    pub fn health_check() -> Self {
        Self {
            screen: None,
            data: serde_json::json!({"status": "active"}),
        }
    }

    /// Answer to an error notification: `{"data": {"acknowledged": true}}`.
    pub fn acknowledge_error() -> Self {
        Self {
            screen: None,
            data: serde_json::json!({"acknowledged": true}),
        }
    }

    /// Show `screen` with `data` (a JSON object whose keys the screen's
    /// layout references).
    pub fn next_screen(screen: impl Into<String>, data: serde_json::Value) -> Self {
        Self {
            screen: Some(screen.into()),
            data,
        }
    }

    /// Add `data.error_message`, which the client shows as a snackbar on the
    /// screen — the documented way to reject bad input without failing.
    ///
    /// `data` must be an object per the docs; if it is anything else it is
    /// replaced by `{"error_message": …}` (the client would reject it
    /// anyway).
    #[must_use]
    pub fn with_error_message(mut self, message: impl Into<String>) -> Self {
        let message = serde_json::Value::String(message.into());
        if let Some(map) = self.data.as_object_mut() {
            map.insert("error_message".to_owned(), message);
        } else {
            self.data = serde_json::json!({ "error_message": message });
        }
        self
    }

    /// Complete the Flow: `{"screen": "SUCCESS", "data":
    /// {"extension_message_response": {"params": {"flow_token": …}}}}`. The
    /// client closes the Flow and sends a response message to the chat.
    pub fn complete(flow_token: impl Into<String>) -> Self {
        Self::complete_with_params(
            flow_token,
            std::iter::empty::<(String, serde_json::Value)>(),
        )
    }

    /// [`Self::complete`] with extra `params`, forwarded to your messages
    /// webhook with the completion message. A `flow_token` key in `params`
    /// is overridden by `flow_token`.
    pub fn complete_with_params<K: Into<String>>(
        flow_token: impl Into<String>,
        params: impl IntoIterator<Item = (K, serde_json::Value)>,
    ) -> Self {
        let mut map: serde_json::Map<String, serde_json::Value> =
            params.into_iter().map(|(k, v)| (k.into(), v)).collect();
        map.insert(
            "flow_token".to_owned(),
            serde_json::Value::String(flow_token.into()),
        );
        Self {
            screen: Some(Self::SUCCESS_SCREEN.to_owned()),
            data: serde_json::json!({"extension_message_response": {"params": map}}),
        }
    }
}

/// The business RSA private key of a Flow data endpoint.
///
/// Load it once at startup and share it (`Arc`); decryption takes `&self`.
/// `Debug` shows only the key size.
pub struct FlowEndpointKey {
    private: PrivateDecryptingKey,
}

impl fmt::Debug for FlowEndpointKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlowEndpointKey")
            .field("bits", &self.private.key_size_bits())
            .finish_non_exhaustive()
    }
}

impl Clone for FlowEndpointKey {
    fn clone(&self) -> Self {
        Self {
            private: self.private.clone(),
        }
    }
}

impl FlowEndpointKey {
    /// Load an **unencrypted** PEM private key: PKCS#8
    /// (`-----BEGIN PRIVATE KEY-----`) or PKCS#1
    /// (`-----BEGIN RSA PRIVATE KEY-----`, the form in Meta's examples).
    ///
    /// Password-protected keys — which Meta's own `openssl genrsa -des3`
    /// command produces — are rejected with [`CryptoError::InvalidKey`]:
    /// aws-lc-rs cannot decrypt them. Remove the password once with
    /// `openssl pkcs8 -topk8 -nocrypt -in private.pem -out private_unencrypted_pkcs8.pem`
    /// (the command from Meta's Java example) and keep the result in your
    /// secret store.
    pub fn from_pem(pem: &str) -> Result<Self, CryptoError> {
        match pem::parse_private_key(pem)? {
            PemPrivateKey::Pkcs8(der) => Self::from_pkcs8_der(&der.0),
            PemPrivateKey::Pkcs1(der) => Self::from_pkcs1_der(&der.0),
        }
    }

    /// Load an unencrypted PKCS#8 DER private key.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self, CryptoError> {
        let private = PrivateDecryptingKey::from_pkcs8(der).map_err(|e| {
            rejected(&e).unwrap_or(CryptoError::InvalidKey(
                "not an unencrypted PKCS#8 RSA private key",
            ))
        })?;
        Self::checked(private)
    }

    /// Load a PKCS#1 (`RSAPrivateKey`) DER private key.
    pub fn from_pkcs1_der(der: &[u8]) -> Result<Self, CryptoError> {
        let pair = KeyPair::from_der(der).map_err(|e| {
            rejected(&e).unwrap_or(CryptoError::InvalidKey("not a PKCS#1 RSA private key"))
        })?;
        // aws-lc-rs only builds decrypting keys from PKCS#8; re-encode.
        let pkcs8 = AsDer::<Pkcs8V1Der>::as_der(&pair)
            .map_err(|_| CryptoError::InvalidKey("could not re-encode the PKCS#1 key"))?;
        Self::from_pkcs8_der(pkcs8.as_ref())
    }

    fn checked(private: PrivateDecryptingKey) -> Result<Self, CryptoError> {
        if private.key_size_bits() != RSA_KEY_BITS {
            return Err(CryptoError::InvalidKey(WRONG_SIZE));
        }
        Ok(Self { private })
    }

    /// The matching public key as PEM (`-----BEGIN PUBLIC KEY-----`), ready
    /// for [`crate::flows::BusinessEncryption::set_public_key`].
    pub fn public_key_pem(&self) -> Result<String, CryptoError> {
        let der = AsDer::<PublicKeyX509Der>::as_der(&self.private.public_key())
            .map_err(|_| CryptoError::InvalidKey("could not encode the public key"))?;
        Ok(pem::encode("PUBLIC KEY", der.as_ref()))
    }

    /// Decrypt a request and return it with the [`ResponseSealer`] that
    /// encrypts your answer to it.
    ///
    /// Every failure is [`CryptoError::Decrypt`] (see the [module
    /// docs](self)); answer it with [`EndpointStatus::DecryptionFailed`].
    pub fn decrypt_request(
        &self,
        request: &EncryptedFlowRequest,
    ) -> Result<(FlowRequest, ResponseSealer), CryptoError> {
        let (plaintext, sealer) = self.open(request).ok_or(CryptoError::Decrypt)?;
        let request = serde_json::from_slice::<FlowRequest>(&plaintext.0)
            .map_err(|_| CryptoError::Decrypt)?;
        Ok((request, sealer))
    }

    /// The whole decryption, with failures collapsed to `None` so no branch
    /// can report anything more specific than "it did not decrypt".
    fn open(&self, request: &EncryptedFlowRequest) -> Option<(Scrubbed, ResponseSealer)> {
        let wrapped_key = BASE64.decode(&request.encrypted_aes_key).ok()?;
        let iv: [u8; IV_LEN] = BASE64
            .decode(&request.initial_vector)
            .ok()?
            .try_into()
            .ok()?;
        let mut data = Scrubbed(BASE64.decode(&request.encrypted_flow_data).ok()?);
        let body_len = data.0.len().checked_sub(TAG_LEN)?;

        let oaep = OaepPrivateDecryptingKey::new(self.private.clone()).ok()?;
        let mut unwrapped = Scrubbed(vec![0u8; oaep.min_output_size()]);
        let aes_key: [u8; AES_KEY_LEN] = oaep
            .decrypt(
                &OAEP_SHA256_MGF1SHA256,
                &wrapped_key,
                &mut unwrapped.0,
                None,
            )
            .ok()?
            .try_into()
            .ok()?;
        let sealer = ResponseSealer { key: aes_key, iv };

        let tag: [u8; TAG_LEN] = data.0.get(body_len..)?.try_into().ok()?;
        let body = data.0.get_mut(..body_len)?;
        FlowCipher::new(&sealer.key.into())
            .decrypt_in_place_detached(&iv.into(), b"", body, &tag.into())
            .ok()?;
        data.0.truncate(body_len);
        Some((data, sealer))
    }
}

/// Encrypts the answer to one request: same AES key, bit-flipped IV.
///
/// [`ResponseSealer::seal`] consumes it, so each request's key/IV pair
/// encrypts exactly one response — reusing a GCM nonce under the same key
/// for two different plaintexts would leak their XOR and the
/// authentication key. `Debug` never shows the key; the key bytes are
/// overwritten when it is dropped (best effort).
///
/// The protocol itself derives the response nonce from the request IV, so
/// a *replayed* request makes the endpoint seal a second response under the
/// same key and nonce. Only a party holding the request bytes can replay
/// it (the WhatsApp client, which knows the key anyway, or someone who has
/// broken your TLS); if that is in your threat model, reject request IVs you
/// have already answered, e.g. with a `KvStore::put_if_absent` on the
/// `initial_vector`.
pub struct ResponseSealer {
    key: [u8; AES_KEY_LEN],
    iv: [u8; IV_LEN],
}

impl fmt::Debug for ResponseSealer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponseSealer").finish_non_exhaustive()
    }
}

impl Drop for ResponseSealer {
    fn drop(&mut self) {
        scrub(&mut self.key);
        scrub(&mut self.iv);
    }
}

impl ResponseSealer {
    /// Serialize `response` to JSON, encrypt it, and return the base64 text
    /// to send as the `text/plain` body ([`RESPONSE_CONTENT_TYPE`]).
    ///
    /// Takes a [`FlowResponse`] or any other serializable value (e.g. a
    /// `serde_json::Value`). Fails with [`CryptoError::Encrypt`] only if
    /// `response` cannot be serialized.
    pub fn seal<T: Serialize + ?Sized>(self, response: &T) -> Result<String, CryptoError> {
        let mut buffer = serde_json::to_vec(response).map_err(|_| CryptoError::Encrypt)?;
        let flipped_iv = self.iv.map(|byte| byte ^ 0xFF);
        let tag = FlowCipher::new(&self.key.into())
            .encrypt_in_place_detached(&flipped_iv.into(), b"", &mut buffer)
            .map_err(|_| CryptoError::Encrypt)?;
        buffer.extend_from_slice(&tag);
        Ok(BASE64.encode(buffer))
    }
}
