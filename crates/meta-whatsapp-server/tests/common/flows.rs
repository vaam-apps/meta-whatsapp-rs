//! What the Flows tests share (roadmap M5d): the deployment's test key, an
//! integrating app that records what it is asked, and Meta's requests as
//! `tests/flows_data/kat_endpoint.mjs` made them with Meta's own Node.js
//! code.
//!
//! The key is the library's TEST-ONLY one (generated for its unit tests,
//! never registered with Meta: `flows/endpoint/testdata/README.md`).

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use meta_whatsapp_rs::client::flows::endpoint::{
    FlowEndpointKey, FlowErrorNotification, FlowRequest, FlowResponse,
};
use meta_whatsapp_rs::core::secret::AppSecret;
use meta_whatsapp_rs::webhooks::axum::body::Body;
use meta_whatsapp_rs::webhooks::axum::http::Method;
use meta_whatsapp_server_core::flows_endpoint::{
    DeploymentKey, FlowCall, FlowHandler, FlowKeys, FlowsDeployment, HandlerError, NoDeploymentKey,
};
use serde_json::{Value, json};

use super::{APP_SECRET, Call, PREVIOUS_APP_SECRET};

/// The deployment's Flow private key (PKCS#8 PEM) in the tests.
pub const KEY_PEM: &str = include_str!(
    "../../../meta-whatsapp-client/src/flows/endpoint/testdata/test_rsa2048_pkcs8.pem"
);

/// A second TEST-ONLY key (roadmap M5d2), beside the KATs made for it.
pub const OTHER_KEY_PEM: &str = include_str!("../flows_data/test_rsa2048_other_pkcs8.pem");

/// Meta's requests and the answers Meta's Node.js code seals for them.
const KAT: &str = include_str!("../flows_data/kat_endpoint.json");

/// The same scenarios, encrypted to [`OTHER_KEY_PEM`]'s public key
/// (`flows_data/kat_other_key.mjs`).
const KAT_OTHER: &str = include_str!("../flows_data/kat_other_key.json");

/// [`kat`], encrypted to [`OTHER_KEY_PEM`].
pub fn kat_other(name: &str) -> Value {
    let all: Value = serde_json::from_str(KAT_OTHER).unwrap();
    all[name].clone()
}

/// [`kat_body`], encrypted to [`OTHER_KEY_PEM`].
pub fn kat_other_body(name: &str) -> Vec<u8> {
    serde_json::to_vec(&kat_other(name)["request"]).unwrap()
}

/// A scenario of `kat_endpoint.json`: `request` (the body Meta's client
/// posts), `request_plaintext`, `response_plaintext`, `sealed_response`.
pub fn kat(name: &str) -> Value {
    let all: Value = serde_json::from_str(KAT).unwrap();
    all[name].clone()
}

/// The body of a scenario's request, as the bytes Meta signs.
pub fn kat_body(name: &str) -> Vec<u8> {
    serde_json::to_vec(&kat(name)["request"]).unwrap()
}

/// What the integrating app was asked.
#[derive(Debug, Clone, PartialEq)]
pub struct Asked {
    pub tenant: String,
    pub waba_id: String,
    pub request: FlowRequest,
}

/// An integrating app that answers with `reply`, and records every call.
pub struct Stub {
    reply: Mutex<Result<FlowResponse, HandlerError>>,
    exchanges: Mutex<Vec<Asked>>,
    notifications: Mutex<Vec<(Asked, FlowErrorNotification)>>,
}

impl Stub {
    pub fn new(reply: Result<FlowResponse, HandlerError>) -> Arc<Self> {
        Arc::new(Self {
            reply: Mutex::new(reply),
            exchanges: Mutex::default(),
            notifications: Mutex::default(),
        })
    }

    /// Answers the screen the KAT's `data_exchange` answer is sealed for.
    pub fn summary() -> Arc<Self> {
        Self::new(Ok(FlowResponse::next_screen(
            "SUMMARY",
            json!({"appointment": "Tuesday 10:00"}),
        )))
    }

    pub fn exchanges(&self) -> Vec<Asked> {
        self.exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn notifications(&self) -> Vec<(Asked, FlowErrorNotification)> {
        self.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Every call, of either kind.
    pub fn calls(&self) -> usize {
        self.exchanges().len() + self.notifications().len()
    }
}

fn asked(call: FlowCall) -> Asked {
    Asked {
        tenant: call.tenant.as_str().to_owned(),
        waba_id: call.waba_id.as_str().to_owned(),
        request: call.request,
    }
}

#[async_trait]
impl FlowHandler for Stub {
    async fn exchange(&self, call: FlowCall) -> Result<FlowResponse, HandlerError> {
        self.exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(asked(call));
        self.reply
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn error_notified(&self, call: FlowCall, notification: FlowErrorNotification) {
        self.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((asked(call), notification));
    }
}

/// The data endpoint of a test service: signed with [`APP_SECRET`] or
/// [`PREVIOUS_APP_SECRET`], the test key, answered by `handler`.
pub fn deployment(handler: Arc<dyn FlowHandler>) -> FlowsDeployment {
    FlowsDeployment::new(
        vec![
            AppSecret::new(APP_SECRET),
            AppSecret::new(PREVIOUS_APP_SECRET),
        ],
        Arc::new(DeploymentKey::new(
            FlowEndpointKey::from_pem(KEY_PEM).unwrap(),
        )),
        handler,
    )
}

/// [`deployment`], with `key` (a PEM) as the deployment's key, or none,
/// and the deployment's key used for a tenant with none of its own only if
/// `fallback` (roadmap M5d2).
pub fn deployment_with(
    key: Option<&str>,
    fallback: bool,
    handler: Arc<dyn FlowHandler>,
) -> FlowsDeployment {
    let keys: Arc<dyn FlowKeys> = match key {
        Some(pem) => Arc::new(DeploymentKey::new(FlowEndpointKey::from_pem(pem).unwrap())),
        None => Arc::new(NoDeploymentKey),
    };
    FlowsDeployment::new(
        vec![
            AppSecret::new(APP_SECRET),
            AppSecret::new(PREVIOUS_APP_SECRET),
        ],
        keys,
        handler,
    )
    .deployment_key_fallback(fallback)
}

/// `POST /webhooks/flows/{waba}` of `body`, signed as Meta signs it with
/// `secret`.
pub fn signed_with(waba: &str, body: &[u8], secret: &str) -> Call {
    let signature = meta_whatsapp_rs::webhooks::sign(&AppSecret::new(secret), body);
    Call::new(Method::POST, format!("/webhooks/flows/{waba}"))
        .header("content-type", "application/json")
        .header("x-hub-signature-256", &signature)
        .body(Body::from(body.to_vec()))
}

/// [`signed_with`] [`APP_SECRET`].
pub fn signed(waba: &str, body: &[u8]) -> Call {
    signed_with(waba, body, APP_SECRET)
}

/// The public half of a private key in PEM (roadmap M5d2).
pub fn public_key_pem(private_pem: &str) -> String {
    FlowEndpointKey::from_pem(private_pem)
        .unwrap()
        .public_key_pem()
        .unwrap()
}

/// The media vector's object, as a data exchange request carries it
/// (`tests/flows_data/kat_media.json`; roadmap M5d2).
pub fn media_object() -> Value {
    let kat: Value = serde_json::from_str(include_str!("../flows_data/kat_media.json")).unwrap();
    kat["media"].clone()
}
