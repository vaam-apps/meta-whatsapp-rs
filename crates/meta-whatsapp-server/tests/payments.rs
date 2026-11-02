//! Payments (roadmap P2; docs/design/server.md, section 4.2): the payment
//! messages (`order_details`, `order_status`, scope `send`) and the
//! payment routes (scope `payments`): a WABA's payment configurations, a
//! payment's lookup and its refund. Each route's request is Meta's, with
//! the tenant's token, and each answer is read from Meta's documented
//! example (`payments/payments-in/onboarding-apis`,
//! `payments/payments-in/pg`, `payments/payments-br/orders`,
//! `payments/payments-br/offsite-pix`). Tenant B on tenant A's WABA or
//! number is M1.3's (`tests/authz.rs`, over every route of the document).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use common::{Call, Harness, Reply};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::core::ids::WabaId;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::idempotency::Fingerprint;
use meta_whatsapp_server::model::{
    IdempotencyClaim, IdempotencyKey, IdempotencyState, Scope, TenantId,
};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
/// The operator's own store: a tenant of the platform's WABA, attached
/// with the platform's system user token.
const OPERATOR: &str = "platform-store";
const OPERATOR_WABA: &str = "102290129340399";
const OPERATOR_PN: &str = "106540352242923";
const OPERATOR_TOKEN: &str = "EAAG-operator-system-user-token";
/// The examples' buyer (`payments-in/pg`, as E.164).
const TO: &str = "+919876543210";
const CONFIGURATION: &str = "test-payment-configuration";

async fn connected(scopes: &[Scope]) -> (Harness, String) {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, scopes).await;
    (h, key)
}

fn error(reply: &Reply) -> Value {
    reply.json()["error"].clone()
}

fn refused(reply: &Reply) -> (StatusCode, String, Option<String>) {
    let error = error(reply);
    (
        reply.status,
        error["code"].as_str().unwrap_or_default().to_owned(),
        error["field"].as_str().map(str::to_owned),
    )
}

fn send(pn: &str, key: &str, body: &Value) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{pn}/messages"))
        .key(key)
        .json(body)
}

/// messages/send-messages, "Example response".
fn accepted() -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": TO, "wa_id": "919876543210"}],
        "messages": [{"id": "wamid.HBgMOTE5ODc2NTQzMjEwFQIAERgSQ0ZCMDVFQ0Q1RDQ4MzdBNjM3AA=="}]
    })
}

/// `payments-in/pg`, step 1, the Razorpay catalog-based example's
/// `interactive` object (the catalog id and header link made real: the
/// page prints placeholders; its header image's `provider` is in no table
/// and not modelled).
fn razorpay_order_details() -> Value {
    json!({
      "type": "order_details",
      "header": {"type": "image", "image": {"link": "https://example.com/bread.jpg"}},
      "body": {"text": "your-text-body-content"},
      "footer": {"text": "your-text-footer-content"},
      "action": {
        "name": "review_and_pay",
        "parameters": {
          "reference_id": "reference-id-value",
          "type": "digital-goods",
          "payment_settings": [{"type": "payment_gateway", "payment_gateway": {
            "type": "razorpay",
            "configuration_name": "payment-config-id",
            "razorpay": {"receipt": "receipt-value", "notes": {"key1": "value1"}}
          }}],
          "currency": "INR",
          "total_amount": {"value": 21000, "offset": 100},
          "order": {
            "status": "pending",
            "catalog_id": "1234567890",
            "expiration": {"timestamp": "1767225600", "description": "cancellation-explanation"},
            "items": [{
              "retailer_id": "1234567",
              "name": "Product name, for example bread",
              "amount": {"value": 10000, "offset": 100},
              "quantity": 1,
              "sale_amount": {"value": 100, "offset": 100}
            }],
            "subtotal": {"value": 20000, "offset": 100},
            "tax": {"value": 1000, "offset": 100, "description": "optional_text"},
            "shipping": {"value": 1000, "offset": 100, "description": "optional_text"},
            "discount": {"value": 1000, "offset": 100, "description": "optional_text",
                         "discount_program_name": "optional_text"}
          }
        }
      }
    })
}

/// The Send API body Meta's pages print around an `interactive` object.
fn envelope(interactive: &Value) -> Value {
    json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": TO,
        "type": "interactive",
        "interactive": interactive
    })
}

fn interactive_send(interactive: &Value) -> Value {
    json!({"to": {"phone": TO}, "type": "interactive", "interactive": interactive})
}

/// P2's decisive case: an `order_details` send goes out with the tenant's
/// merchant token from the vault, never the operator's, though the
/// operator's own WABA token is in the same vault; the operator's own
/// number sends with its own. And the request is Meta's page's, byte for
/// byte. Decisive: the send through the number's capability
/// (`OwnedNumber::client`, `Client::with_token`).
#[tokio::test]
async fn an_order_details_send_carries_the_tenants_token_never_the_operators() {
    let (h, key) = connected(&[Scope::Send]).await;
    h.tenant(OPERATOR).await;
    h.connect(OPERATOR, OPERATOR_WABA, &[OPERATOR_PN], OPERATOR_TOKEN)
        .await;
    let operator_key = h.tenant_key(OPERATOR, &[Scope::Send]).await;
    h.graph.push_json(200, accepted());
    let reply = h
        .call(send(PN, &key, &interactive_send(&razorpay_order_details())))
        .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    assert_eq!(
        reply.json()["message_id"],
        "wamid.HBgMOTE5ODc2NTQzMjEwFQIAERgSQ0ZCMDVFQ0Q1RDQ4MzdBNjM3AA=="
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.path(), format!("/v25.0/{PN}/messages"));
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_ne!(request.bearer(), Some(OPERATOR_TOKEN));
    assert_eq!(request.json().unwrap(), envelope(&razorpay_order_details()));
    // The control: the operator's own number sends with its own token.
    h.graph.push_json(200, accepted());
    let own = h
        .call(send(
            OPERATOR_PN,
            &operator_key,
            &interactive_send(&razorpay_order_details()),
        ))
        .await;
    assert_eq!(own.status, StatusCode::ACCEPTED, "{}", own.text);
    assert_eq!(
        h.graph.last_request().unwrap().bearer(),
        Some(OPERATOR_TOKEN)
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// The other payment messages' examples, each sent as its page prints it:
/// India's `order_status` (`payments-in/pg`, step 6), Brazil's Pix order
/// in full and simplified (`payments-br/offsite-pix`) and Brazil's
/// `order_status` with its payment (`payments-br/orders`). And an inbox
/// reply carries one too. Decisive: the two interactive types in the send
/// union (out of it, each is `422 unsupported_message_type`).
#[tokio::test]
async fn the_payment_messages_are_sent_as_their_pages_print_them() {
    let (h, key) = connected(&[Scope::Send]).await;
    let pix = json!({"type": "pix_dynamic_code", "pix_dynamic_code": {
        "code": "00020101021226700014br.gov.bcb.pix2548pix.example.com...",
        "merchant_name": "Account holder name",
        "key": "39580525000189",
        "key_type": "CNPJ"
    }});
    let brl = |order: Option<Value>| {
        let mut parameters = json!({
          "reference_id": "unique-reference-id",
          "type": "digital-goods",
          "payment_type": "br",
          "payment_settings": [pix.clone()],
          "currency": "BRL",
          "total_amount": {"value": 50000, "offset": 100}
        });
        if let Some(order) = order {
            parameters["order"] = order;
        }
        json!({
          "type": "order_details",
          "body": {"text": "Your message content"},
          "action": {"name": "review_and_pay", "parameters": parameters}
        })
    };
    let brl_order = json!({
      "status": "pending",
      "tax": {"value": 0, "offset": 100, "description": "optional text"},
      "items": [{"retailer_id": "1234567", "name": "Cake",
                 "amount": {"value": 50000, "offset": 100}, "quantity": 1}],
      "subtotal": {"value": 50000, "offset": 100}
    });
    let examples = [
        json!({
          "type": "order_status",
          "body": {"text": "your-text-body-content"},
          "action": {"name": "review_order", "parameters": {
            "reference_id": "reference-id-value",
            "order": {"status": "processing", "description": "optional-text"}
          }}
        }),
        brl(Some(brl_order)),
        brl(None),
        json!({
          "type": "order_status",
          "body": {"text": "your-mandatory-text-body-content"},
          "footer": {"text": "your-optional-text-footer-content"},
          "action": {"name": "review_order", "parameters": {
            "reference_id": "unique-reference-id",
            "order": {"status": "processing"},
            "payment": {"status": "captured", "timestamp": 1_722_445_231}
          }}
        }),
    ];
    for example in &examples {
        h.graph.push_json(200, accepted());
        let reply = h.call(send(PN, &key, &interactive_send(example))).await;
        assert_eq!(
            reply.status,
            StatusCode::ACCEPTED,
            "{example}: {}",
            reply.text
        );
        let request = h.graph.last_request().unwrap();
        assert_eq!(request.bearer(), Some(TOKEN));
        assert_eq!(request.json().unwrap(), envelope(example), "{example}");
    }
    assert_eq!(h.graph.requests().len(), examples.len());
    assert_eq!(h.graph.remaining(), 0);
}

/// Every payment message is checked before any request, by the library's
/// rules for its currency and the service's for the object: nothing is
/// sent, `422` on the field at fault. Decisive: each refusal.
#[tokio::test]
async fn payment_messages_are_checked_before_any_request() {
    let (h, key) = connected(&[Scope::Send]).await;
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut interactive = razorpay_order_details();
        edit(&mut interactive);
        interactive_send(&interactive)
    };
    let cases: Vec<(Value, &str, &str)> = vec![
        // A currency the library has no rules for.
        (
            with(&|i| i["action"]["parameters"]["currency"] = json!("USD")),
            "invalid_request",
            "interactive.action.parameters",
        ),
        // A key the library would drop.
        (
            with(&|i| i["action"]["parameters"]["referense_id"] = json!("x")),
            "invalid_request",
            "interactive.action.parameters.referense_id",
        ),
        // The total equation (21000 = 20000 + 1000 + 1000 - 1000).
        (
            with(&|i| i["action"]["parameters"]["total_amount"]["value"] = json!(21001)),
            "invalid_request",
            "interactive.action.parameters.total_amount.value",
        ),
        (
            with(&|i| i["action"]["name"] = json!("cta_url")),
            "invalid_request",
            "interactive.action.name",
        ),
        (
            with(&|i| i["action"]["buttons"] = json!([])),
            "invalid_request",
            "interactive.action.buttons",
        ),
        (
            with(&|i| {
                i["header"] =
                    json!({"type": "video", "video": {"link": "https://example.com/v.mp4"}});
            }),
            "invalid_request",
            "interactive.header",
        ),
        (
            with(&|i| i["header"]["image"] = json!({"link": "http://example.com/bread.jpg"})),
            "invalid_request",
            "interactive.header.image.link",
        ),
        (
            with(&|i| {
                i["action"]["parameters"] = json!({"display_text": "Pay", "url": "https://x"});
            }),
            "invalid_request",
            "interactive.action.parameters",
        ),
        // An order status: India's `reference_id` alphabet, a header no
        // page documents, a key the library would drop.
        (
            interactive_send(&json!({"type": "order_status", "body": {"text": "b"},
                "action": {"name": "review_order", "parameters": {"reference_id": "a/b",
                "order": {"status": "processing"}}}})),
            "invalid_request",
            "interactive.action.parameters.reference_id",
        ),
        (
            interactive_send(&json!({"type": "order_status", "body": {"text": "b"},
                "header": {"type": "text", "text": "h"},
                "action": {"name": "review_order", "parameters": {"reference_id": "r1"}}})),
            "invalid_request",
            "interactive.header",
        ),
        (
            interactive_send(&json!({"type": "order_status", "body": {"text": "b"},
                "action": {"name": "review_order", "parameters": {"reference_id": "r1",
                "payment": {"status": "captured", "when": 1}}}})),
            "invalid_request",
            "interactive.action.parameters.payment.when",
        ),
        // A payment interactive the service does not send.
        (
            interactive_send(
                &json!({"type": "payment_method", "body": {"text": "b"}, "action": {}}),
            ),
            "unsupported_message_type",
            "",
        ),
    ];
    for (body, code, field) in cases {
        let reply = h.call(send(PN, &key, &body)).await;
        let (status, got, got_field) = refused(&reply);
        assert_eq!(
            (status, got.as_str(), got_field.unwrap_or_default().as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, code, field),
            "{body}: {}",
            reply.text
        );
    }
    assert!(h.graph.requests().is_empty(), "nothing was sent");
}

/// `payments-in/checkout-button-templates`, "Send a checkout button
/// template", the example request's `template` (its button `index` a
/// string, as every template button's here).
fn checkout_template() -> Value {
    let address = json!({
        "name": "Nidhi Tripathi", "phone_number": "919000090000",
        "address": "Bandra Kurla Complex", "city": "Mumbai", "state": "Maharastra",
        "in_pin_code": "400051", "house_number": "12", "tower_number": "5",
        "building_name": "One BKC", "landmark_area": "Near BKC Circle"
    });
    json!({
      "name": "item_back_in_stock_v2",
      "language": {"policy": "deterministic", "code": "en_US"},
      "components": [
        {"type": "header", "parameters": [{"type": "image", "image": {"id": "1558081531584829"}}]},
        {"type": "body", "parameters": [{"type": "text", "text": "Nidhi"},
                                        {"type": "text", "text": "Blue Elf Aloe"}]},
        {"type": "button", "sub_type": "order_details", "index": "0", "parameters": [
          {"type": "action", "action": {"order_details": {
            "reference_id": "abc.123_xyz-1",
            "type": "physical-goods",
            "currency": "INR",
            "payment_settings": [{"type": "payment_gateway", "payment_gateway": {
              "type": "razorpay", "configuration_name": "prod-razor-pay-config-05"}}],
            "shipping_info": {"country": "IN", "addresses": [address]},
            "order": {
              "items": [{
                "amount": {"offset": 100, "value": 200000},
                "sale_amount": {"offset": 100, "value": 150000},
                "name": "Blue Elf Aloe", "quantity": 1,
                "country_of_origin": "India",
                "importer_name": "Lucky Shrub Imports and Exports",
                "importer_address": {"address_line1": "One BKC",
                  "address_line2": "Bandra Kurla Complex", "city": "Mumbai",
                  "zone_code": "MH", "postal_code": "400051", "country_code": "IN"}
              }],
              "subtotal": {"offset": 100, "value": 150000},
              "shipping": {"offset": 100, "value": 20000},
              "tax": {"offset": 100, "value": 10000},
              "discount": {"offset": 100, "value": 15000, "description": "Additional 10% off"},
              "status": "pending",
              "expiration": {"timestamp": "1726627150"}
            },
            "total_amount": {"offset": 100, "value": 165000}
          }}}
        ]}
      ]
    })
}

/// Roadmap P1d through the service: a checkout button template's send
/// (its `shipping_info`, its expiration without a description) and a
/// third party validation setting reach Meta as their pages print them,
/// read through the library's types with no key dropped; what P1d keeps
/// out of a send is `422` before any request: a coupon (the checkout
/// endpoint's), `shipping_info` in an `order_details` message, and an
/// `order_details` message's expiration without its description.
#[tokio::test]
async fn checkout_templates_and_third_party_validation_are_sent_as_their_pages_print_them() {
    let (h, key) = connected(&[Scope::Send]).await;
    let template = checkout_template();
    h.graph.push_json(200, accepted());
    let body = json!({"to": {"phone": TO}, "type": "template", "template": template});
    let reply = h.call(send(PN, &key, &body)).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        request.json().unwrap(),
        json!({"messaging_product": "whatsapp", "recipient_type": "individual", "to": TO,
               "type": "template", "template": template})
    );
    // `payments-in/pg`, "Third party validation": a Razorpay setting.
    let mut tpv = razorpay_order_details();
    tpv["action"]["parameters"]["payment_settings"] = json!([{"type": "razorpay",
        "razorpay": {"encrypted_payment_gateway_data": "encrypted-data"}}]);
    h.graph.push_json(200, accepted());
    let reply = h.call(send(PN, &key, &interactive_send(&tpv))).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text);
    assert_eq!(
        h.graph.last_request().unwrap().json().unwrap(),
        envelope(&tpv)
    );
    let sent = h.graph.requests().len();

    let order = "template.components[2].parameters[0].action.order_details";
    let mut couponed = body.clone();
    couponed["template"]["components"][2]["parameters"][0]["action"]["order_details"]["coupon"] =
        json!({"code": "TRYNEW10", "discount": {"value": 16500, "offset": 100}});
    let mut shipped = razorpay_order_details();
    shipped["action"]["parameters"]["type"] = json!("physical-goods");
    shipped["action"]["parameters"]["shipping_info"] = json!({"country": "IN", "addresses": []});
    let mut undescribed = razorpay_order_details();
    undescribed["action"]["parameters"]["order"]["expiration"] = json!({"timestamp": "1767225600"});
    for (body, field) in [
        (couponed, format!("{order}.coupon")),
        (
            interactive_send(&shipped),
            "interactive.action.parameters.shipping_info".to_owned(),
        ),
        (
            interactive_send(&undescribed),
            "interactive.action.parameters.order.expiration.description".to_owned(),
        ),
    ] {
        let reply = h.call(send(PN, &key, &body)).await;
        assert_eq!(
            refused(&reply),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request".to_owned(),
                Some(field)
            ),
            "{body}: {}",
            reply.text
        );
        assert!(!reply.text.contains("encrypted-data"));
    }
    assert_eq!(h.graph.requests().len(), sent, "nothing more reached Meta");
    assert_eq!(h.graph.remaining(), 0);
}

/// `payments-in/onboarding-apis`: a configuration as Meta's sample
/// response prints it.
fn configuration() -> Value {
    json!({
      "configuration_name": CONFIGURATION,
      "merchant_category_code": {"code": "0000", "description": "Test MCC Code"},
      "purpose_code": {"code": "00", "description": "Test Purpose Code"},
      "status": "Active",
      "provider_mid": "test-payment-gateway-mid",
      "provider_name": "RazorPay",
      "created_timestamp": 1_720_203_204,
      "updated_timestamp": 1_721_088_316
    })
}

/// [`configuration`], as the service answers it.
fn configuration_answered() -> Value {
    json!({
      "configuration_name": CONFIGURATION,
      "merchant_category_code": {"code": "0000", "description": "Test MCC Code"},
      "purpose_code": {"code": "00", "description": "Test Purpose Code"},
      "status": "Active",
      "provider_mid": "test-payment-gateway-mid",
      "provider_name": "razorpay",
      "merchant_vpa": null,
      "created_timestamp": "2024-07-05T18:13:24Z",
      "updated_timestamp": "2024-07-16T00:05:16Z"
    })
}

const OAUTH_URL: &str = "https://www.facebook.com/payment/onboarding/init/";

/// Every configuration route is Meta's documented call, with the WABA's
/// token, reading Meta's sample responses (`payments-in/onboarding-apis`:
/// get all, get one, create a gateway's and a `upi_vpa` one, regenerate
/// the OAuth link, remove).
#[tokio::test]
#[allow(clippy::too_many_lines)] // one route after another
async fn the_configuration_routes_are_metas_calls_with_the_wabas_token() {
    let (h, key) = connected(&[Scope::Payments]).await;
    let base = format!("/v1/wabas/{WABA}/payment-configurations");
    // Get all.
    h.graph.push_json(
        200,
        json!({"data": [{"payment_configurations": [configuration()]}]}),
    );
    let listed = h.call(Call::get(&base).key(&key)).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text);
    assert_eq!(listed.json(), json!({"data": [configuration_answered()]}));
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        (request.method.clone(), request.path().to_owned()),
        (Method::GET, format!("/v25.0/{WABA}/payment_configurations"))
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    // Get one.
    h.graph.push_json(200, json!({"data": [configuration()]}));
    let one = h
        .call(Call::get(format!("{base}/{CONFIGURATION}")).key(&key))
        .await;
    assert_eq!(one.status, StatusCode::OK, "{}", one.text);
    assert_eq!(one.json(), configuration_answered());
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        request.path(),
        format!("/v25.0/{WABA}/payment_configuration/{CONFIGURATION}")
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    // Create: the gateway's sample request and response.
    let gateway = json!({
        "configuration_name": CONFIGURATION,
        "purpose_code": "00",
        "merchant_category_code": "0000",
        "provider_name": "razorpay",
        "redirect_url": "https://test-redirect-url.com"
    });
    h.graph.push_json(
        200,
        json!({"oauth_url": OAUTH_URL, "expiration": 1_721_687_287, "success": true}),
    );
    let created = h
        .call(Call::new(Method::POST, &base).key(&key).json(&gateway))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert_eq!(
        created.json(),
        json!({"oauth_url": OAUTH_URL, "expiration": "2024-07-22T22:28:07Z"})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        (request.method.clone(), request.path().to_owned()),
        (Method::POST, format!("/v25.0/{WABA}/payment_configuration"))
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(request.json().unwrap(), gateway);
    // Create: the UPI VPA one, whose answer has no link.
    let upi = json!({
        "configuration_name": CONFIGURATION,
        "purpose_code": "00",
        "merchant_category_code": "0000",
        "provider_name": "upi_vpa",
        "merchant_vpa": "test-upi-merchant-vpa@test"
    });
    h.graph.push_json(200, json!({"success": true}));
    let created = h
        .call(Call::new(Method::POST, &base).key(&key).json(&upi))
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert_eq!(
        created.json(),
        json!({"oauth_url": null, "expiration": null})
    );
    assert_eq!(h.graph.last_request().unwrap().json().unwrap(), upi);
    // Regenerate the OAuth link.
    h.graph.push_json(
        200,
        json!({"oauth_url": OAUTH_URL, "expiration": 1_721_687_287}),
    );
    let link = h
        .call(
            Call::new(Method::POST, format!("{base}/{CONFIGURATION}/oauth-link"))
                .key(&key)
                .json(&json!({"redirect_url": "https://test-redirect-url.com"})),
        )
        .await;
    assert_eq!(link.status, StatusCode::OK, "{}", link.text);
    assert_eq!(
        link.json(),
        json!({"oauth_url": OAUTH_URL, "expiration": "2024-07-22T22:28:07Z"})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        (request.method.clone(), request.path().to_owned()),
        (
            Method::POST,
            format!("/v25.0/{WABA}/generate_payment_configuration_oauth_link")
        )
    );
    assert_eq!(
        request.json().unwrap(),
        json!({"configuration_name": CONFIGURATION,
               "redirect_url": "https://test-redirect-url.com"})
    );
    // An empty body asks without a redirect.
    h.graph.push_json(200, json!({"oauth_url": OAUTH_URL}));
    let link = h
        .call(Call::new(Method::POST, format!("{base}/{CONFIGURATION}/oauth-link")).key(&key))
        .await;
    assert_eq!(link.status, StatusCode::OK, "{}", link.text);
    assert_eq!(
        h.graph.last_request().unwrap().json().unwrap(),
        json!({"configuration_name": CONFIGURATION})
    );
    // Remove.
    h.graph.push_json(200, json!({"success": true}));
    let deleted = h
        .call(Call::new(Method::DELETE, format!("{base}/{CONFIGURATION}")).key(&key))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        (request.method.clone(), request.path().to_owned()),
        (
            Method::DELETE,
            format!("/v25.0/{WABA}/payment_configuration")
        )
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(
        request.json().unwrap(),
        json!({"configuration_name": CONFIGURATION})
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// A configuration that is not the WABA's is `404 not_found`, whether Meta
/// answers an empty `data` or refuses the name, without Meta's code; a bad
/// name or a provider the page does not list is `422`, nothing asked.
#[tokio::test]
async fn a_configuration_not_in_the_waba_is_not_found() {
    let (h, key) = connected(&[Scope::Payments]).await;
    let path = format!("/v1/wabas/{WABA}/payment-configurations/{CONFIGURATION}");
    h.graph.push_json(200, json!({"data": []}));
    let missing = h.call(Call::get(&path).key(&key)).await;
    assert_eq!(
        (missing.status, error(&missing)["code"].clone()),
        (StatusCode::NOT_FOUND, json!("not_found"))
    );
    h.graph.push_json(
        400,
        json!({"error": {"message": "(#100) Invalid parameter", "type": "OAuthException", "code": 100}}),
    );
    let refused_name = h.call(Call::get(&path).key(&key)).await;
    assert_eq!(
        refused_name.status,
        StatusCode::NOT_FOUND,
        "{}",
        refused_name.text
    );
    assert!(error(&refused_name)["graph"].is_null());
    let asked = h.graph.requests().len();
    for (call, field) in [
        (
            Call::get(format!("/v1/wabas/{WABA}/payment-configurations/%2E%2E")).key(&key),
            "configuration_name",
        ),
        (
            Call::new(
                Method::POST,
                format!("/v1/wabas/{WABA}/payment-configurations"),
            )
            .key(&key)
            .json(&json!({"configuration_name": CONFIGURATION, "provider_name": "stripe"})),
            "provider_name",
        ),
        (
            Call::new(
                Method::POST,
                format!("/v1/wabas/{WABA}/payment-configurations"),
            )
            .key(&key)
            .json(
                &json!({"configuration_name": CONFIGURATION, "provider_name": "payu",
                              "success": true}),
            ),
            "body",
        ),
    ] {
        let reply = h.call(call).await;
        assert_eq!(
            refused(&reply),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request".to_owned(),
                Some(field.to_owned())
            ),
            "{}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), asked, "nothing asked");
}

/// `payments-in/pg`, step 5's example of a successful lookup, its
/// placeholders made real.
fn payment() -> Value {
    json!({"payments": [{
        "reference_id": "reference-id-value",
        "status": "captured",
        "currency": "INR",
        "amount": {"value": 21000, "offset": 100},
        "transactions": [{
            "id": "order_PG1",
            "pg_transaction_id": "pay_PG1",
            "type": "razorpay",
            "status": "success",
            "created_timestamp": 1_720_203_204,
            "updated_timestamp": 1_720_203_300,
            "method": {"type": "upi"},
            "refunds": [{"id": "rfnd_1", "amount": {"value": 100, "offset": 100},
                         "speed_processed": "instant", "status": "success",
                         "created_timestamp": 1_720_203_400, "updated_timestamp": 1_720_203_500}]
        }],
        "receipt": "receipt-value",
        "notes": {"key1": "value1"}
    }]})
}

/// A lookup looks the configuration up in the number's WABA, then the
/// payment under the number, with the number's token; the answer is
/// Meta's.
#[tokio::test]
async fn a_lookup_finds_the_configuration_in_the_numbers_waba_first() {
    let (h, key) = connected(&[Scope::Payments]).await;
    h.graph.push_json(200, json!({"data": [configuration()]}));
    h.graph.push_json(200, payment());
    let reply = h
        .call(
            Call::get(format!(
                "/v1/numbers/{PN}/payments/{CONFIGURATION}/reference-id-value"
            ))
            .key(&key),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text);
    let answered = reply.json();
    assert_eq!(answered["data"][0]["status"], "captured");
    assert_eq!(answered["data"][0]["transactions"][0]["type"], "razorpay");
    assert_eq!(
        answered["data"][0]["transactions"][0]["refunds"][0]["created_timestamp"],
        "2024-07-05T18:16:40Z"
    );
    assert_eq!(answered["data"][0]["notes"], json!({"key1": "value1"}));
    let requests = h.graph.requests();
    assert_eq!(
        requests
            .iter()
            .map(|r| r.path().to_owned())
            .collect::<Vec<_>>(),
        [
            format!("/v25.0/{WABA}/payment_configuration/{CONFIGURATION}"),
            format!("/v25.0/{PN}/payments/{CONFIGURATION}/reference-id-value"),
        ]
    );
    assert!(requests.iter().all(|r| r.bearer() == Some(TOKEN)));
    // Meta's own lookup error (not a Graph error) is `502 upstream`,
    // without its text.
    h.graph.push_json(200, json!({"data": [configuration()]}));
    h.graph.push_json(
        500,
        json!({"errors": [{"code": 500, "title": "Generic error",
                           "details": "System error. Please try again."}]}),
    );
    let failed = h
        .call(
            Call::get(format!(
                "/v1/numbers/{PN}/payments/{CONFIGURATION}/reference-id-value"
            ))
            .key(&key),
        )
        .await;
    assert_eq!(error(&failed)["code"], "upstream", "{}", failed.text);
    assert!(!failed.text.contains("System error"), "{}", failed.text);
    assert_eq!(h.graph.remaining(), 0);
}

fn refund_body() -> Value {
    json!({
      "reference_id": "reference-id-value",
      "speed": "normal",
      "payment_config_id": CONFIGURATION,
      "amount": {"currency": "INR", "value": "100", "offset": "100"}
    })
}

/// A refund under a fresh `Idempotency-Key` (a refund requires one: D56).
fn refund(key: &str, body: &Value) -> Call {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let fresh = format!("refund:fresh:{}", NEXT.fetch_add(1, Ordering::Relaxed));
    refund_keyed(key, &fresh, body)
}

/// A refund under `idempotency_key`.
fn refund_keyed(key: &str, idempotency_key: &str, body: &Value) -> Call {
    keyless_refund(key, body).header("idempotency-key", idempotency_key)
}

/// A refund without an `Idempotency-Key`.
fn keyless_refund(key: &str, body: &Value) -> Call {
    Call::new(Method::POST, format!("/v1/numbers/{PN}/payments/refunds"))
        .key(key)
        .json(body)
}

/// The tenant-isolation case of a number-scoped route (design M5.2): a
/// lookup or a refund naming a configuration that is not the number's
/// WABA's (one token may reach other tenants' WABAs) is `404 not_found`,
/// and the payment is neither read nor refunded: the one request is the
/// check, on the WABA's own path. Decisive: the check
/// (`Payments::configuration_of_the_waba`).
#[tokio::test]
async fn another_wabas_configuration_is_not_found_and_nothing_is_read_or_refunded() {
    let (h, key) = connected(&[Scope::Payments]).await;
    for call in [
        Call::get(format!(
            "/v1/numbers/{PN}/payments/another-tenants-configuration/reference-id-value"
        ))
        .key(&key),
        refund(
            &key,
            &json!({"reference_id": "reference-id-value",
                    "payment_config_id": "another-tenants-configuration",
                    "amount": {"currency": "INR", "value": 100, "offset": 100}}),
        ),
    ] {
        let before = h.graph.requests().len();
        h.graph.push_json(200, json!({"data": []}));
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, error(&reply)["code"].clone()),
            (StatusCode::NOT_FOUND, json!("not_found")),
            "{}",
            reply.text
        );
        let requests = h.graph.requests();
        assert_eq!(requests.len(), before + 1, "only the check");
        assert_eq!(
            requests.last().unwrap().path(),
            format!("/v25.0/{WABA}/payment_configuration/another-tenants-configuration")
        );
    }
    assert_eq!(h.graph.remaining(), 0);
}

/// A refund (`payments-in/pg`, step 8) is Meta's request, under an
/// `Idempotency-Key` it is never made twice: a timeout is `504` with
/// `may_have_been_sent`, and the same key replays that with no second
/// request; a refund that went through replays its answer; another body
/// under a key is `422 idempotency_key_reused`. Decisive: the key on the
/// refund route, and the library never replaying the refund.
#[tokio::test]
async fn a_refund_is_never_made_twice() {
    let (h, key) = connected(&[Scope::Payments]).await;
    // Through.
    h.graph.push_json(200, json!({"data": [configuration()]}));
    h.graph.push_json(
        200,
        json!({"id": "refund-id", "status": "pending", "speed_processed": "normal"}),
    );
    let made = h.call(refund_keyed(&key, "refund:1", &refund_body())).await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.text);
    assert_eq!(
        made.json(),
        json!({"id": "refund-id", "status": "pending", "speed_processed": "normal"})
    );
    let request = h.graph.last_request().unwrap();
    assert_eq!(
        (request.method.clone(), request.path().to_owned()),
        (Method::POST, format!("/v25.0/{PN}/payments_refund"))
    );
    assert_eq!(request.bearer(), Some(TOKEN));
    assert_eq!(request.json().unwrap(), refund_body());
    let again = h.call(refund_keyed(&key, "refund:1", &refund_body())).await;
    assert_eq!(again.status, StatusCode::CREATED);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.text, made.text);
    let mut other = refund_body();
    other["amount"]["value"] = json!("200");
    let reused = h.call(refund_keyed(&key, "refund:1", &other)).await;
    assert_eq!(error(&reused)["code"], "idempotency_key_reused");
    assert_eq!(h.graph.requests().len(), 2, "one check, one refund");
    // Timed out.
    h.graph.push_json(200, json!({"data": [configuration()]}));
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h.call(refund_keyed(&key, "refund:2", &refund_body())).await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    assert_eq!(error(&cut)["may_have_been_sent"], true);
    let repeated = h.call(refund_keyed(&key, "refund:2", &refund_body())).await;
    assert_eq!(repeated.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(repeated.headers["idempotent-replayed"], "true");
    assert_eq!(h.graph.requests().len(), 4, "no second refund");
    // Checked first: nothing asked.
    let mut bad = refund_body();
    bad["amount"]["offset"] = json!("10");
    let refused_offset = h.call(refund(&key, &bad)).await;
    assert_eq!(
        refused(&refused_offset),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request".to_owned(),
            Some("amount.offset".to_owned())
        )
    );
    assert_eq!(h.graph.requests().len(), 4);
    assert_eq!(h.graph.remaining(), 0);
}

/// The OAuth link is a credential: the caller gets it, the service's
/// idempotency records never hold it, and a repeat under the same key
/// answers the creation without it. Decisive: the kept answer without the
/// link (`Success::json_keeping`).
#[tokio::test]
async fn a_created_configurations_oauth_link_is_answered_never_kept() {
    let (h, key) = connected(&[Scope::Payments]).await;
    let secret_link = "https://www.facebook.com/payment/onboarding/init/?token=OAUTH-LINK-SECRET";
    let body = json!({"configuration_name": CONFIGURATION, "provider_name": "razorpay"});
    let create = || {
        Call::new(
            Method::POST,
            format!("/v1/wabas/{WABA}/payment-configurations"),
        )
        .key(&key)
        .header("idempotency-key", "configuration:1")
        .json(&body)
    };
    h.graph.push_json(
        200,
        json!({"oauth_url": secret_link, "expiration": 1_721_687_287, "success": true}),
    );
    let created = h.call(create()).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text);
    assert_eq!(created.json()["oauth_url"], secret_link);
    assert_eq!(created.headers["cache-control"], "no-store", "a credential");
    let again = h.call(create()).await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.text);
    assert_eq!(again.headers["idempotent-replayed"], "true");
    assert_eq!(again.headers["cache-control"], "no-store");
    assert_eq!(
        again.json(),
        json!({"oauth_url": null, "expiration": "2024-07-22T22:28:07Z"})
    );
    assert_eq!(h.graph.requests().len(), 1, "created once");
    // What the records keep.
    let fingerprint = Fingerprint::create_payment_configuration(&WabaId::new(WABA).unwrap(), &body);
    let claim = h
        .store
        .claim_idempotency_key(
            &TenantId::parse(TENANT).unwrap(),
            &IdempotencyKey::parse("configuration:1").unwrap(),
            fingerprint.as_bytes(),
            "probe",
            Duration::from_secs(60),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    let IdempotencyClaim::Existing(record) = claim else {
        panic!("the key was kept");
    };
    let IdempotencyState::Completed { status, body } = record.state else {
        panic!("completed");
    };
    assert_eq!(status, 201);
    let kept = String::from_utf8(body).unwrap();
    assert!(!kept.contains("OAUTH-LINK-SECRET"), "{kept}");
    // A new link is answered, and not kept either.
    h.graph.push_json(200, json!({"oauth_url": secret_link}));
    let link = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/wabas/{WABA}/payment-configurations/{CONFIGURATION}/oauth-link"),
            )
            .key(&key)
            .header("idempotency-key", "link:1"),
        )
        .await;
    assert_eq!(link.json()["oauth_url"], secret_link, "{}", link.text);
    assert_eq!(link.headers["cache-control"], "no-store", "a credential");
    let repeat = h
        .call(
            Call::new(
                Method::POST,
                format!("/v1/wabas/{WABA}/payment-configurations/{CONFIGURATION}/oauth-link"),
            )
            .key(&key)
            .header("idempotency-key", "link:1")
            .json(&json!({})),
        )
        .await;
    assert_eq!(
        repeat.headers["idempotent-replayed"], "true",
        "{}",
        repeat.text
    );
    assert_eq!(repeat.json()["oauth_url"], Value::Null);
    assert_eq!(h.graph.requests().len(), 2);
    assert_eq!(h.graph.remaining(), 0);
}

/// The payment routes take the `payments` scope, and the payment messages
/// `send`: neither stands in for the other. Decisive: the payment
/// operations' scope (`Operation::scope`).
#[tokio::test]
async fn the_payment_routes_take_the_payments_scope() {
    let (h, send_key) = connected(&[Scope::Send, Scope::Numbers, Scope::Templates]).await;
    let payments_key = h.tenant_key(TENANT, &[Scope::Payments]).await;
    for call in [
        Call::get(format!("/v1/wabas/{WABA}/payment-configurations")),
        Call::get(format!("/v1/numbers/{PN}/payments/{CONFIGURATION}/r1")),
        Call::new(Method::POST, format!("/v1/numbers/{PN}/payments/refunds")).json(&refund_body()),
    ] {
        let reply = h.call(call.key(&send_key)).await;
        assert_eq!(
            (reply.status, error(&reply)["code"].clone()),
            (StatusCode::FORBIDDEN, json!("forbidden")),
            "{}",
            reply.text
        );
    }
    let order = h
        .call(send(
            PN,
            &payments_key,
            &interactive_send(&razorpay_order_details()),
        ))
        .await;
    assert_eq!(order.status, StatusCode::FORBIDDEN, "{}", order.text);
    assert!(h.graph.requests().is_empty());
}

/// No legal act: until the WABA accepts the WhatsApp Payments terms of
/// service, Meta refuses with `131005` (`payments-in/onboarding-apis`,
/// "Errors"): `403 permission`, with Meta's `details` carrying the link
/// the merchant opens; the service makes one request, and accepts
/// nothing.
#[tokio::test]
async fn the_terms_of_service_refusal_carries_metas_link_and_nothing_is_accepted() {
    let (h, key) = connected(&[Scope::Payments]).await;
    h.graph.push_json(
        403,
        json!({"error": {
            "message": "(#131005) Access denied",
            "type": "OAuthException",
            "code": 131_005,
            "error_data": {
                "messaging_product": "whatsapp",
                "details": "WhatsApp Payments Terms of Service acceptance pending for this WhatsApp Business Account. Please use the following link to accept terms of service before using Business APIs: https://fb.me/12345"
            }
        }}),
    );
    let reply = h
        .call(Call::get(format!("/v1/wabas/{WABA}/payment-configurations")).key(&key))
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text);
    let error = error(&reply);
    assert_eq!(error["code"], "permission");
    assert!(
        error["graph"]["details"]
            .as_str()
            .unwrap()
            .ends_with("https://fb.me/12345"),
        "{error}"
    );
    assert_eq!(h.graph.requests().len(), 1);
    assert_eq!(h.graph.remaining(), 0);
}

/// Design D51 (review of P2): the configuration check trusts only an
/// answer of the very name asked; one Meta answers under another name (a
/// match by case, or anything else) is `404 not_found` and nothing is read
/// or refunded. A name holding `/` or `\` (which Meta could read as
/// several path segments once it decodes the library's `%2F`) is `422` on
/// its field before any request, in a path or in the refund's body.
/// Decisive: the name's comparison in `Payments::configuration_of_the_waba`,
/// and the separators in `payments::configuration_name`.
#[tokio::test]
async fn a_configuration_meta_answers_under_another_name_is_not_found() {
    let (h, key) = connected(&[Scope::Payments]).await;
    let asked = "Test-Payment-Configuration";
    for call in [
        Call::get(format!(
            "/v1/numbers/{PN}/payments/{asked}/reference-id-value"
        ))
        .key(&key),
        refund(
            &key,
            &json!({"reference_id": "reference-id-value", "payment_config_id": asked,
                    "amount": {"currency": "INR", "value": "100", "offset": "100"}}),
        ),
    ] {
        let before = h.graph.requests().len();
        // Meta answers the configuration of another name.
        h.graph.push_json(200, json!({"data": [configuration()]}));
        let reply = h.call(call).await;
        assert_eq!(
            (reply.status, error(&reply)["code"].clone()),
            (StatusCode::NOT_FOUND, json!("not_found")),
            "{}",
            reply.text
        );
        let requests = h.graph.requests();
        assert_eq!(requests.len(), before + 1, "only the check");
        assert_eq!(
            requests.last().unwrap().path(),
            format!("/v25.0/{WABA}/payment_configuration/{asked}")
        );
    }
    let sent = h.graph.requests().len();
    for (call, field) in [
        (
            Call::get(format!(
                "/v1/numbers/{PN}/payments/x%2F..%2F..%2F{WABA}/reference-id-value"
            ))
            .key(&key),
            "configuration_name",
        ),
        (
            Call::get(format!(
                "/v1/wabas/{WABA}/payment-configurations/a%5C..%5Cb"
            ))
            .key(&key),
            "configuration_name",
        ),
        (
            Call::new(
                Method::DELETE,
                format!("/v1/wabas/{WABA}/payment-configurations/a%2Fb"),
            )
            .key(&key),
            "configuration_name",
        ),
        (
            Call::new(
                Method::POST,
                format!("/v1/wabas/{WABA}/payment-configurations/..%2Fx/oauth-link"),
            )
            .key(&key),
            "configuration_name",
        ),
        (
            Call::new(
                Method::POST,
                format!("/v1/wabas/{WABA}/payment-configurations"),
            )
            .key(&key)
            .json(&json!({"configuration_name": "a/b", "provider_name": "razorpay"})),
            "configuration_name",
        ),
        (
            refund(
                &key,
                &json!({"reference_id": "reference-id-value",
                        "payment_config_id": format!("../{CONFIGURATION}"),
                        "amount": {"currency": "INR", "value": "100", "offset": "100"}}),
            ),
            "payment_config_id",
        ),
    ] {
        let reply = h.call(call).await;
        assert_eq!(
            refused(&reply),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request".to_owned(),
                Some(field.to_owned())
            ),
            "{}",
            reply.text
        );
    }
    assert_eq!(h.graph.requests().len(), sent, "nothing asked of Meta");
    assert_eq!(h.graph.remaining(), 0);
}

/// The amounts [`a_refunds_amount_is_checked_before_any_request`] refuses,
/// each with the field it is refused on.
fn refused_amounts() -> Vec<(Value, &'static str)> {
    vec![
        (
            json!({"currency": "INR", "value": -100, "offset": 100}),
            "body",
        ),
        (
            json!({"currency": "INR", "value": 1.5, "offset": 100}),
            "body",
        ),
        (
            json!({"currency": "INR", "value": 1e30, "offset": 100}),
            "body",
        ),
        (
            json!({"currency": "INR", "value": null, "offset": 100}),
            "body",
        ),
        (
            json!({"currency": "INR", "value": 0, "offset": 100}),
            "amount.value",
        ),
        (
            json!({"currency": "INR", "value": "0", "offset": "100"}),
            "amount.value",
        ),
        (
            json!({"currency": "INR", "value": "", "offset": "100"}),
            "amount.value",
        ),
        (
            json!({"currency": "INR", "value": "-100", "offset": "100"}),
            "amount.value",
        ),
        (
            json!({"currency": "INR", "value": "+100", "offset": "100"}),
            "amount.value",
        ),
        (
            json!({"currency": "INR", "value": "1 00", "offset": "100"}),
            "amount.value",
        ),
        (
            json!({"currency": "INR", "value": "18446744073709551616", "offset": "100"}),
            "amount.value",
        ),
        (
            json!({"currency": "INR", "value": "100", "offset": "1"}),
            "amount.offset",
        ),
        (
            json!({"currency": "INR", "value": 100, "offset": 4_294_967_396_u64}),
            "amount.offset",
        ),
        (
            json!({"currency": "inr", "value": "100", "offset": "100"}),
            "amount.currency",
        ),
        (
            json!({"currency": "USD", "value": "100", "offset": "100"}),
            "amount.currency",
        ),
        (json!({"currency": "INR", "value": "100"}), "body"),
    ]
}

/// Design D56 (review of P2): a refund's amount over HTTP, as a caller
/// writes it, is checked before any request: a negative, fractional, zero,
/// empty, signed, spaced or larger-than-64-bit `value`, an `offset` that
/// is not 100, a currency that is not `INR` (in any other case), a missing
/// amount; each is `422`, nothing refunded, nothing asked. Decisive: the
/// parts' reading (`RefundAmountPart`, `AmountPart::value`), the offset
/// and currency checks, and the library's positive amount.
#[tokio::test]
async fn a_refunds_amount_is_checked_before_any_request() {
    let (h, key) = connected(&[Scope::Payments]).await;
    let with = |amount: Value| {
        json!({"reference_id": "reference-id-value", "payment_config_id": CONFIGURATION,
               "amount": amount})
    };
    let cases = refused_amounts();
    for (amount, field) in cases {
        let reply = h
            .call(refund_keyed(&key, "refund:checked", &with(amount.clone())))
            .await;
        assert_eq!(
            refused(&reply),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request".to_owned(),
                Some(field.to_owned())
            ),
            "{amount}: {}",
            reply.text
        );
    }
    let missing = h
        .call(refund(
            &key,
            &json!({"reference_id": "reference-id-value", "payment_config_id": CONFIGURATION}),
        ))
        .await;
    assert_eq!(
        missing.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        missing.text
    );
    assert!(h.graph.requests().is_empty(), "nothing asked of Meta");
    // The key was never kept: the largest amount that fits goes under it.
    h.graph.push_json(200, json!({"data": [configuration()]}));
    h.graph.push_json(
        200,
        json!({"id": "refund-id", "status": "failed", "speed_processed": "normal"}),
    );
    let largest = h
        .call(refund_keyed(
            &key,
            "refund:checked",
            &with(json!({"currency": "INR", "value": u64::MAX, "offset": 100})),
        ))
        .await;
    assert_eq!(largest.status, StatusCode::CREATED, "{}", largest.text);
    assert_eq!(
        h.graph.last_request().unwrap().json().unwrap()["amount"],
        json!({"currency": "INR", "value": u64::MAX.to_string(), "offset": "100"})
    );
    assert_eq!(h.graph.remaining(), 0);
}

/// An inbox reply (scope `inbox`, roadmap M2a) carries a payment message
/// as a send does (P2): the conversation's contact as its recipient, the
/// tenant's token, Meta's object as the page prints it; checked the same
/// way before any request (a key the library would drop is `422`); and
/// held to the 24-hour window like every free-form message (`409
/// customer_service_window_closed`, zero requests). Decisive: the reply's
/// path through the core's `reply_message`, and the inbox's window.
#[tokio::test]
async fn an_inbox_reply_carries_a_payment_message_checked_and_within_its_window() {
    use common::inbox::{TO as CONTACT, TOKEN as INBOX_TOKEN, accepted as replied};
    use common::meta::{EXAMPLE_PN, EXAMPLE_WA_ID, EXAMPLE_WABA, EXAMPLE_WAMID, bytes, text};
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, EXAMPLE_WABA, &[EXAMPLE_PN], INBOX_TOKEN)
        .await;
    let received = h
        .webhook(&bytes(&text(EXAMPLE_WABA, EXAMPLE_PN, EXAMPLE_WAMID)))
        .await;
    assert_eq!(received.status, StatusCode::OK, "{}", received.text);
    h.clock
        .set(time::OffsetDateTime::now_utc() + time::Duration::seconds(2));
    let key = h.tenant_key(TENANT, &[Scope::Inbox]).await;
    let reply_with = |idempotency_key: &str, interactive: &Value| {
        Call::new(
            Method::POST,
            format!("/v1/numbers/{EXAMPLE_PN}/conversations/{EXAMPLE_WA_ID}/messages"),
        )
        .key(&key)
        .header("idempotency-key", idempotency_key)
        .json(&json!({"type": "interactive", "interactive": interactive}))
    };
    h.graph.push_json(200, replied("wamid.ORDER-REPLY"));
    let sent = h
        .call(reply_with("order:1:details", &razorpay_order_details()))
        .await;
    assert_eq!(sent.status, StatusCode::ACCEPTED, "{}", sent.text);
    let request = h.graph.last_request().unwrap();
    assert_eq!(request.path(), format!("/v25.0/{EXAMPLE_PN}/messages"));
    assert_eq!(request.bearer(), Some(INBOX_TOKEN));
    let mut expected = envelope(&razorpay_order_details());
    expected["to"] = json!(CONTACT);
    assert_eq!(request.json().unwrap(), expected);
    let mut dropped = razorpay_order_details();
    dropped["action"]["parameters"]["referense_id"] = json!("x");
    let refused_key = h.call(reply_with("order:1:dropped", &dropped)).await;
    assert_eq!(
        refused(&refused_key),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request".to_owned(),
            Some("interactive.action.parameters.referense_id".to_owned())
        ),
        "{}",
        refused_key.text
    );
    let mut wrong_total = razorpay_order_details();
    wrong_total["action"]["parameters"]["total_amount"]["value"] = json!(21001);
    let refused_total = h.call(reply_with("order:1:total", &wrong_total)).await;
    assert_eq!(
        refused(&refused_total).2.as_deref(),
        Some("interactive.action.parameters.total_amount.value"),
        "{}",
        refused_total.text
    );
    assert_eq!(h.graph.requests().len(), 1);
    // 25 hours on: the window is closed for an order as for a text.
    h.clock
        .set(time::OffsetDateTime::now_utc() + time::Duration::hours(25));
    let status = json!({"type": "order_status", "body": {"text": "your-text-body-content"},
        "action": {"name": "review_order", "parameters": {"reference_id": "reference-id-value",
        "order": {"status": "processing", "description": "optional-text"}}}});
    for (idempotency_key, interactive) in [
        ("order:1:late", razorpay_order_details()),
        ("order:1:status", status),
    ] {
        let closed = h.call(reply_with(idempotency_key, &interactive)).await;
        assert_eq!(
            (closed.status, closed.code().as_str()),
            (StatusCode::CONFLICT, "customer_service_window_closed"),
            "{}",
            closed.text
        );
    }
    assert_eq!(h.graph.requests().len(), 1, "refused with zero requests");
    assert_eq!(h.graph.remaining(), 0);
}

/// Review of P2: a refund whose configuration check failed (Meta timed
/// out or failed on that read) was never sent: it is answered with
/// `may_have_been_sent: false` and its `Idempotency-Key` is released, not
/// kept as a refund that may have been made, so the same key refunds once
/// Meta answers. Decisive: the check's error marked not sent
/// (`Payments::configuration_of_the_waba`).
#[tokio::test]
async fn a_refund_whose_check_failed_was_not_sent_and_frees_its_key() {
    let (h, key) = connected(&[Scope::Payments]).await;
    let keyed = || refund_keyed(&key, "refund:checked-first", &refund_body());
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h.call(keyed()).await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    assert_eq!(error(&cut)["may_have_been_sent"], false, "{}", cut.text);
    h.graph.push_json(
        500,
        json!({"error": {"message": "An unexpected error has occurred. Please retry your request later.",
                         "type": "OAuthException", "code": 2, "is_transient": true}}),
    );
    let failed = h.call(keyed()).await;
    assert!(failed.status.is_server_error(), "{}", failed.text);
    assert_eq!(
        error(&failed)["may_have_been_sent"],
        false,
        "{}",
        failed.text
    );
    assert!(failed.headers.get("idempotent-replayed").is_none());
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 2, "the checks only");
    assert!(requests.iter().all(|r| r.method == Method::GET));
    // The key was released each time: it refunds now, once.
    h.graph.push_json(200, json!({"data": [configuration()]}));
    h.graph.push_json(
        200,
        json!({"id": "refund-id", "status": "pending", "speed_processed": "normal"}),
    );
    let made = h.call(keyed()).await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.text);
    assert!(made.headers.get("idempotent-replayed").is_none());
    assert_eq!(
        h.graph.last_request().unwrap().path(),
        format!("/v25.0/{PN}/payments_refund")
    );
    assert_eq!(h.graph.requests().len(), 4);
    assert_eq!(h.graph.remaining(), 0);
}

/// D56, as amended on 2026-11-02 (the coordinator's decision): money moves
/// only under an `Idempotency-Key`. A refund without one is `428
/// idempotency_key_required`, `may_have_been_sent: false`, whatever its
/// body, and nothing is asked of Meta, not even the configuration check.
/// Decisive: `payments::refund_key` in the refund's handler.
#[tokio::test]
async fn a_refund_without_an_idempotency_key_sends_nothing() {
    let (h, key) = connected(&[Scope::Payments]).await;
    for body in [refund_body(), json!({"reference_id": "a/b"})] {
        let reply = h.call(keyless_refund(&key, &body)).await;
        assert_eq!(
            (reply.status.as_u16(), error(&reply)["code"].clone()),
            (428, json!("idempotency_key_required")),
            "{}",
            reply.text
        );
        assert_eq!(error(&reply)["may_have_been_sent"], false, "{}", reply.text);
    }
    assert!(h.graph.requests().is_empty(), "nothing asked of Meta");
    assert_eq!(h.graph.remaining(), 0);
}
