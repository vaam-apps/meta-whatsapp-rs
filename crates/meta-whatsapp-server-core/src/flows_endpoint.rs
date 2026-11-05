//! The Flows data endpoint (roadmap M5d, parity row 62): what Meta calls
//! while a user is in a Flow, behind the service's public listener
//! (`POST /webhooks/flows/{waba_id}`), with no HTTP framework. Meta's page
//! is `flows/guides/implementingyourflowendpoint`.
//!
//! ```text
//! FlowsEndpoint::handle(waba_id, signature, body)
//!   1. X-Hub-Signature-256 over the raw body, against every app secret,
//!      in constant time (the library's verify_request_signature):
//!      Refusal::BadSignature (432), before a record is read, a key
//!      loaded or the body parsed
//!   2. the WABA's tenant, by the service's records (unbound, or a
//!      suspended tenant's: Refusal::NotFound, 404) and the tenant's
//!      Flow key (FlowKeys; none: 404)
//!   3. decryption (RSA-OAEP key unwrap, AES-128-GCM): Refusal::Undecryptable
//!      (421, one answer for every cause: Meta's client then downloads the
//!      public key again and retries)
//!   4. the action:
//!        ping            ─► {"data":{"status":"active"}}, answered by the service
//!        data.error set  ─► the handler is told (error_notified), answered
//!                           {"data":{"acknowledged":true}} by the service
//!        anything else   ─► the integrating app's FlowHandler::exchange
//!   5. the answer sealed with the same AES key and the flipped IV
//!      (text/plain, base64)
//! ```
//!
//! **How the integrating app answers** is the [`FlowHandler`] port (design
//! D132): the service decrypts, the app decides the next screen. An
//! embedder implements the trait; the binary's own handler is
//! [`NoHandler`] (a health check and an error notification are answered
//! by the service, every data exchange is `503 unavailable` until a
//! handler is supplied). A callback URL called synchronously is the swap
//! the roadmap names, and it is one more implementation of this trait.
//!
//! **The key** is the business RSA private key the Flows encryption uses
//! (`flows/guides/whatsapp-business-encryption`): the [`FlowKeys`] port
//! gives the key for a tenant's WABA ([`DeploymentKey`]: one key for the
//! deployment, loaded from a file at startup). A secret: it never reaches
//! `Debug` (the library's `FlowEndpointKey` shows its size only), a log,
//! an error or the OpenAPI document.
//!
//! **What is logged** is the outcome, the WABA, the action and the screen
//! of a request, never its `data` (a user's answers), its `flow_token` or
//! a body: [`FlowCall`]'s `Debug` shows the action and the screen only.
//!
//! **Not done here**: Meta's request carries no Flow id, so a handler
//! cannot tell Flows of one WABA apart except by the `flow_token` the
//! app put in the message; and a replayed signed request is answered
//! again (the library's note about request IVs: the guard it suggests is
//! not applied).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
pub use meta_whatsapp_rs::client::flows::endpoint::RESPONSE_CONTENT_TYPE;
use meta_whatsapp_rs::client::flows::endpoint::{
    EncryptedFlowRequest, EndpointAction, EndpointStatus, FlowEndpointKey, FlowErrorNotification,
    FlowRequest, FlowResponse, verify_request_signature,
};
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_rs::core::secret::AppSecret;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::model::{TenantId, TenantStatus};
use crate::request::graph_id;
use crate::store::RecordStore;

/// The target of this module's log lines.
pub const LOG_TARGET: &str = "meta_whatsapp_server::api::flows_endpoint";

/// Largest request body read: 256 KiB. Meta's body is a wrapped key, an IV
/// and the encrypted payload (a screen's form data, media given as links),
/// and the page states no size; the bound keeps an unauthenticated read
/// small, since the signature can only be checked on the whole body.
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;

/// Requests a replica answers at once: past it, `503` before the body is
/// read (Meta's client retries or fails the screen).
pub const MAX_IN_FLIGHT: usize = 32;

/// How long the integrating app has to answer: past it, `503`. Meta's
/// health page asks for "less than 1 second" and degrades a Flow whose
/// endpoint is slow, so a handler that takes longer than this is failing
/// its Flow already.
pub const HANDLER_TIMEOUT: Duration = Duration::from_secs(5);

// ─── The ports ───────────────────────────────────────────────────────────

/// Gives the Flow private key of a tenant's WABA.
#[async_trait]
pub trait FlowKeys: Send + Sync + 'static {
    /// The key a request for `waba_id`, bound to `tenant`, was encrypted
    /// to, or `None` when there is none (the request is then `404`).
    ///
    /// # Errors
    ///
    /// The store failing: `503`.
    async fn key_for(
        &self,
        tenant: &TenantId,
        waba_id: &WabaId,
    ) -> Result<Option<Arc<FlowEndpointKey>>, StorageError>;
}

/// One key for the deployment, whatever the tenant or the WABA: the
/// deployment's Flow private key, loaded once. `Debug` shows the key's
/// size only.
#[derive(Debug, Clone)]
pub struct DeploymentKey(Arc<FlowEndpointKey>);

impl DeploymentKey {
    /// The deployment's `key`.
    pub fn new(key: FlowEndpointKey) -> Self {
        Self(Arc::new(key))
    }
}

#[async_trait]
impl FlowKeys for DeploymentKey {
    async fn key_for(
        &self,
        _tenant: &TenantId,
        _waba_id: &WabaId,
    ) -> Result<Option<Arc<FlowEndpointKey>>, StorageError> {
        Ok(Some(self.0.clone()))
    }
}

/// A decrypted request the integrating app is asked to answer.
pub struct FlowCall {
    /// The tenant bound to the WABA the request came for.
    pub tenant: TenantId,
    /// The WABA of the path (the endpoint's address names it).
    pub waba_id: WabaId,
    /// The request, as Meta's client sent it. Its `Debug` hides the
    /// `flow_token` and the media keys, not `data`.
    pub request: FlowRequest,
}

impl std::fmt::Debug for FlowCall {
    /// The tenant, the WABA, the action and the screen: never `data` (a
    /// user's answers) or the token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowCall")
            .field("tenant", &self.tenant)
            .field("waba_id", &self.waba_id)
            .field("action", &self.request.action)
            .field("screen", &self.request.screen)
            .finish_non_exhaustive()
    }
}

/// Why the integrating app did not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HandlerError {
    /// It cannot answer now (no handler, a backend down): `503`.
    #[error("the Flow handler is unavailable")]
    Unavailable,
    /// It failed: `500`.
    #[error("the Flow handler failed")]
    Failed,
}

/// The integrating app: decides the next screen of a Flow. Implemented by
/// an embedder; the service's own is [`NoHandler`].
#[async_trait]
pub trait FlowHandler: Send + Sync + 'static {
    /// Answer a `data_exchange`, `INIT` or `BACK` request. Within
    /// [`HANDLER_TIMEOUT`], else `503`.
    ///
    /// # Errors
    ///
    /// [`HandlerError`].
    async fn exchange(&self, call: FlowCall) -> Result<FlowResponse, HandlerError>;

    /// Meta's client reports that the app's previous answer was invalid
    /// (an error notification). The service acknowledges it whatever this
    /// does; the default ignores it. Within [`HANDLER_TIMEOUT`].
    async fn error_notified(&self, call: FlowCall, notification: FlowErrorNotification) {
        let _ = (call, notification);
    }
}

/// The binary's handler: no app is plugged in, so every data exchange is
/// refused `503`. A health check and an error notification are answered by
/// the service itself and never reach a handler.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHandler;

#[async_trait]
impl FlowHandler for NoHandler {
    async fn exchange(&self, _call: FlowCall) -> Result<FlowResponse, HandlerError> {
        Err(HandlerError::Unavailable)
    }
}

// ─── The deployment ──────────────────────────────────────────────────────

/// What the data endpoint runs with: the app secrets Meta signs with, the
/// Flow key(s), the integrating app. `Debug` shows no secret.
#[derive(Clone)]
pub struct FlowsDeployment {
    app_secrets: Vec<AppSecret>,
    keys: Arc<dyn FlowKeys>,
    handler: Arc<dyn FlowHandler>,
    handler_timeout: Duration,
}

impl std::fmt::Debug for FlowsDeployment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowsDeployment")
            .field("app_secrets", &self.app_secrets.len())
            .field("handler_timeout", &self.handler_timeout)
            .finish_non_exhaustive()
    }
}

impl FlowsDeployment {
    /// An endpoint verifying requests against `app_secrets` (the active
    /// one, then the previous one while rotating: Meta's guide on
    /// resetting the app secret), decrypting with `keys`, answered by
    /// `handler`.
    pub fn new(
        app_secrets: Vec<AppSecret>,
        keys: Arc<dyn FlowKeys>,
        handler: Arc<dyn FlowHandler>,
    ) -> Self {
        Self {
            app_secrets,
            keys,
            handler,
            handler_timeout: HANDLER_TIMEOUT,
        }
    }

    /// Give the handler `timeout` instead of [`HANDLER_TIMEOUT`] (tests).
    #[must_use]
    pub fn with_handler_timeout(mut self, timeout: Duration) -> Self {
        self.handler_timeout = timeout;
        self
    }
}

/// Why a request is not answered with a sealed response, and the status it
/// is answered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// No signature, a malformed one, or one no app secret produced: `432`.
    BadSignature,
    /// The WABA is not bound to an active tenant, or has no key: `404`.
    NotFound,
    /// The request could not be decrypted, or is not a Flow request:
    /// `421`.
    Undecryptable,
    /// The handler is unavailable or too slow, or the records are: `503`.
    Unavailable,
    /// The handler failed: `500`.
    Failed,
}

impl Refusal {
    /// The HTTP status to answer.
    pub fn status(self) -> u16 {
        match self {
            Self::BadSignature => EndpointStatus::SignatureMismatch.code(),
            Self::NotFound => 404,
            Self::Undecryptable => EndpointStatus::DecryptionFailed.code(),
            Self::Unavailable => 503,
            Self::Failed => 500,
        }
    }

    /// The outcome's label for the adapter's metrics.
    pub fn outcome(self) -> &'static str {
        match self {
            Self::BadSignature => "bad_signature",
            Self::NotFound => "not_found",
            Self::Undecryptable => "undecryptable",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }
}

/// The outcome label of a sealed answer to a data exchange.
pub const ANSWERED: &str = "answered";
/// The outcome label of a sealed answer to a health check.
pub const HEALTH_CHECK: &str = "health_check";
/// The outcome label of a sealed acknowledgement of an error notification.
pub const ERROR_NOTIFICATION: &str = "error_notification";

/// A sealed answer: the base64 text to send as `text/plain`, and the
/// outcome's label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// The body.
    pub body: String,
    /// The outcome's label ([`ANSWERED`], [`HEALTH_CHECK`],
    /// [`ERROR_NOTIFICATION`]).
    pub outcome: &'static str,
}

/// The data endpoint over the service's records.
pub struct FlowsEndpoint {
    deployment: FlowsDeployment,
    records: Arc<dyn RecordStore>,
    places: Arc<Semaphore>,
}

impl std::fmt::Debug for FlowsEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowsEndpoint")
            .field("deployment", &self.deployment)
            .finish_non_exhaustive()
    }
}

impl FlowsEndpoint {
    /// The endpoint `deployment` describes, routing by `records`.
    pub fn new(deployment: FlowsDeployment, records: Arc<dyn RecordStore>) -> Self {
        Self {
            deployment,
            records,
            places: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        }
    }

    /// A place among the requests answered at once ([`MAX_IN_FLIGHT`]),
    /// held until dropped; `None`: none left (`503`, before the body is
    /// read).
    pub fn admit(&self) -> Option<OwnedSemaphorePermit> {
        self.places.clone().try_acquire_owned().ok()
    }

    /// Answer one request for `waba_id` (the path's): `signature` is the
    /// `X-Hub-Signature-256` header's value, `body` the raw body (at most
    /// [`MAX_REQUEST_BYTES`]: the adapter's bound).
    ///
    /// # Errors
    ///
    /// [`Refusal`]: what to answer instead of a sealed response.
    pub async fn handle(
        &self,
        waba_id: &str,
        signature: Option<&str>,
        body: &[u8],
    ) -> Result<Sealed, Refusal> {
        // The signature first: before a record is read, a key loaded or a
        // byte parsed. The public key is public, so anyone can encrypt a
        // well-formed request; only the signature proves Meta sent it.
        if verify_request_signature(body, signature, &self.deployment.app_secrets).is_err() {
            return Err(Refusal::BadSignature);
        }
        let (tenant, waba) = self.tenant_of(waba_id).await?;
        let key = self
            .deployment
            .keys
            .key_for(&tenant, &waba)
            .await
            .map_err(|_| Refusal::Unavailable)?
            .ok_or(Refusal::NotFound)?;
        let encrypted: EncryptedFlowRequest =
            serde_json::from_slice(body).map_err(|_| Refusal::Undecryptable)?;
        let (request, sealer) = key
            .decrypt_request(&encrypted)
            .map_err(|_| Refusal::Undecryptable)?;
        let (response, outcome) = self.respond(tenant, waba, request).await?;
        let body = sealer.seal(&response).map_err(|_| Refusal::Failed)?;
        Ok(Sealed { body, outcome })
    }

    /// The tenant the WABA is bound to, if it is active.
    async fn tenant_of(&self, waba_id: &str) -> Result<(TenantId, WabaId), Refusal> {
        graph_id("waba_id", waba_id).map_err(|_| Refusal::NotFound)?;
        let waba = WabaId::new(waba_id).map_err(|_| Refusal::NotFound)?;
        let binding = self
            .records
            .waba(&waba)
            .await
            .map_err(|_| Refusal::Unavailable)?
            .ok_or(Refusal::NotFound)?;
        let tenant = self
            .records
            .tenant(&binding.tenant_id)
            .await
            .map_err(|_| Refusal::Unavailable)?
            .ok_or(Refusal::NotFound)?;
        if tenant.status != TenantStatus::Active {
            return Err(Refusal::NotFound);
        }
        Ok((binding.tenant_id, waba))
    }

    /// The response to `request`, and its outcome's label.
    async fn respond(
        &self,
        tenant: TenantId,
        waba_id: WabaId,
        request: FlowRequest,
    ) -> Result<(FlowResponse, &'static str), Refusal> {
        let timeout = self.deployment.handler_timeout;
        if request.action == EndpointAction::Ping {
            return Ok((FlowResponse::health_check(), HEALTH_CHECK));
        }
        if let Some(notification) = request.error_notification() {
            tracing::warn!(
                target: LOG_TARGET,
                waba_id = waba_id.as_str(),
                action = %action_name(&request.action),
                screen = request.screen.as_deref().unwrap_or(""),
                error = %bounded(&notification.error),
                "Meta's client reported an invalid answer from a Flow's endpoint"
            );
            let call = FlowCall {
                tenant,
                waba_id,
                request,
            };
            // Best effort: the acknowledgement does not wait on the app
            // beyond its time limit, and never depends on its answer.
            let _ = tokio::time::timeout(
                timeout,
                self.deployment.handler.error_notified(call, notification),
            )
            .await;
            return Ok((FlowResponse::acknowledge_error(), ERROR_NOTIFICATION));
        }
        tracing::info!(
            target: LOG_TARGET,
            waba_id = waba_id.as_str(),
            action = %action_name(&request.action),
            screen = request.screen.as_deref().unwrap_or(""),
            "a Flow data exchange"
        );
        let call = FlowCall {
            tenant,
            waba_id,
            request,
        };
        match tokio::time::timeout(timeout, self.deployment.handler.exchange(call)).await {
            Ok(Ok(response)) => Ok((response, ANSWERED)),
            Ok(Err(HandlerError::Unavailable)) | Err(_) => Err(Refusal::Unavailable),
            Ok(Err(HandlerError::Failed)) => Err(Refusal::Failed),
        }
    }
}

/// An action's wire name, for a log line.
fn action_name(action: &EndpointAction) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Meta's error key from a notification, bounded for a log line (it is
/// the client's text: a short machine key in practice).
fn bounded(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(64).collect()
}
