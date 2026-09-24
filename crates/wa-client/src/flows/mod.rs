//! WhatsApp Flows: the management API (create, list, read, edit, upload Flow
//! JSON, preview, publish, deprecate, delete), the business public key a
//! data endpoint's traffic is encrypted to, and — with the `flows-endpoint`
//! feature — the data endpoint's own crypto (`flows::endpoint`).
//!
//! Docs: `flows/guides/flowsapi`, `flows/guides/lifecycle`,
//! `flows/guides/whatsapp-business-encryption`,
//! `reference/whatsapp-business-phone-number/business-encryption-api`,
//! `flows/guides/implementingyourflowendpoint`, `flows/guides/media_upload`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! | Accessor | Scope | Calls |
//! | --- | --- | --- |
//! | [`Client::flows`](crate::Client::flows) | WABA | [`Flows::create`], [`Flows::list`], [`Flows::list_stream`] |
//! | [`Client::flow`](crate::Client::flow) | one Flow | [`Flow::get`], [`Flow::update`], [`Flow::upload_flow_json`], [`Flow::assets`], [`Flow::preview`], [`Flow::publish`], [`Flow::deprecate`], [`Flow::delete`] |
//! | [`Client::business_encryption`](crate::Client::business_encryption) | phone number | [`BusinessEncryption::set_public_key`], [`BusinessEncryption::get`] |
//!
//! Not covered: sending a Flow (an interactive `flow` message or a template
//! with a `FLOW` button — see the messages and templates modules), Flow
//! webhooks (`wa-webhooks`), Flow JSON itself (opaque here; Meta validates
//! it), and the Flow metrics API (`flows/guides/metrics_api`), which Meta
//! deprecated on 2026-04-30 in favour of the Flow health webhooks.
//!
//! # Create, fill and publish a Flow
//!
//! ```no_run
//! # async fn demo(client: wa_client::Client) -> wa_client::Result<()> {
//! use wa_client::flows::{CreateFlow, FlowCategory};
//!
//! let flow_json = r#"{"version":"5.0","screens":[{"id":"WELCOME_SCREEN","title":"Welcome",
//!   "terminal":true,"success":true,"data":{},"layout":{"type":"SingleColumnLayout",
//!   "children":[{"type":"TextHeading","text":"Hello World"},{"type":"Footer",
//!   "label":"Complete","on-click-action":{"name":"complete","payload":{}}}]}}]}"#;
//!
//! let created = client
//!     .flows("<WABA_ID>")
//!     .create(&CreateFlow::new("My first flow", [FlowCategory::Other]))
//!     .await?;
//! let flow = client.flow(created.id);
//! let upload = flow.upload_flow_json(flow_json).await?;
//! if upload.validation_errors.is_empty() {
//!     flow.publish().await?;
//! } else {
//!     for e in &upload.validation_errors {
//!         eprintln!("{} at line {:?}: {}", e.error, e.line_start, e.message);
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! # Serve a data endpoint
//!
//! With the `flows-endpoint` feature: upload the public key once per phone
//! number, then decrypt each request, answer it, and seal the answer. See
//! `flows::endpoint` for the protocol and a full handler.
//!
//! ```no_run
//! # #[cfg(feature = "flows-endpoint")]
//! # async fn demo(client: wa_client::Client, pem: &str, body: &[u8], sig: Option<&str>,
//! #     secrets: &[wa_core::secret::AppSecret]) -> anyhow::Result<(u16, String)> {
//! use wa_client::flows::endpoint::{
//!     EncryptedFlowRequest, EndpointStatus, FlowAction, FlowEndpointKey, FlowResponse,
//!     verify_request_signature,
//! };
//!
//! let key = FlowEndpointKey::from_pem(pem)?; // once, at startup
//! client
//!     .business_encryption("<PHONE_NUMBER_ID>")
//!     .set_public_key(&key.public_key_pem()?)
//!     .await?;
//!
//! // Per request: signature over the raw body first, then decrypt.
//! if verify_request_signature(body, sig, secrets).is_err() {
//!     return Ok((EndpointStatus::SignatureMismatch.code(), String::new()));
//! }
//! let encrypted: EncryptedFlowRequest = serde_json::from_slice(body)?;
//! let Ok((request, sealer)) = key.decrypt_request(&encrypted) else {
//!     return Ok((EndpointStatus::DecryptionFailed.code(), String::new()));
//! };
//! let response = match request.action {
//!     FlowAction::Ping => FlowResponse::health_check(),
//!     _ => FlowResponse::next_screen("WELCOME", serde_json::json!({"greeting": "Hi"})),
//! };
//! Ok((EndpointStatus::Ok.code(), sealer.seal(&response)?))
//! # }
//! ```

mod encryption;
mod management;
mod types;

#[cfg(feature = "flows-endpoint")]
pub mod endpoint;

#[cfg(test)]
mod tests;

pub use encryption::{BusinessEncryption, BusinessPublicKey, PublicKeySignatureStatus};
pub use management::{Flow, Flows};
pub use types::{
    CreateFlow, CreatedFlow, FlowAsset, FlowAssetType, FlowCategory, FlowDetails, FlowJsonUpload,
    FlowPreview, FlowStatus, FlowValidationError, FlowValidationPointer, MAX_FLOW_JSON_BYTES,
    UpdateFlow,
};
