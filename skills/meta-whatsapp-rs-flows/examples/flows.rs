//! Reference code for the `meta-whatsapp-rs-flows` skill: managing WhatsApp Flows
//! (create, check validation errors, publish), the business public key, and
//! a Flow data endpoint (feature `flows-endpoint`) that verifies the
//! signature before decrypting anything.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use serde_json::json;
use meta_whatsapp_rs::client::flows::endpoint::{
    EncryptedFlowRequest, EndpointAction, EndpointStatus, FlowEndpointKey, FlowResponse,
    verify_request_signature,
};
use meta_whatsapp_rs::client::flows::{CreateFlow, FlowCategory};
use meta_whatsapp_rs::core::ids::FlowId;
use meta_whatsapp_rs::prelude::*;

/// Create a Flow from its JSON; publish only when Meta found no errors.
pub async fn create_and_publish(
    client: &Client,
    waba_id: WabaId,
    flow_json: &serde_json::Value,
) -> meta_whatsapp_rs::Result<Result<FlowId, Vec<String>>> {
    let flow = CreateFlow::new("fitting_booking", [FlowCategory::AppointmentBooking])
        .flow_json_value(flow_json)
        .endpoint_uri("https://shop.example/flows/fitting"); // only for Flows with a data endpoint
    let created = client.flows(waba_id).create(&flow).await?;
    if !created.validation_errors.is_empty() {
        // Created as a draft: fix the JSON, `upload_flow_json`, then publish.
        let errors = created
            .validation_errors
            .iter()
            .map(|e| e.message.clone())
            .collect();
        return Ok(Err(errors));
    }
    client.flow(created.id.clone()).publish().await?; // published Flows cannot be edited
    Ok(Ok(created.id))
}

/// Once per phone number: the public half of the endpoint's RSA key.
pub async fn upload_public_key(
    client: &Client,
    phone_number_id: PhoneNumberId,
    key: &FlowEndpointKey,
) -> anyhow::Result<()> {
    let pem = key.public_key_pem()?;
    client
        .business_encryption(phone_number_id)
        .set_public_key(&pem)
        .await?;
    Ok(())
}

/// The data endpoint: `(HTTP status, text/plain body)`. `body` is the raw
/// request body; `signature` its `X-Hub-Signature-256` header.
pub fn flow_endpoint(
    key: &FlowEndpointKey, // unencrypted PKCS#8 or PKCS#1 PEM, from your secret store
    app_secrets: &[AppSecret],
    signature: Option<&str>,
    body: &[u8],
) -> (u16, String) {
    // Signature FIRST: the public key is public, so anyone can encrypt a
    // well-formed request to you; only the signature proves Meta sent it.
    if verify_request_signature(body, signature, app_secrets).is_err() {
        return (EndpointStatus::SignatureMismatch.code(), String::new()); // 432
    }
    let Ok(encrypted) = serde_json::from_slice::<EncryptedFlowRequest>(body) else {
        return (EndpointStatus::DecryptionFailed.code(), String::new());
    };
    let Ok((request, sealer)) = key.decrypt_request(&encrypted) else {
        // One answer for every failure (no oracle): the client refetches the key and retries.
        return (EndpointStatus::DecryptionFailed.code(), String::new()); // 421
    };
    let response = match &request.action {
        EndpointAction::Ping => FlowResponse::health_check(),
        _ if request.error_notification().is_some() => FlowResponse::acknowledge_error(),
        EndpointAction::Init => {
            FlowResponse::next_screen("SLOTS", json!({"slots": ["9:00", "14:00"]}))
        }
        EndpointAction::DataExchange => {
            FlowResponse::complete(request.flow_token.clone().unwrap_or_default())
        }
        _ => FlowResponse::next_screen("SLOTS", json!({})),
    };
    match sealer.seal(&response) {
        Ok(body) => (EndpointStatus::Ok.code(), body), // same AES key, flipped IV
        Err(_) => (500, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::testing::ScriptedTransport;

    use super::*;

    #[tokio::test]
    async fn validation_errors_keep_it_a_draft() {
        let transport = ScriptedTransport::new();
        transport.push_json(
            200,
            json!({"id": "1234", "success": true, "validation_errors": [{
                "error": "INVALID_PROPERTY_VALUE", "error_type": "FLOW_JSON_ERROR",
                "message": "Invalid value found for property 'type'.",
                "line_start": 10, "line_end": 10, "column_start": 21, "column_end": 34
            }]}),
        );
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .build()
            .unwrap();
        let outcome = create_and_publish(
            &client,
            "102290129340398".into(),
            &json!({"version": "7.1"}),
        )
        .await
        .unwrap();
        assert!(outcome.is_err()); // nothing published
        let body = transport.last_request().unwrap().json().unwrap();
        assert_eq!(body["categories"], json!(["APPOINTMENT_BOOKING"]));
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn unsigned_requests_are_refused_before_decryption() {
        let secrets = [AppSecret::new("app-secret")];
        let body = br#"{"encrypted_flow_data":"x","encrypted_aes_key":"y","initial_vector":"z"}"#;
        assert!(verify_request_signature(body, None, &secrets).is_err());
        let signed = meta_whatsapp_rs::webhooks::sign(&secrets[0], body);
        assert!(verify_request_signature(body, Some(&signed), &secrets).is_ok());
        assert_eq!(EndpointStatus::SignatureMismatch.code(), 432);
        assert_eq!(EndpointStatus::DecryptionFailed.code(), 421);
    }
}
