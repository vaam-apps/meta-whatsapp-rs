//! Reference code for the `meta-whatsapp-rs-webhook-endpoint` skill: the endpoint Meta
//! calls — verify token, signatures, body limit, dedup — on axum or on any
//! other framework, and the status codes Meta must get.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use std::future::Future;
use std::sync::Arc;

use meta_whatsapp_rs::core::error::WebhookError;
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::{DeliveryReport, VerificationQuery};

/// Built once at startup from your secret store; a bad secret stops boot.
pub fn webhook_handler(
    app_secrets: Vec<AppSecret>, // current first; the old one too while rotating
    verify_token: &str,
    kv: Arc<dyn KvStore>, // shared by every instance
    sink: Arc<dyn EventSink<WebhookEvent>>,
) -> meta_whatsapp_rs::Result<WebhookHandler> {
    if verify_token.trim().is_empty() {
        // A blank token would answer 403 to every verification, at runtime only.
        return Err(
            meta_whatsapp_rs::core::error::ConfigError::new("WA_VERIFY_TOKEN is blank").into(),
        );
    }
    let handler = WebhookHandler::builder(
        SignatureVerifier::new(app_secrets)?, // refuses an empty list or a blank secret
        VerifyToken::new(verify_token),
        sink,
    )
    .dedup(DedupGuard::new(kv)) // Meta retries for 7 days and sends to every subscribed app
    .build(); // body limit: 3 MiB (`.max_body_bytes(n)` to change)
    Ok(handler)
}

/// axum (feature `axum`): `GET` verifies, `POST` delivers.
pub fn routes(handler: WebhookHandler) -> meta_whatsapp_rs::webhooks::axum::Router {
    // The axum the router is built with, re-exported: no axum pin of your own.
    meta_whatsapp_rs::webhooks::axum::Router::new().nest(
        "/webhooks/whatsapp",
        meta_whatsapp_rs::webhooks::router(Arc::new(handler)),
    )
}

/// Serve them: `axum::serve` through the same re-export.
pub async fn serve(handler: WebhookHandler, addr: &str) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    meta_whatsapp_rs::webhooks::axum::serve(listener, routes(handler)).await
}

/// Any other framework, `GET`: echo the challenge as `text/plain`.
pub fn answer_get(handler: &WebhookHandler, query: &VerificationQuery) -> (u16, String) {
    match handler.verify(query) {
        Ok(challenge) => (200, challenge),
        Err(_) => (403, String::new()),
    }
}

/// Any other framework, `POST`: the header first, then the body (at most
/// `max_body_bytes`, raw, never parsed or re-serialized), then `deliver`.
pub async fn answer_post<Fut>(
    handler: &WebhookHandler,
    signature: Option<&str>, // the `meta_whatsapp_rs::webhooks::SIGNATURE_HEADER` header
    read_body: impl FnOnce(usize) -> Fut,
) -> u16
where
    Fut: Future<Output = Option<Vec<u8>>>, // None: longer than the limit
{
    let Some(signature) = signature else {
        return 401; // before reading a byte of the body
    };
    let Some(body) = read_body(handler.max_body_bytes()).await else {
        return 413;
    };
    status_of(&handler.deliver(Some(signature), &body).await)
}

/// Anything but 200 makes Meta redeliver the whole batch.
pub fn status_of(result: &meta_whatsapp_rs::Result<DeliveryReport>) -> u16 {
    match result {
        Ok(_) => 200, // delivered, duplicate, or signed but unparseable (`Unparsed`)
        Err(Error::Webhook(
            WebhookError::MissingSignature
            | WebhookError::MalformedSignature
            | WebhookError::SignatureMismatch,
        )) => 401,
        Err(Error::Webhook(WebhookError::PayloadTooLarge { .. })) => 413,
        Err(Error::Webhook(WebhookError::ClaimInFlight)) => 503, // another request is delivering it
        Err(_) => 500, // your sink or the dedup store failed: Meta retries
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use meta_whatsapp_rs::adapters::sink::FnSink;
    use meta_whatsapp_rs::adapters::store::MemoryKvStore;
    use meta_whatsapp_rs::webhooks::axum::body::Body;
    use meta_whatsapp_rs::webhooks::axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    use super::*;

    const SECRET: &str = "test-app-secret";

    fn handler() -> WebhookHandler {
        let sink = FnSink::new(|_event: WebhookEvent| async { Ok(()) });
        webhook_handler(
            vec![AppSecret::new(SECRET)],
            "verify-me",
            Arc::new(MemoryKvStore::new()),
            Arc::new(sink),
        )
        .unwrap()
    }

    fn body() -> Vec<u8> {
        json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "messages", "value": {"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
                "messages": [{"from_user_id": "US.1", "id": "wamid.1", "timestamp": "1749416383",
                    "type": "text", "text": {"body": "Hi"}}]}}]}]})
        .to_string()
        .into_bytes()
    }

    #[tokio::test]
    async fn axum_verification_and_delivery() {
        let app = routes(handler());
        let verify = Request::get(
            "/webhooks/whatsapp?hub.mode=subscribe&hub.verify_token=verify-me&hub.challenge=1158201444",
        )
        .body(Body::empty())
        .unwrap();
        let response = app.clone().oneshot(verify).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let challenge = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&challenge[..], b"1158201444");

        let unsigned = Request::post("/webhooks/whatsapp")
            .body(Body::from(body()))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(unsigned).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        let signature = meta_whatsapp_rs::webhooks::sign(&AppSecret::new(SECRET), &body());
        let signed = Request::post("/webhooks/whatsapp")
            .header(meta_whatsapp_rs::webhooks::SIGNATURE_HEADER, signature)
            .body(Body::from(body()))
            .unwrap();
        assert_eq!(app.oneshot(signed).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn framework_free_answers() {
        let handler = handler();
        let query: VerificationQuery = serde_json::from_value(json!({
            "hub.mode": "subscribe", "hub.verify_token": "wrong", "hub.challenge": "1"
        }))
        .unwrap();
        assert_eq!(answer_get(&handler, &query).0, 403);

        let mut body_was_read = false;
        let status = answer_post(&handler, None, |_| {
            body_was_read = true;
            async { Some(body()) }
        })
        .await;
        assert_eq!((status, body_was_read), (401, false));

        let signature = meta_whatsapp_rs::webhooks::sign(&AppSecret::new(SECRET), &body());
        assert_eq!(
            answer_post(&handler, Some(&signature), |_| async { Some(body()) }).await,
            200
        );
        assert_eq!(
            answer_post(&handler, Some("sha256=00"), |_| async { Some(body()) }).await,
            401
        );
        assert_eq!(
            answer_post(&handler, Some(&signature), |_| async { None }).await,
            413
        );
    }

    #[test]
    fn blank_secrets_fail_at_startup() {
        let sink = Arc::new(FnSink::new(|_e: WebhookEvent| async { Ok(()) }));
        let kv = Arc::new(MemoryKvStore::new());
        assert!(webhook_handler(vec![AppSecret::new(" ")], "t", kv.clone(), sink.clone()).is_err());
        assert!(webhook_handler(vec![], "t", kv.clone(), sink.clone()).is_err());
        assert!(webhook_handler(vec![AppSecret::new(SECRET)], "", kv, sink).is_err());
    }
}
