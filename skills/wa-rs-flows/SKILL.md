---
name: wa-rs-flows
description: "WhatsApp Flows with wa-rs - creating a Flow from its JSON (CreateFlow, categories, validation errors), publishing, previews and assets, uploading the business public key, sending a Flow message, and the Flow data endpoint (feature flows-endpoint) - verify X-Hub-Signature-256 first, decrypt with FlowEndpointKey (RSA-OAEP + AES-GCM), answer ping, INIT, data_exchange and error notifications with FlowResponse, and the 421 and 432 status codes. Load when building WhatsApp Flows (forms, bookings, sign-ups) or the HTTPS endpoint a Flow calls."
---

# wa-rs-flows

> **Verified against wa-rs 6909be3b54768abc3d5f9b04543a49f32b072669 (2026-09-25).** On another revision, trust the code over this page.

Reference code: [examples/flows.rs](examples/flows.rs), compiled and
tested by wa-rs's own gate.

## When to use

A Flow is a multi-screen form inside WhatsApp. Managing Flows:
`wa_rs::client::flows` (`client.flows(waba_id)`, `client.flow(flow_id)`).
A Flow that needs your data at runtime calls your **data endpoint**,
whose crypto is `wa_rs::client::flows::endpoint` (feature
`flows-endpoint`). Sending a Flow message: `wa-rs-interactive-messages`.

## Create and publish

```rust
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
```

Also on `Flow`: `get`, `preview(invalidate)`, `update(&UpdateFlow)`,
`upload_flow_json(bytes)` (≤ 10 MiB, `MAX_FLOW_JSON_BYTES`), `assets`,
`deprecate`, `delete` (drafts only). Status changes arrive as
`WebhookEvent::FlowUpdated`.

## The business public key

Every phone number that sends endpoint-backed Flows needs the public half
of your 2048-bit RSA key uploaded (Meta signs it):

```rust
let pem = key.public_key_pem()?;
client
    .business_encryption(phone_number_id)
    .set_public_key(&pem)
    .await?;
```

`FlowEndpointKey::from_pem` takes an **unencrypted** PKCS#8 or PKCS#1
PEM; Meta's `openssl genrsa -des3` produces a password-protected one:
convert it once with `openssl pkcs8 -topk8 -nocrypt` and keep the result
in your secret store.

## The data endpoint: signature first

```rust
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
```

~~`flows::endpoint::FlowAction`~~: renamed `EndpointAction` in 6909be3
(2026-09-24); `Flows::list_stream` and `Flow::assets_stream` take a query.

Answer 200 with `sealer.seal(&response)?` as `text/plain`
(`RESPONSE_CONTENT_TYPE`): the same AES key, the bit-flipped IV.
`FlowResponse::with_error_message` shows an error on the screen;
`complete_with_params` ends the Flow with parameters.

## Pitfalls

- **Verify the signature before parsing or decrypting anything**: the
  public key is public, so a well-formed encrypted request proves nothing.
- Every decryption failure is the single `CryptoError::Decrypt`; answer
  421, which makes the WhatsApp client refetch the key and retry (it also
  recovers from a key rotation). Never tell the caller why it failed.
- Uploaded media in a `data_exchange` request comes as a `cdn_url` to
  download yourself: check its host first (`FlowMedia`), then
  `decrypt_media`.
- `FlowRequest`'s `Debug` hides the flow token and media keys, but the
  rest of `data` is the user's form input: log it like personal data.
- The RSA step uses aws-lc-rs (constant time), never the `rsa` crate
  (RUSTSEC-2023-0071): keep it that way in your own code.

## What wa-rs does not do

- No Flow JSON builder or validator: write the JSON (Meta's Flow
  Builder) and read `validation_errors`.
- The Flows metrics API is not wrapped (deprecated by Meta 2026-04-30).
- No HTTP server for the endpoint: wire `flow_endpoint` into your
  framework, like the webhook endpoint (`wa-rs-webhook-endpoint`).

## Related skills

`wa-rs-interactive-messages` (sending a Flow), `wa-rs-webhook-events`
(`NfmReply`, the completed Flow), `wa-rs-webhook-endpoint`,
`wa-rs-production` (key custody).
