//! Reference code for the `wa-rs-setup` skill: building the `Client` once,
//! and acting as a merchant with `with_token`.
//!
//! wa-rs compiles this file and runs its tests in its own gate
//! (`crates/wa-rs/tests/skills.rs`), so it matches the API of the commit the
//! skill is stamped with.

use std::time::Duration;

use wa_rs::adapters::http::ReqwestTransport;
use wa_rs::core::config::{ApiVersion, GraphEndpoint};
use wa_rs::prelude::*;

/// The two production clients: one business, or one platform serving many.
pub fn production_clients() -> anyhow::Result<(Client, Client)> {
    // One business, one system user token (feature `reqwest`, on by default):
    let client = wa_rs::client(std::env::var("WA_SYSTEM_USER_TOKEN")?)?;

    // Multi-tenant: no default token; every call runs as a merchant (below).
    let platform = wa_rs::client_builder()?.build()?;
    Ok((client, platform))
}

/// Every setting, chosen once at startup.
pub fn configured_client(token: AccessToken) -> wa_rs::Result<Client> {
    let transport = ReqwestTransport::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()?;
    Client::builder()
        .transport(transport) // required: `ReqwestTransport`, or your own `HttpTransport`
        .access_token(token) // optional: the default token
        .api_version(ApiVersion::new(25, 0)) // pinned: moves only when you change it
        .timeout(Duration::from_secs(15)) // per request; default 30 s
        .retry(RetryPolicy {
            max_retries: 2,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(4),
        })
        .build()
}

/// A Graph proxy (or a mock server) instead of `graph.facebook.com`. The
/// token then goes to the proxy, never to production Graph.
pub fn behind_a_proxy(base_url: &str) -> wa_rs::Result<Client> {
    let endpoint = GraphEndpoint::custom(base_url, ApiVersion::DEFAULT)?;
    wa_rs::client_builder()?.endpoint(endpoint).build()
}

/// Act as one merchant: the same transport, pool and retry policy, their
/// token. Cheap: never build a client per request.
pub async fn send_as_merchant(
    platform: &Client,
    merchant_token: AccessToken, // e.g. from the TokenVault, by phone number id
    phone_number_id: PhoneNumberId,
    to: Recipient,
) -> wa_rs::Result<SendResponse> {
    let merchant = platform.with_token(merchant_token);
    let text = OutboundMessage::text(to, "Your order has shipped.");
    merchant.messages(phone_number_id).send(&text).await
}

/// An endpoint wa-rs does not wrap: the request builders keep the auth,
/// retries, error decoding and credential host allowlist.
pub async fn unwrapped(client: &Client, waba_id: &WabaId) -> wa_rs::Result<serde_json::Value> {
    client
        .get_at(&[waba_id.as_str()]) // one verbatim, percent-encoded segment per element
        .query("fields", "id,name,timezone_id")
        .send()
        .await
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wa_rs::core::testing::ScriptedTransport;

    use super::*;

    #[tokio::test]
    async fn with_token_sends_the_merchants_token_to_the_pinned_version() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"messages": [{"id": "wamid.1"}]}));
        let platform = Client::builder()
            .transport(transport.clone())
            .api_version(ApiVersion::new(25, 0))
            .build()
            .unwrap();
        assert!(platform.token().is_none());

        let sent = send_as_merchant(
            &platform,
            AccessToken::new("MERCHANT-TOKEN"),
            PhoneNumberId::new("106540352242922"),
            Recipient::phone("+16505551234"),
        )
        .await
        .unwrap();

        assert_eq!(sent.message_id().map(MessageId::as_str), Some("wamid.1"));
        let request = transport.last_request().unwrap();
        assert_eq!(request.path(), "/v25.0/106540352242922/messages");
        assert_eq!(request.bearer(), Some("MERCHANT-TOKEN"));
        assert_eq!(request.json().unwrap()["to"], "+16505551234");
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn an_id_stays_one_path_segment() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"id": "1"}));
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .build()
            .unwrap();
        // An id read from a database or a webhook cannot address another object.
        unwrapped(&client, &WabaId::new("123/subscribed_apps"))
            .await
            .unwrap();
        let request = transport.last_request().unwrap();
        assert_eq!(request.path(), "/v25.0/123%2Fsubscribed_apps");
        assert_eq!(
            request.query("fields").as_deref(),
            Some("id,name,timezone_id")
        );
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn a_proxy_endpoint_keeps_the_version() {
        let client = behind_a_proxy("https://graph-proxy.internal/").unwrap();
        assert_eq!(
            client.endpoint().base().as_str(),
            "https://graph-proxy.internal/"
        );
        assert_eq!(client.endpoint().version(), ApiVersion::DEFAULT);
    }
}
