//! Request and response bodies are the examples of the payments pages
//! (named above each test). Placeholders the examples leave unfilled
//! (`utc_timestamp_in_seconds`, `CREATED_TIMESTAMP`, `PHONE_NUMBER`, a
//! value that lists its alternatives with `/` or `|`) are filled with a
//! plausible value; everything else is copied.

use std::time::Duration;

use http::Method;
use meta_whatsapp_core::error::TransportError;
use meta_whatsapp_core::ids::{MediaId, PhoneNumberId, WabaId};
use meta_whatsapp_core::recipient::Recipient;
use meta_whatsapp_core::testing::ScriptedTransport;
use meta_whatsapp_core::{Error, ErrorKind};
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::*;
use crate::common::MediaSource;
use crate::messages::{MessageContent, OutboundMessage};
use crate::{Client, RetryPolicy};
use meta_whatsapp_core::ids::CatalogId;

/// Brazil's pages and rules (roadmap P1b), on this module's helpers.
mod brazil;
/// The payment templates (roadmap P1c), on this module's helpers.
mod templates;

const PHONE_ID: &str = "106540352242922";
const WABA: &str = "102290129340398";
const TO: &str = "919876543210";
/// `the-catalog_id` in the examples.
const CATALOG: &str = "1234567890";
/// `your-media-id` in the examples.
const MEDIA: &str = "1166846181421424";
/// `utc_timestamp_in_seconds` in the examples.
const EXPIRES: u64 = 1_767_225_600;

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

/// A client that would replay a transient failure twice, for the
/// never-replayed tests.
fn retrying_client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy {
            max_retries: 2,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        })
        .build()
        .unwrap()
}

/// The send response every payments page prints (step "Make a POST call
/// to messages endpoint").
fn send_response() -> Value {
    json!({
      "messaging_product": "whatsapp",
      "contacts": [{"input": "[PHONE_NUMBER_ID]", "wa_id": "[PHONE-NUMBER_ID]"}],
      "messages": [{"id": "wamid.HBgLMTY1MDUwNzY1MjAVAgARGBI5QTNDQTVCM0Q0Q0Q2RTY3RTcA"}]
    })
}

/// Send `content` and return the body that went out, after checking the
/// request's method, path and token, the answer, and that nothing else
/// was sent.
#[track_caller]
fn sent(content: impl Into<MessageContent>) -> Value {
    let message = OutboundMessage::new(Recipient::phone(TO), content);
    if let Err(e) = message.validate() {
        panic!("documented example failed validation: {e}");
    }
    let t = ScriptedTransport::new();
    t.push_json(200, send_response());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let response = rt
        .block_on(
            client(&t)
                .messages(PhoneNumberId::new(PHONE_ID).unwrap())
                .send(&message),
        )
        .unwrap();
    assert_eq!(
        response.message_id().map(|m| m.as_str().to_owned()),
        Some("wamid.HBgLMTY1MDUwNzY1MjAVAgARGBI5QTNDQTVCM0Q0Q0Q2RTY3RTcA".to_owned())
    );
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{PHONE_ID}/messages"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(t.remaining(), 0);
    req.json().unwrap()
}

/// The Send API envelope around an example's `interactive` object (each
/// page's step "Add common message parameters").
fn envelope(interactive: &Value) -> Value {
    json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": TO,
        "type": "interactive",
        "interactive": interactive
    })
}

/// The importer address of the examples' items.
fn importer_address() -> ImporterAddress {
    ImporterAddress::new("B8/733 nand nagri", "East Delhi", "110093", "IN")
        .address_line2("police station")
        .zone_code("DL")
}

/// The examples' non-catalog item, twice.
fn non_catalog_items() -> [OrderItem; 2] {
    let item = || {
        OrderItem::new("Product name, for example bread", Amount::inr(10000), 1)
            .sale_amount(Amount::inr(100))
            .import(
                "country-of-origin",
                "name-of-importer-business",
                importer_address(),
            )
    };
    [item(), item()]
}

fn non_catalog_items_json() -> Value {
    let item = json!({
      "name": "Product name, for example bread",
      "amount": {"value": 10000, "offset": 100},
      "quantity": 1,
      "sale_amount": {"value": 100, "offset": 100},
      "country_of_origin": "country-of-origin",
      "importer_name": "name-of-importer-business",
      "importer_address": {
        "address_line1": "B8/733 nand nagri",
        "address_line2": "police station",
        "city": "East Delhi",
        "zone_code": "DL",
        "postal_code": "110093",
        "country_code": "IN"
      }
    });
    json!([item.clone(), item])
}

/// The examples' subtotal, tax, shipping and discount on `items`.
fn order(items: impl IntoIterator<Item = OrderItem>) -> Order {
    Order::new(
        items,
        Amount::inr(20000),
        DescribedAmount::new(Amount::inr(1000)).description("optional_text"),
    )
    .shipping(DescribedAmount::new(Amount::inr(1000)).description("optional_text"))
    .discount(
        Discount::new(Amount::inr(1000))
            .description("optional_text")
            .program_name("optional_text"),
    )
}

fn order_lines_json() -> Value {
    json!({
      "subtotal": {"value": 20000, "offset": 100},
      "tax": {"value": 1000, "offset": 100, "description": "optional_text"},
      "shipping": {"value": 1000, "offset": 100, "description": "optional_text"},
      "discount": {
        "value": 1000,
        "offset": 100,
        "description": "optional_text",
        "discount_program_name": "optional_text"
      }
    })
}

fn expiration() -> OrderExpiration {
    OrderExpiration::new(EXPIRES, "cancellation-explanation")
}

fn expiration_json() -> Value {
    json!({"timestamp": EXPIRES.to_string(), "description": "cancellation-explanation"})
}

/// Merge `extra` into the object `base`.
fn with(mut base: Value, extra: Value) -> Value {
    let (Value::Object(b), Value::Object(e)) = (&mut base, extra) else {
        panic!("objects only");
    };
    b.extend(e);
    base
}

fn details(setting: impl Into<PaymentSetting>, order: Order) -> OrderDetails {
    OrderDetails::new(
        "your-text-body-content",
        OrderDetailsParameters::inr("reference-id-value", GoodsType::Digital, setting, order),
    )
    .footer("your-text-footer-content")
}

// ─── order_details: the pages' examples ──────────────────────────────────

/// `payments-in/upi-intent/dynamic-vpa`, step 2: the UPI intent link
/// integration without a catalog.
#[test]
fn upi_intent_link_example() {
    let msg = details(
        PaymentSetting::upi_intent_link(
            "upi://pay?pa=merchant_vpa&pn=merchant%20Name&mc=mc_code&purpose=purpose_code&tr=transaction_record",
        ),
        order(non_catalog_items()).expiration(expiration()),
    )
    .header_image(MediaSource::link("your-media-url-link"));
    assert_eq!(msg.parameters.total_amount, Amount::inr(21000));
    assert_eq!(
        sent(msg),
        envelope(&json!({
          "type": "order_details",
          "header": {"type": "image", "image": {"link": "your-media-url-link"}},
          "body": {"text": "your-text-body-content"},
          "footer": {"text": "your-text-footer-content"},
          "action": {
            "name": "review_and_pay",
            "parameters": {
              "reference_id": "reference-id-value",
              "type": "digital-goods",
              "payment_settings": [{
                "type": "upi_intent_link",
                "upi_intent_link": {
                  "link": "upi://pay?pa=merchant_vpa&pn=merchant%20Name&mc=mc_code&purpose=purpose_code&tr=transaction_record"
                }
              }],
              "currency": "INR",
              "total_amount": {"value": 21000, "offset": 100},
              "order": with(json!({
                "status": "pending",
                "expiration": expiration_json(),
                "items": non_catalog_items_json()
              }), order_lines_json())
            }
          }
        }))
    );
}

/// `payments-in/payment-links`, step 2: the integration without a
/// catalog (header by media id, `payment_type` `upi`).
#[test]
fn payment_link_example() {
    let mut params = OrderDetailsParameters::inr(
        "reference-id-value",
        GoodsType::Digital,
        PaymentSetting::payment_link("https://the-payment-link"),
        order(non_catalog_items()).expiration(expiration()),
    )
    .payment_type(PaymentType::Upi);
    params.total_amount = Amount::inr(21000);
    let msg = OrderDetails::new("your-text-body-content", params)
        .footer("your-text-footer-content")
        .header_image(MediaId::new(MEDIA).unwrap());
    assert_eq!(
        sent(msg),
        envelope(&json!({
          "type": "order_details",
          "header": {"type": "image", "image": {"id": MEDIA}},
          "body": {"text": "your-text-body-content"},
          "footer": {"text": "your-text-footer-content"},
          "action": {
            "name": "review_and_pay",
            "parameters": {
              "reference_id": "reference-id-value",
              "type": "digital-goods",
              "payment_type": "upi",
              "payment_settings": [{
                "type": "payment_link",
                "payment_link": {"uri": "https://the-payment-link"}
              }],
              "currency": "INR",
              "total_amount": {"value": 21000, "offset": 100},
              "order": with(json!({
                "status": "pending",
                "expiration": expiration_json(),
                "items": non_catalog_items_json()
              }), order_lines_json())
            }
          }
        }))
    );
}

/// The catalog item of the catalog-based examples.
fn catalog_order() -> Order {
    order([
        OrderItem::new("Product name, for example bread", Amount::inr(10000), 1)
            .retailer_id("1234567")
            .sale_amount(Amount::inr(100)),
    ])
    .catalog_id(CatalogId::new(CATALOG))
    .expiration(expiration())
}

fn catalog_order_json() -> Value {
    with(
        json!({
          "status": "pending",
          "catalog_id": CATALOG,
          "expiration": expiration_json(),
          "items": [{
            "retailer_id": "1234567",
            "name": "Product name, for example bread",
            "amount": {"value": 10000, "offset": 100},
            "quantity": 1,
            "sale_amount": {"value": 100, "offset": 100}
          }]
        }),
        order_lines_json(),
    )
}

fn gateway_example(provider: GatewayProvider, gateway_json: &Value) -> (OrderDetails, Value) {
    let msg = details(
        PaymentGateway::new(provider, "payment-config-id"),
        catalog_order(),
    )
    .header_image(MediaSource::link("http(s)://the-url"));
    let expected = envelope(&json!({
      "type": "order_details",
      // The example's `image.provider` is in no table and not modelled.
      "header": {"type": "image", "image": {"link": "http(s)://the-url"}},
      "body": {"text": "your-text-body-content"},
      "footer": {"text": "your-text-footer-content"},
      "action": {
        "name": "review_and_pay",
        "parameters": {
          "reference_id": "reference-id-value",
          "type": "digital-goods",
          "payment_settings": [{"type": "payment_gateway", "payment_gateway": gateway_json}],
          "currency": "INR",
          "total_amount": {"value": 21000, "offset": 100},
          "order": catalog_order_json()
        }
      }
    }));
    (msg, expected)
}

/// `payments-in/pg`, step 1: the `BillDesk` catalog-based integration.
#[test]
fn billdesk_example() {
    let fields = BilldeskFields {
        additional_info1: Some("additional_info1-value".into()),
        additional_info2: Some("additional_info2-value".into()),
        additional_info3: Some("additional_info3-value".into()),
        additional_info4: Some("additional_info4-value".into()),
        additional_info5: Some("additional_info5-value".into()),
        additional_info6: Some("additional_info6-value".into()),
        additional_info7: Some("additional_info7-value".into()),
    };
    let (msg, expected) = gateway_example(
        GatewayProvider::Billdesk(fields),
        &json!({
          "type": "billdesk",
          "configuration_name": "payment-config-id",
          "billdesk": {
            "additional_info1": "additional_info1-value",
            "additional_info2": "additional_info2-value",
            "additional_info3": "additional_info3-value",
            "additional_info4": "additional_info4-value",
            "additional_info5": "additional_info5-value",
            "additional_info6": "additional_info6-value",
            "additional_info7": "additional_info7-value"
          }
        }),
    );
    assert_eq!(sent(msg), expected);
}

/// `payments-in/pg`, step 1: the Razorpay catalog-based integration.
#[test]
fn razorpay_example() {
    let fields = RazorpayFields {
        receipt: Some("receipt-value".into()),
        notes: [("key1".to_owned(), "value1".to_owned())].into(),
    };
    let (msg, expected) = gateway_example(
        GatewayProvider::Razorpay(fields),
        &json!({
          "type": "razorpay",
          "configuration_name": "payment-config-id",
          "razorpay": {"receipt": "receipt-value", "notes": {"key1": "value1"}}
        }),
    );
    assert_eq!(sent(msg), expected);
}

/// `payments-in/pg`, step 1: the `PayU` and Zaakpay integrations without a
/// catalog.
#[test]
fn payu_and_zaakpay_examples() {
    let cases = [
        (
            GatewayProvider::Payu(PayuFields {
                udf1: Some("value1".into()),
                udf2: Some("value2".into()),
                udf3: Some("value3".into()),
                udf4: Some("value4".into()),
            }),
            json!({
              "type": "payu",
              "configuration_name": "payment-config-id",
              "payu": {"udf1": "value1", "udf2": "value2", "udf3": "value3", "udf4": "value4"}
            }),
        ),
        (
            GatewayProvider::Zaakpay(ZaakpayFields {
                extra1: Some("value1".into()),
                extra2: Some("value2".into()),
            }),
            json!({
              "type": "zaakpay",
              "configuration_name": "payment-config-id",
              "zaakpay": {"extra1": "value1", "extra2": "value2"}
            }),
        ),
    ];
    for (provider, gateway_json) in cases {
        let msg = details(
            PaymentGateway::new(provider, "payment-config-id"),
            order(non_catalog_items()).expiration(expiration()),
        )
        .header_image(MediaSource::link("your-media-url-link"));
        assert_eq!(
            sent(msg),
            envelope(&json!({
              "type": "order_details",
              "header": {"type": "image", "image": {"link": "your-media-url-link"}},
              "body": {"text": "your-text-body-content"},
              "footer": {"text": "your-text-footer-content"},
              "action": {
                "name": "review_and_pay",
                "parameters": {
                  "reference_id": "reference-id-value",
                  "type": "digital-goods",
                  "payment_settings": [{"type": "payment_gateway", "payment_gateway": gateway_json}],
                  "currency": "INR",
                  "total_amount": {"value": 21000, "offset": 100},
                  "order": with(json!({
                    "status": "pending",
                    "expiration": expiration_json(),
                    "items": non_catalog_items_json()
                  }), order_lines_json())
                }
              }
            }))
        );
    }
}

/// `payments-in/pg`, "Merchant preferred UPI payment method" and
/// "Restrict available payment options": the two keys those sections add
/// to `payment_gateway` (their examples show nothing else of it).
#[test]
fn preferred_app_and_payment_options() {
    let gateway = PaymentGateway::new(
        GatewayProvider::Razorpay(RazorpayFields::default()),
        "payment-config-id",
    )
    .preferred_upi_app(UpiApp::GooglePay)
    .enabled_payment_options([PaymentOption::Web]);
    let wire = serde_json::to_value(PaymentSetting::from(gateway)).unwrap();
    assert_eq!(
        wire,
        json!({
          "type": "payment_gateway",
          "payment_gateway": {
            "type": "razorpay",
            "configuration_name": "payment-config-id",
            "preferred_payment_methods": [{"method": "gpay"}],
            "enabled_payment_options": ["web"]
          }
        })
    );
}

/// `payments-in/upi-intent` (the older payment configuration method):
/// `payment_type` and `payment_configuration` instead of
/// `payment_settings`, without a catalog.
#[test]
fn payment_configuration_example() {
    let mut params = OrderDetailsParameters::inr(
        "reference-id-value",
        GoodsType::Digital,
        PaymentSetting::upi_intent_link("unused"),
        order(non_catalog_items()).expiration(expiration()),
    )
    .payment_type(PaymentType::Upi);
    params.payment_settings.clear();
    params.payment_configuration = Some("unique-payment-config-id".into());
    let msg = OrderDetails::new("your-text-body-content", params)
        .footer("your-text-footer-content")
        .header_image(MediaId::new(MEDIA).unwrap());
    assert_eq!(
        sent(msg),
        envelope(&json!({
          "type": "order_details",
          "header": {"type": "image", "image": {"id": MEDIA}},
          "body": {"text": "your-text-body-content"},
          "footer": {"text": "your-text-footer-content"},
          "action": {
            "name": "review_and_pay",
            "parameters": {
              "reference_id": "reference-id-value",
              "type": "digital-goods",
              "payment_type": "upi",
              "payment_configuration": "unique-payment-config-id",
              "currency": "INR",
              "total_amount": {"value": 21000, "offset": 100},
              "order": with(json!({
                "status": "pending",
                "expiration": expiration_json(),
                "items": non_catalog_items_json()
              }), order_lines_json())
            }
          }
        }))
    );
}

/// The keys no example shows with a value, from the tables: beneficiaries,
/// `quick_pay`, an item image, the payment link's URLs.
#[test]
fn table_only_keys() {
    let item = OrderItem::new("Bread", Amount::inr(5000), 2)
        .image_link("https://example.com/bread.png")
        .import(
            "India",
            "Bread Co",
            ImporterAddress::new("1 Road", "Pune", "411001", "IN"),
        );
    let order = Order::new(
        [item],
        Amount::inr(10000),
        DescribedAmount::new(Amount::inr(0)),
    )
    .quick_pay();
    let link = PaymentLink::new("https://rzp.io/i/rNiAagU8y")
        .success_url("https://shop.example/ok")
        .cancel_url("https://shop.example/ko");
    let params = OrderDetailsParameters::inr("BM345A-12", GoodsType::Physical, link, order)
        .payment_type(PaymentType::Upi)
        .beneficiary(
            Beneficiary::new("Asha", "12 MG Road", "Bengaluru", "Karnataka", "560001")
                .address_line2("Near the park"),
        );
    let body = sent(OrderDetails::new("Pay for your bread", params));
    let parameters = &body["interactive"]["action"]["parameters"];
    assert_eq!(
        parameters["beneficiaries"],
        json!([{
          "name": "Asha",
          "address_line1": "12 MG Road",
          "address_line2": "Near the park",
          "city": "Bengaluru",
          "state": "Karnataka",
          "country": "India",
          "postal_code": "560001"
        }])
    );
    assert_eq!(parameters["order"]["type"], json!("quick_pay"));
    assert_eq!(
        parameters["order"]["items"][0]["image"],
        json!({"link": "https://example.com/bread.png"})
    );
    assert_eq!(
        parameters["payment_settings"][0]["payment_link"],
        json!({
          "uri": "https://rzp.io/i/rNiAagU8y",
          "success_url": "https://shop.example/ok",
          "cancel_url": "https://shop.example/ko"
        })
    );
    assert_eq!(
        parameters["total_amount"],
        json!({"value": 10000, "offset": 100})
    );
    assert!(body["interactive"].get("header").is_none());
    assert!(body["interactive"].get("footer").is_none());
}

/// A refusal from the pages: the WhatsApp Payments terms of service not
/// accepted (`payments-in/upi-intent/dynamic-vpa`, "Errors").
#[tokio::test]
async fn terms_not_accepted_is_a_payment_error() {
    let t = ScriptedTransport::new();
    t.push_json(
        400,
        json!({
          "error": {
            "message": "(#134011) WhatsApp Payments terms of service has not been accepted",
            "type": "OAuthException",
            "code": 134011,
            "error_data": {
              "messaging_product": "whatsapp",
              "details": "WhatsApp Payments Terms of Service acceptance pending for this WhatsApp Business Account. Please use the following link to accept terms of service before using Business APIs: https://fb.me/12345"
            }
          }
        }),
    );
    let msg = OutboundMessage::new(
        Recipient::phone(TO),
        details(
            PaymentSetting::upi_intent_link("upi://pay?pa=a@b"),
            catalog_order(),
        ),
    );
    let err = client(&t)
        .messages(PhoneNumberId::new(PHONE_ID).unwrap())
        .send(&msg)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Payment);
    assert_eq!(err.graph().map(|g| g.code), Some(134011));
    assert_eq!(t.remaining(), 0);
}

/// An order is a send: never replayed after a timeout.
#[tokio::test]
async fn order_details_is_never_replayed() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let msg = OutboundMessage::new(
        Recipient::phone(TO),
        details(
            PaymentSetting::upi_intent_link("upi://pay?pa=a@b"),
            catalog_order(),
        ),
    );
    let err = retrying_client(&t)
        .messages(PhoneNumberId::new(PHONE_ID).unwrap())
        .send(&msg)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(t.requests().len(), 1);
    assert_eq!(t.remaining(), 0);
}

// ─── order_details: local checks ─────────────────────────────────────────

fn valid() -> OrderDetails {
    details(
        PaymentSetting::upi_intent_link("upi://pay?pa=a@b"),
        order(non_catalog_items()),
    )
}

#[track_caller]
fn refused(msg: &OrderDetails) -> String {
    let field = msg
        .validate()
        .expect_err("expected a validation error")
        .field;
    // `send` refuses it too, before any request.
    let t = ScriptedTransport::new();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(
            client(&t)
                .messages(PhoneNumberId::new(PHONE_ID).unwrap())
                .send(&OutboundMessage::new(Recipient::phone(TO), msg.clone())),
        )
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)));
    assert!(t.requests().is_empty());
    field
}

const P: &str = "interactive.action.parameters";

type Edit = fn(&mut OrderDetails);

/// Each edit of a valid message is refused on `field`, before sending.
#[track_caller]
fn check_rules(cases: &[(Edit, &str)]) {
    assert!(valid().validate().is_ok());
    for (edit, field) in cases {
        let mut msg = valid();
        edit(&mut msg);
        let got = refused(&msg);
        assert!(
            got == *field || got == format!("{P}.{field}"),
            "expected `{field}`, got `{got}`"
        );
    }
}

#[test]
fn message_and_payment_setting_rules_are_checked() {
    check_rules(&[
        (|m| m.body = String::new(), "interactive.body.text"),
        (|m| m.body = "x".repeat(1025), "interactive.body.text"),
        (
            |m| m.footer = Some("x".repeat(61)),
            "interactive.footer.text",
        ),
        (
            |m| m.header = Some(MediaSource::link("")),
            "interactive.header.image.link",
        ),
        (
            |m| m.parameters.reference_id = String::new(),
            "reference_id",
        ),
        (
            |m| m.parameters.reference_id = "x".repeat(36),
            "reference_id",
        ),
        (
            |m| m.parameters.reference_id = "BM 345".into(),
            "reference_id",
        ),
        (
            |m| m.parameters.payment_settings.clear(),
            "payment_settings",
        ),
        (
            |m| m.parameters.payment_settings = vec![PaymentSetting::upi_intent_link("")],
            "payment_settings[0].upi_intent_link.link",
        ),
        (
            |m| m.parameters.payment_settings = vec![PaymentSetting::payment_link("https://x")],
            "payment_type",
        ),
        (
            |m| {
                m.parameters.payment_settings =
                    vec![PaymentLink::new("https://x").success_url("").into()];
                m.parameters.payment_type = Some(PaymentType::Upi);
            },
            "payment_settings[0].payment_link.success_url",
        ),
        (
            |m| {
                m.parameters.payment_settings = vec![
                    PaymentGateway::new(
                        GatewayProvider::Payu(PayuFields::default()),
                        "x".repeat(61),
                    )
                    .into(),
                ];
            },
            "payment_settings[0].payment_gateway.configuration_name",
        ),
    ]);
}

#[test]
fn gateway_field_rules_are_checked() {
    check_rules(&[
        (
            |m| {
                let fields = BilldeskFields {
                    additional_info7: Some("x".repeat(121)),
                    ..BilldeskFields::default()
                };
                m.parameters.payment_settings =
                    vec![PaymentGateway::new(GatewayProvider::Billdesk(fields), "pg").into()];
            },
            "payment_settings[0].payment_gateway.billdesk.additional_info7",
        ),
        (
            |m| {
                let fields = RazorpayFields {
                    receipt: Some("x".repeat(41)),
                    ..RazorpayFields::default()
                };
                m.parameters.payment_settings =
                    vec![PaymentGateway::new(GatewayProvider::Razorpay(fields), "pg").into()];
            },
            "payment_settings[0].payment_gateway.razorpay.receipt",
        ),
        (
            |m| {
                let fields = RazorpayFields {
                    notes: (0..16).map(|i| (format!("k{i}"), "v".to_owned())).collect(),
                    ..RazorpayFields::default()
                };
                m.parameters.payment_settings =
                    vec![PaymentGateway::new(GatewayProvider::Razorpay(fields), "pg").into()];
            },
            "payment_settings[0].payment_gateway.razorpay.notes",
        ),
        (
            |m| {
                let fields = RazorpayFields {
                    notes: [("k".to_owned(), "v".repeat(257))].into(),
                    ..RazorpayFields::default()
                };
                m.parameters.payment_settings =
                    vec![PaymentGateway::new(GatewayProvider::Razorpay(fields), "pg").into()];
            },
            "payment_settings[0].payment_gateway.razorpay.notes.k",
        ),
        (
            |m| {
                let fields = PayuFields {
                    udf2: Some("x".repeat(256)),
                    ..PayuFields::default()
                };
                m.parameters.payment_settings =
                    vec![PaymentGateway::new(GatewayProvider::Payu(fields), "pg").into()];
            },
            "payment_settings[0].payment_gateway.payu.udf2",
        ),
        (
            |m| {
                let fields = ZaakpayFields {
                    extra1: Some("x".repeat(181)),
                    ..ZaakpayFields::default()
                };
                m.parameters.payment_settings =
                    vec![PaymentGateway::new(GatewayProvider::Zaakpay(fields), "pg").into()];
            },
            "payment_settings[0].payment_gateway.zaakpay.extra1",
        ),
        (
            |m| m.parameters.payment_configuration = Some("x".repeat(61)),
            "payment_configuration",
        ),
        (
            |m| {
                m.parameters.payment_settings.clear();
                m.parameters.payment_configuration = Some("pc".into());
            },
            "payment_type",
        ),
    ]);
}

#[test]
fn beneficiary_and_total_rules_are_checked() {
    check_rules(&[
        (
            |m| {
                m.parameters.beneficiaries =
                    vec![Beneficiary::new("x".repeat(201), "l", "c", "s", "560001")];
            },
            "beneficiaries[0].name",
        ),
        (
            |m| {
                m.parameters.beneficiaries =
                    vec![Beneficiary::new("n", "x".repeat(101), "c", "s", "560001")];
            },
            "beneficiaries[0].address_line1",
        ),
        (
            |m| {
                m.parameters.beneficiaries = vec![
                    Beneficiary::new("n", "l", "c", "s", "560001").address_line2("x".repeat(101)),
                ];
            },
            "beneficiaries[0].address_line2",
        ),
        (
            |m| m.parameters.beneficiaries = vec![Beneficiary::new("n", "l", "", "s", "560001")],
            "beneficiaries[0].city",
        ),
        (
            |m| m.parameters.beneficiaries = vec![Beneficiary::new("n", "l", "c", "", "560001")],
            "beneficiaries[0].state",
        ),
        (
            |m| {
                let mut b = Beneficiary::new("n", "l", "c", "s", "560001");
                b.country = "IN".into();
                m.parameters.beneficiaries = vec![b];
            },
            "beneficiaries[0].country",
        ),
        (
            |m| m.parameters.beneficiaries = vec![Beneficiary::new("n", "l", "c", "s", "56001")],
            "beneficiaries[0].postal_code",
        ),
        (
            |m| m.parameters.total_amount = Amount::new(21000, 1000),
            "total_amount.offset",
        ),
        (
            |m| m.parameters.total_amount = Amount::inr(21001),
            "total_amount.value",
        ),
        (
            |m| {
                let o = m.parameters.order.as_mut().unwrap();
                o.subtotal = Amount::inr(0);
                o.tax = DescribedAmount::new(Amount::inr(0));
                o.shipping = None;
                o.discount = None;
                m.parameters.total_amount = Amount::inr(0);
            },
            "total_amount.value",
        ),
        (
            |m| {
                m.parameters.order.as_mut().unwrap().discount =
                    Some(Discount::new(Amount::inr(50000)));
            },
            "total_amount.value",
        ),
    ]);
}

#[test]
fn order_rules_are_checked() {
    check_rules(&[
        (|m| m.parameters.order = None, "order"),
        (
            |m| m.parameters.order.as_mut().unwrap().items.clear(),
            "order.items",
        ),
        (
            |m| m.parameters.order.as_mut().unwrap().items[1].name = "x".repeat(61),
            "order.items[1].name",
        ),
        (
            |m| m.parameters.order.as_mut().unwrap().items[0].amount = Amount::new(10000, 1),
            "order.items[0].amount.offset",
        ),
        (
            |m| {
                m.parameters.order.as_mut().unwrap().items[0].sale_amount =
                    Some(Amount::new(100, 1));
            },
            "order.items[0].sale_amount.offset",
        ),
        (
            |m| m.parameters.order.as_mut().unwrap().items[0].country_of_origin = None,
            "order.items[0].country_of_origin",
        ),
        (
            |m| m.parameters.order.as_mut().unwrap().items[0].importer_name = None,
            "order.items[0].importer_name",
        ),
        (
            |m| m.parameters.order.as_mut().unwrap().items[0].importer_address = None,
            "order.items[0].importer_address",
        ),
        (
            |m| {
                let item = &mut m.parameters.order.as_mut().unwrap().items[0];
                *item = item
                    .clone()
                    .retailer_id("SKU")
                    .image_link("https://x/i.png");
            },
            "order.items[0].image",
        ),
        (
            |m| {
                let o = m.parameters.order.as_mut().unwrap();
                o.items[0] = o.items[0].clone().image_link("https://x/i.png");
                o.catalog_id = Some(CatalogId::new(CATALOG));
            },
            "order.catalog_id",
        ),
        (
            |m| {
                let o = m.parameters.order.as_mut().unwrap();
                let item = o.items[0].clone().image_link("https://x/i.png");
                o.items = vec![item; 11];
            },
            "order.items",
        ),
        (
            |m| m.parameters.order.as_mut().unwrap().subtotal = Amount::new(20000, 10),
            "order.subtotal.offset",
        ),
        (
            |m| {
                m.parameters.order.as_mut().unwrap().tax =
                    DescribedAmount::new(Amount::inr(1000)).description("x".repeat(61));
            },
            "order.tax.description",
        ),
        (
            |m| {
                m.parameters.order.as_mut().unwrap().shipping =
                    Some(DescribedAmount::new(Amount::new(1000, 1)));
            },
            "order.shipping.offset",
        ),
        (
            |m| {
                m.parameters.order.as_mut().unwrap().discount =
                    Some(Discount::new(Amount::inr(1000)).program_name("x".repeat(61)));
            },
            "order.discount.discount_program_name",
        ),
        (
            |m| {
                m.parameters.order.as_mut().unwrap().expiration =
                    Some(OrderExpiration::new(EXPIRES, "x".repeat(121)));
            },
            "order.expiration.description",
        ),
    ]);
}

/// The catalog makes the items' origin and importer optional.
#[test]
fn a_catalog_order_needs_no_importer() {
    let msg = details(
        PaymentSetting::upi_intent_link("upi://pay?pa=a@b"),
        catalog_order(),
    );
    assert!(msg.validate().is_ok());
}

/// The total the constructor computes is the documented sum.
#[test]
fn the_total_is_subtotal_plus_tax_plus_shipping_minus_discount() {
    let order = Order::new(
        [OrderItem::new("A", Amount::inr(400), 1).retailer_id("A")],
        Amount::inr(400),
        DescribedAmount::new(Amount::inr(500)),
    )
    .shipping(DescribedAmount::new(Amount::inr(0)))
    .discount(Discount::new(Amount::inr(250)));
    // payments-in/orderdetailstemplate: 400 + 500 + 0 - 250 = 650.
    assert_eq!(order.total(), Some(650));
    let below_zero = Order::new([], Amount::inr(1), DescribedAmount::new(Amount::inr(0)))
        .discount(Discount::new(Amount::inr(2)));
    assert_eq!(below_zero.total(), None);
}

/// `n` characters of two bytes each: a limit counted in bytes would
/// refuse it at half its length.
fn chars(n: usize) -> String {
    "é".repeat(n)
}

/// Every documented limit is inclusive, and counted in characters, not
/// bytes: a value exactly at it passes (the tests above refuse one more).
#[test]
fn every_limit_is_inclusive_and_counts_characters() {
    let item = OrderItem::new(chars(60), Amount::inr(10000), 1).import(
        "India",
        "Bread Co",
        ImporterAddress::new("1 Road", "Pune", "411001", "IN"),
    );
    let order = Order::new(
        [item.clone(), item],
        Amount::inr(20000),
        DescribedAmount::new(Amount::inr(1000)).description(chars(60)),
    )
    .shipping(DescribedAmount::new(Amount::inr(1000)).description(chars(60)))
    .discount(
        Discount::new(Amount::inr(1000))
            .description(chars(60))
            .program_name(chars(60)),
    )
    .expiration(OrderExpiration::new(EXPIRES, chars(120)));
    let gateways = [
        GatewayProvider::Billdesk(BilldeskFields {
            additional_info1: Some(chars(120)),
            additional_info7: Some(chars(120)),
            ..BilldeskFields::default()
        }),
        GatewayProvider::Razorpay(RazorpayFields {
            receipt: Some(chars(40)),
            notes: (0..15).map(|i| (format!("k{i}"), chars(256))).collect(),
        }),
        GatewayProvider::Payu(PayuFields {
            udf1: Some(chars(255)),
            udf4: Some(chars(255)),
            ..PayuFields::default()
        }),
        GatewayProvider::Zaakpay(ZaakpayFields {
            extra1: Some(chars(180)),
            extra2: Some(chars(180)),
        }),
    ];
    let mut params = OrderDetailsParameters::inr(
        "x".repeat(35),
        GoodsType::Physical,
        PaymentSetting::upi_intent_link("upi://pay?pa=a@b"),
        order,
    )
    .beneficiary(
        Beneficiary::new(chars(200), chars(100), "Pune", "Maharashtra", "411001")
            .address_line2(chars(100)),
    );
    for provider in gateways {
        params = params.payment_setting(PaymentGateway::new(provider, chars(60)));
    }
    let msg = OrderDetails::new(chars(1024), params).footer(chars(60));
    assert_eq!(msg.validate(), Ok(()));

    // Ten items with custom images, the most an order with one takes.
    let pictured = OrderItem::new("Bread", Amount::inr(100), 1)
        .image_link("https://x/i.png")
        .import(
            "India",
            "Bread Co",
            ImporterAddress::new("1 Road", "Pune", "411001", "IN"),
        );
    let order = Order::new(
        vec![pictured; 10],
        Amount::inr(1000),
        DescribedAmount::new(Amount::inr(0)),
    );
    let msg = details(PaymentSetting::upi_intent_link("upi://pay?pa=a@b"), order);
    assert_eq!(msg.validate(), Ok(()));

    // The older payment configuration's name, at 60.
    let mut msg = valid();
    msg.parameters.payment_settings.clear();
    msg.parameters.payment_configuration = Some(chars(60));
    msg.parameters.payment_type = Some(PaymentType::Upi);
    assert_eq!(msg.validate(), Ok(()));
    // The configuration API takes the same names.
    assert_eq!(
        CreatePaymentConfiguration::new(chars(60), PaymentProvider::Razorpay).validate(),
        Ok(())
    );
}

/// Amounts near `u64::MAX` neither overflow nor wrap: the total is
/// computed in `u128`, and one beyond `u64::MAX` is refused.
#[test]
fn totals_beyond_u64_are_refused_without_overflowing() {
    let max = Amount::inr(u64::MAX);
    let item = OrderItem::new("Gold", max, u32::MAX).retailer_id("G");
    let at_max = Order::new([item.clone()], max, DescribedAmount::new(Amount::inr(0)))
        .catalog_id(CatalogId::new(CATALOG));
    assert_eq!(at_max.total(), Some(u64::MAX));
    let msg = details(PaymentSetting::upi_intent_link("upi://pay?pa=a@b"), at_max);
    assert_eq!(msg.parameters.total_amount, max);
    assert_eq!(msg.validate(), Ok(()));

    let beyond = Order::new([item], max, DescribedAmount::new(max))
        .shipping(DescribedAmount::new(max))
        .discount(Discount::new(Amount::inr(1)))
        .catalog_id(CatalogId::new(CATALOG));
    assert_eq!(beyond.total(), None);
    let mut msg = details(PaymentSetting::upi_intent_link("upi://pay?pa=a@b"), beyond);
    // The constructor cannot compute it: it sends no total, refused.
    assert_eq!(msg.parameters.total_amount, Amount::inr(0));
    assert_eq!(refused(&msg), format!("{P}.total_amount.value"));
    msg.parameters.total_amount = max;
    let err = msg.validate().unwrap_err();
    assert_eq!(err.field, format!("{P}.total_amount.value"));
    assert!(err.to_string().contains("above u64::MAX"), "{err}");
}

/// `payments-in/upi-intent` (the older `payment_configuration` alone)
/// makes the items' origin and importer optional without a catalog; the
/// `payment_settings` pages require them.
#[test]
fn the_older_configuration_needs_no_importer() {
    let bare = || {
        Order::new(
            [OrderItem::new("Bread", Amount::inr(100), 1)],
            Amount::inr(100),
            DescribedAmount::new(Amount::inr(0)),
        )
    };
    let mut msg = details(PaymentSetting::upi_intent_link("upi://pay?pa=a@b"), bare());
    assert_eq!(
        refused(&msg),
        format!("{P}.order.items[0].country_of_origin")
    );
    msg.parameters.payment_settings.clear();
    msg.parameters.payment_configuration = Some("pc".into());
    msg.parameters.payment_type = Some(PaymentType::Upi);
    assert_eq!(msg.validate(), Ok(()));
}

// ─── order_status ────────────────────────────────────────────────────────

/// `payments-in/pg`, step 6 (and the same example on the UPI intent and
/// payment links pages): an `order_status` message. Its `status` lists
/// the values; `processing` is sent.
#[test]
fn order_status_example() {
    let msg = OrderStatus::new(
        "your-text-body-content",
        "reference-id-value",
        OrderUpdate::new(OrderStatusValue::Processing).description("optional-text"),
    );
    assert_eq!(
        sent(msg),
        envelope(&json!({
          "type": "order_status",
          "body": {"text": "your-text-body-content"},
          "action": {
            "name": "review_order",
            "parameters": {
              "reference_id": "reference-id-value",
              "order": {"status": "processing", "description": "optional-text"}
            }
          }
        }))
    );
}

/// Every status of the `order_status` parameters table, as written there.
#[test]
fn order_status_values_are_the_tables() {
    let values = [
        (OrderStatusValue::Pending, "pending"),
        (OrderStatusValue::Processing, "processing"),
        (OrderStatusValue::PartiallyShipped, "partially_shipped"),
        (OrderStatusValue::Shipped, "shipped"),
        (OrderStatusValue::Completed, "completed"),
        (OrderStatusValue::Canceled, "canceled"),
    ];
    for (value, wire) in values {
        assert_eq!(serde_json::to_value(&value).unwrap(), json!(wire));
    }
    assert_eq!(
        serde_json::to_value(OrderStatusValue::Other("returned".into())).unwrap(),
        json!("returned")
    );
}

#[test]
fn order_status_rules_are_checked() {
    type StatusEdit = fn(&mut OrderStatus);
    let ok =
        || OrderStatus::new("b", "ref", OrderUpdate::new(OrderStatusValue::Canceled)).footer("f");
    assert!(ok().validate().is_ok());
    let cases: &[(StatusEdit, &str)] = &[
        (|m| m.body = "x".repeat(1025), "interactive.body.text"),
        (
            |m| m.footer = Some(String::new()),
            "interactive.footer.text",
        ),
        (
            |m| m.reference_id = String::new(),
            "interactive.action.parameters.reference_id",
        ),
        (
            |m| m.reference_id = "x".repeat(36),
            "interactive.action.parameters.reference_id",
        ),
        (
            |m| m.reference_id = "BM 345".into(),
            "interactive.action.parameters.reference_id",
        ),
        (|m| m.order = None, "interactive.action.parameters.order"),
        (
            |m| m.order = Some(OrderUpdate::new(OrderStatusValue::Pending)),
            "interactive.action.parameters.order.status",
        ),
        (
            |m| m.order = Some(OrderUpdate::new(OrderStatusValue::Other(String::new()))),
            "interactive.action.parameters.order.status",
        ),
        (
            |m| {
                m.order =
                    Some(OrderUpdate::new(OrderStatusValue::Canceled).description("x".repeat(121)));
            },
            "interactive.action.parameters.order.description",
        ),
    ];
    for (edit, field) in cases {
        let mut msg = ok();
        edit(&mut msg);
        let message = OutboundMessage::new(Recipient::phone(TO), msg);
        assert_eq!(message.validate().unwrap_err().field, *field);
    }
    // At the limits, in characters; every status the table allows, and
    // one this version does not know.
    for status in [
        OrderStatusValue::Processing,
        OrderStatusValue::PartiallyShipped,
        OrderStatusValue::Shipped,
        OrderStatusValue::Completed,
        OrderStatusValue::Canceled,
        OrderStatusValue::Other("returned".into()),
    ] {
        let msg = OrderStatus::new(
            chars(1024),
            "B.m_3-4".repeat(5),
            OrderUpdate::new(status).description(chars(120)),
        )
        .footer(chars(60));
        assert_eq!(msg.validate(), Ok(()));
    }
}

// ─── Lookup ──────────────────────────────────────────────────────────────

async fn lookup(answer: Value) -> (Vec<Payment>, ScriptedTransport) {
    let t = ScriptedTransport::new();
    t.push_json(200, answer);
    let payments = client(&t)
        .payments(PhoneNumberId::new(PHONE_ID).unwrap())
        .lookup("payment-config-id", "reference-id-value")
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(
        req.path(),
        format!("/v25.0/{PHONE_ID}/payments/payment-config-id/reference-id-value")
    );
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(t.remaining(), 0);
    (payments, t)
}

/// `payments-in/pg`, step 5, "Response by payment stage": no payment
/// attempted.
#[tokio::test]
async fn lookup_before_any_attempt() {
    let (payments, _) = lookup(json!({
      "payments": [{
        "reference_id": "<your_reference_id>",
        "status": "pending",
        "amount": {"offset": 100, "value": 1000},
        "currency": "INR"
      }]
    }))
    .await;
    assert_eq!(payments.len(), 1);
    let p = &payments[0];
    assert_eq!(p.reference_id, "<your_reference_id>");
    assert_eq!(p.status, PaymentStatus::Pending);
    assert_eq!(p.amount, Amount::inr(1000));
    assert_eq!(p.currency, "INR");
    assert!(p.transactions.is_empty());
    assert_eq!(p.razorpay, RazorpayFields::default());
}

/// "Payment successful" and "Payment failed".
#[tokio::test]
async fn lookup_after_a_success_and_after_a_failure() {
    let (payments, _) = lookup(json!({
      "payments": [{
        "reference_id": "<your_reference_id>",
        "status": "captured",
        "amount": {"offset": 100, "value": 1000},
        "currency": "INR",
        "transactions": [{
          "id": "<order_id>",
          "pg_transaction_id": "<payment_id>",
          "type": "razorpay",
          "status": "success",
          "created_timestamp": 1772129215,
          "updated_timestamp": 1772129215,
          "amount": {"offset": 100, "value": 1000},
          "order_amount": {"offset": 100, "value": 1000},
          "currency": "INR",
          "method": {"type": "upi"}
        }]
      }]
    }))
    .await;
    let p = &payments[0];
    assert_eq!(p.status, PaymentStatus::Captured);
    let tx = &p.transactions[0];
    assert_eq!(tx.id, "<order_id>");
    assert_eq!(tx.pg_transaction_id.as_deref(), Some("<payment_id>"));
    assert_eq!(tx.gateway, GatewayType::Razorpay);
    assert_eq!(tx.status, TransactionStatus::Success);
    assert_eq!(tx.created_timestamp.unix_timestamp(), 1_772_129_215);
    assert_eq!(tx.updated_timestamp.unix_timestamp(), 1_772_129_215);
    assert_eq!(tx.amount, Some(Amount::inr(1000)));
    assert_eq!(tx.order_amount, Some(Amount::inr(1000)));
    assert_eq!(tx.currency.as_deref(), Some("INR"));
    assert_eq!(
        tx.method.as_ref().map(|m| &m.kind),
        Some(&PaymentMethodType::Upi)
    );
    assert!(tx.error.is_none());

    let (payments, _) = lookup(json!({
      "payments": [{
        "reference_id": "<your_reference_id>",
        "status": "pending",
        "amount": {"offset": 100, "value": 1000},
        "currency": "INR",
        "transactions": [{
          "id": "<order_id>",
          "pg_transaction_id": "<payment_id>",
          "type": "razorpay",
          "status": "failed",
          "created_timestamp": 1772129329,
          "updated_timestamp": 1772129329,
          "amount": {"offset": 100, "value": 1000},
          "order_amount": {"offset": 100, "value": 1000},
          "currency": "INR",
          "method": {"type": "upi"},
          "error": {"code": "BAD_REQUEST_ERROR", "reason": "incorrect_pin"}
        }]
      }]
    }))
    .await;
    let tx = &payments[0].transactions[0];
    assert_eq!(payments[0].status, PaymentStatus::Pending);
    assert_eq!(tx.status, TransactionStatus::Failed);
    let error = tx.error.as_ref().unwrap();
    assert_eq!(error.code, "BAD_REQUEST_ERROR");
    assert_eq!(error.reason, "incorrect_pin");
}

/// The full example of step 5, with the fields passed on to the gateway
/// and a refund (placeholders filled).
#[tokio::test]
async fn lookup_returns_the_gateway_fields_and_refunds() {
    let (payments, _) = lookup(json!({
      "payments": [{
        "reference_id": "reference-id-value",
        "status": "captured",
        "currency": "INR",
        "amount": {"value": 21000, "offset": 100},
        "transactions": [{
          "id": "[PG-ORDER-ID]",
          "pg_transaction_id": "[PG-TXN-ID]",
          "type": "billdesk",
          "status": "success",
          "created_timestamp": "1772129215",
          "updated_timestamp": "1772129300",
          "method": {"type": "netbanking"},
          "error": {"code": "pg-generated-error-code", "reason": "pg-generated-descriptive-reason"},
          "refunds": [{
            "id": "[REFUND-ID]",
            "amount": {"value": 100, "offset": 100},
            "speed_processed": "instant",
            "status": "success",
            "created_timestamp": "1772130000",
            "updated_timestamp": "1772130060"
          }]
        }],
        "receipt": "receipt-value",
        "notes": {"key1": "value1", "key2": "value2"},
        "udf1": "udf1-value",
        "udf2": "udf2-value",
        "udf3": "udf3-value",
        "udf4": "udf4-value",
        "additional_info1": "additional_info1-value",
        "additional_info2": "additional_info2-value",
        "additional_info3": "additional_info3-value",
        "additional_info4": "additional_info4-value",
        "additional_info5": "additional_info5-value",
        "additional_info6": "additional_info6-value",
        "additional_info7": "additional_info7-value"
      }]
    }))
    .await;
    let p = &payments[0];
    assert_eq!(p.razorpay.receipt.as_deref(), Some("receipt-value"));
    assert_eq!(
        p.razorpay.notes.get("key2").map(String::as_str),
        Some("value2")
    );
    assert_eq!(p.payu.udf4.as_deref(), Some("udf4-value"));
    assert_eq!(
        p.billdesk.additional_info7.as_deref(),
        Some("additional_info7-value")
    );
    assert_eq!(p.zaakpay, ZaakpayFields::default());
    let tx = &p.transactions[0];
    assert_eq!(tx.gateway, GatewayType::Billdesk);
    assert_eq!(
        tx.method.as_ref().map(|m| &m.kind),
        Some(&PaymentMethodType::Netbanking)
    );
    let refund = &tx.refunds[0];
    assert_eq!(refund.id, "[REFUND-ID]");
    assert_eq!(refund.amount, Amount::inr(100));
    assert_eq!(refund.speed_processed, RefundSpeed::Instant);
    assert_eq!(refund.status, TransactionStatus::Success);
    assert_eq!(refund.updated_timestamp.unix_timestamp(), 1_772_130_060);
}

/// Values the page does not list parse into the catch-alls.
#[tokio::test]
async fn unknown_values_are_kept() {
    let (payments, _) = lookup(json!({
      "payments": [{
        "reference_id": "r",
        "status": "refunded",
        "currency": "INR",
        "amount": {"value": 1, "offset": 100},
        "transactions": [{
          "id": "o", "type": "cashfree", "status": "authorized",
          "created_timestamp": 1, "updated_timestamp": 2,
          "method": {"type": "emi"}, "new_field": true
        }],
        "new_field": {"x": 1}
      }]
    }))
    .await;
    let p = &payments[0];
    assert_eq!(p.status, PaymentStatus::Other("refunded".into()));
    let tx = &p.transactions[0];
    assert_eq!(tx.gateway, GatewayType::Other("cashfree".into()));
    assert_eq!(tx.status, TransactionStatus::Other("authorized".into()));
    assert_eq!(
        tx.method.as_ref().map(|m| &m.kind),
        Some(&PaymentMethodType::Other("emi".into()))
    );
}

#[tokio::test]
async fn lookup_names_are_one_segment_each() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"payments": []}));
    let payments = client(&t)
        .payments(PhoneNumberId::new(PHONE_ID).unwrap())
        .lookup("config/../x", "ref?a=1")
        .await
        .unwrap();
    assert!(payments.is_empty());
    assert_eq!(
        t.last_request().unwrap().path(),
        format!("/v25.0/{PHONE_ID}/payments/config%2F..%2Fx/ref%3Fa=1")
    );
    for (config, reference) in [("", "ref"), ("pc", ""), ("..", "ref"), ("pc", ".")] {
        let err = client(&t)
            .payments(PhoneNumberId::new(PHONE_ID).unwrap())
            .lookup(config, reference)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Validation(_)),
            "{config:?} {reference:?}"
        );
    }
    assert_eq!(t.remaining(), 0);
    assert_eq!(t.requests().len(), 1);
}

/// `payments-in/pg`, step 5: the lookup's "generic error", not Graph's
/// envelope, is an `Error::Http` (and a GET, replayed).
#[tokio::test]
async fn the_lookup_error_example_is_an_http_error() {
    let generic = json!({
      "errors": [{"code": 500, "title": "Generic error", "details": "System error. Please try again."}]
    });
    let t = ScriptedTransport::new();
    t.push_json(500, generic.clone());
    t.push_json(500, generic.clone());
    t.push_json(500, generic);
    let err = retrying_client(&t)
        .payments(PhoneNumberId::new(PHONE_ID).unwrap())
        .lookup("pc", "ref")
        .await
        .unwrap_err();
    let Error::Http {
        status,
        body_snippet,
    } = &err
    else {
        panic!("expected Error::Http, got {err:?}");
    };
    assert_eq!(*status, 500);
    assert!(body_snippet.contains("Generic error"));
    assert_eq!(t.requests().len(), 3, "a read is replayed");
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn lookup_is_replayed_on_a_transient_error() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, json!({"payments": []}));
    retrying_client(&t)
        .payments(PhoneNumberId::new(PHONE_ID).unwrap())
        .lookup("pc", "ref")
        .await
        .unwrap();
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);
}

/// A payment that does not decode keeps the merchant's fields out of the
/// error.
#[tokio::test]
async fn an_unreadable_payment_is_withheld() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"payments": [{"notes": {"buyer": "SECRET-BUYER"}}]}),
    );
    let err = client(&t)
        .payments(PhoneNumberId::new(PHONE_ID).unwrap())
        .lookup("pc", "ref")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }));
    assert!(!format!("{err} {err:?}").contains("SECRET-BUYER"));
}

// ─── Refunds ─────────────────────────────────────────────────────────────

/// `payments-in/pg`, step 8: the refund request and answer.
#[tokio::test]
async fn refund_example() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"id": "refund-id", "status": "pending", "speed_processed": "normal"}),
    );
    let created = client(&t)
        .payments(PhoneNumberId::new(PHONE_ID).unwrap())
        .refund(
            &RefundRequest::inr("reference-id-value", "payment-config-id", Amount::inr(100))
                .speed(RefundSpeed::Normal),
        )
        .await
        .unwrap();
    assert_eq!(created.id, "refund-id");
    assert_eq!(created.status, TransactionStatus::Pending);
    assert_eq!(created.speed_processed, RefundSpeed::Normal);
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{PHONE_ID}/payments_refund"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({
          "reference_id": "reference-id-value",
          "speed": "normal",
          "payment_config_id": "payment-config-id",
          "amount": {"currency": "INR", "value": "100", "offset": "100"}
        }))
    );
    assert_eq!(t.remaining(), 0);

    // The example's value and offset are both 100: a partial refund of
    // ₹12.34, without a speed, tells them apart.
    t.push_json(
        200,
        json!({"id": "refund-id", "status": "completed", "speed_processed": "instant"}),
    );
    let created = client(&t)
        .payments(PhoneNumberId::new(PHONE_ID).unwrap())
        .refund(&RefundRequest::inr("BM345A-12", "pc", Amount::inr(1234)))
        .await
        .unwrap();
    assert_eq!(created.status, TransactionStatus::Completed);
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(json!({
          "reference_id": "BM345A-12",
          "payment_config_id": "pc",
          "amount": {"currency": "INR", "value": "1234", "offset": "100"}
        }))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn refund_is_never_replayed() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let err = retrying_client(&t)
        .payments(PhoneNumberId::new(PHONE_ID).unwrap())
        .refund(&RefundRequest::inr("r", "pc", Amount::inr(100)))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(t.requests().len(), 1, "a replay could refund twice");
    assert_eq!(t.remaining(), 0);

    // Nor on a server error: the refund may have been made.
    t.push_json(
        503,
        json!({"error": {"message": "x", "code": 2, "is_transient": true}}),
    );
    let err = retrying_client(&t)
        .payments(PhoneNumberId::new(PHONE_ID).unwrap())
        .refund(&RefundRequest::inr("r", "pc", Amount::inr(100)))
        .await
        .unwrap_err();
    assert!(
        err.is_retryable(),
        "a transient error, not replayed all the same"
    );
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn refund_is_checked_first() {
    let cases = [
        (RefundRequest::inr("", "pc", Amount::inr(1)), "reference_id"),
        (
            RefundRequest::inr("r", "", Amount::inr(1)),
            "payment_config_id",
        ),
        (
            RefundRequest::inr("r", "pc", Amount::inr(0)),
            "amount.value",
        ),
        (
            RefundRequest::inr("r", "pc", Amount::new(1, 1)),
            "amount.offset",
        ),
    ];
    let t = ScriptedTransport::new();
    for (request, field) in cases {
        assert_eq!(request.validate().unwrap_err().field, field);
        let err = client(&t)
            .payments(PhoneNumberId::new(PHONE_ID).unwrap())
            .refund(&request)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
    }
    assert!(t.requests().is_empty());
}

// ─── Payment configurations ──────────────────────────────────────────────

fn configurations(t: &ScriptedTransport) -> PaymentConfigurations {
    client(t).payment_configurations(WabaId::new(WABA).unwrap())
}

/// The configuration of the onboarding page's answers.
#[track_caller]
fn assert_test_configuration(c: &PaymentConfiguration) {
    assert_eq!(c.configuration_name, "test-payment-configuration");
    let mcc = c.merchant_category_code.as_ref().unwrap();
    assert_eq!(
        (mcc.code.as_str(), mcc.description.as_str()),
        ("0000", "Test MCC Code")
    );
    let purpose = c.purpose_code.as_ref().unwrap();
    assert_eq!(
        (purpose.code.as_str(), purpose.description.as_str()),
        ("00", "Test Purpose Code")
    );
    assert_eq!(c.status, PaymentConfigurationStatus::Active);
    assert_eq!(c.provider_mid.as_deref(), Some("test-payment-gateway-mid"));
    // "RazorPay" in the example; the table writes `razorpay`.
    assert_eq!(c.provider_name, Some(PaymentProvider::Razorpay));
    assert_eq!(
        c.created_timestamp
            .map(time::OffsetDateTime::unix_timestamp),
        Some(1_720_203_204)
    );
    assert_eq!(
        c.updated_timestamp
            .map(time::OffsetDateTime::unix_timestamp),
        Some(1_721_088_316)
    );
    assert_eq!(c.merchant_vpa, None);
}

fn sample_configuration() -> Value {
    json!({
      "configuration_name": "test-payment-configuration",
      "merchant_category_code": {"code": "0000", "description": "Test MCC Code"},
      "purpose_code": {"code": "00", "description": "Test Purpose Code"},
      "status": "Active",
      "provider_mid": "test-payment-gateway-mid",
      "provider_name": "RazorPay",
      "created_timestamp": 1720203204,
      "updated_timestamp": 1721088316
    })
}

/// `payments-in/onboarding-apis`, "Get all payment configurations" (the
/// example's `....` dropped).
#[tokio::test]
async fn list_example() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"data": [{"payment_configurations": [sample_configuration()]}]}),
    );
    let list = configurations(&t).list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_test_configuration(&list[0]);
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), format!("/v25.0/{WABA}/payment_configurations"));
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(t.remaining(), 0);
}

/// "Get a specific payment configuration".
#[tokio::test]
async fn get_example() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"data": [sample_configuration()]}));
    t.push_json(200, json!({"data": []}));
    let one = configurations(&t)
        .get("test-payment-configuration")
        .await
        .unwrap()
        .unwrap();
    assert_test_configuration(&one);
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(
        req.path(),
        format!("/v25.0/{WABA}/payment_configuration/test-payment-configuration")
    );
    assert_eq!(configurations(&t).get("a/b").await.unwrap(), None);
    assert_eq!(
        t.last_request().unwrap().path(),
        format!("/v25.0/{WABA}/payment_configuration/a%2Fb")
    );
    assert_eq!(t.remaining(), 0);
}

/// Every spelling of the statuses Meta prints.
#[test]
fn configuration_statuses_parse_in_every_spelling() {
    for (raw, status) in [
        ("Active", PaymentConfigurationStatus::Active),
        (
            "Needs_Connecting",
            PaymentConfigurationStatus::NeedsConnecting,
        ),
        (
            "Needs Connecting",
            PaymentConfigurationStatus::NeedsConnecting,
        ),
        ("Needs_Testing", PaymentConfigurationStatus::NeedsTesting),
        ("Needs Testing", PaymentConfigurationStatus::NeedsTesting),
        (
            "Suspended",
            PaymentConfigurationStatus::Other("Suspended".into()),
        ),
    ] {
        let parsed: PaymentConfigurationStatus = serde_json::from_value(json!(raw)).unwrap();
        assert_eq!(parsed, status, "{raw}");
    }
    assert_eq!(
        serde_json::to_value(PaymentConfigurationStatus::NeedsTesting).unwrap(),
        json!("Needs_Testing")
    );
}

/// "Create a payment configuration": both examples and their answers.
#[tokio::test]
async fn create_examples() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({
          "oauth_url": "https://www.facebook.com/payment/onboarding/init/",
          "expiration": 1721687287,
          "success": true
        }),
    );
    t.push_json(200, json!({"success": true}));
    let gateway =
        CreatePaymentConfiguration::new("test-payment-configuration", PaymentProvider::Razorpay)
            .purpose_code("00")
            .merchant_category_code("0000")
            .redirect_url("https://test-redirect-url.com");
    let link = configurations(&t).create(&gateway).await.unwrap();
    assert_eq!(
        link.oauth_url.as_ref().map(SecretUrl::expose_secret),
        Some("https://www.facebook.com/payment/onboarding/init/")
    );
    assert_eq!(
        link.expiration.map(time::OffsetDateTime::unix_timestamp),
        Some(1_721_687_287)
    );
    let req = t.requests()[0].clone();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{WABA}/payment_configuration"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({
          "configuration_name": "test-payment-configuration",
          "purpose_code": "00",
          "merchant_category_code": "0000",
          "provider_name": "razorpay",
          "redirect_url": "https://test-redirect-url.com"
        }))
    );

    let vpa = CreatePaymentConfiguration::upi_vpa(
        "test-payment-configuration",
        "test-upi-merchant-vpa@test",
    )
    .purpose_code("00")
    .merchant_category_code("0000");
    let link = configurations(&t).create(&vpa).await.unwrap();
    assert_eq!(link.oauth_url, None);
    assert_eq!(
        t.last_request().unwrap().json(),
        Some(json!({
          "configuration_name": "test-payment-configuration",
          "purpose_code": "00",
          "merchant_category_code": "0000",
          "provider_name": "upi_vpa",
          "merchant_vpa": "test-upi-merchant-vpa@test"
        }))
    );
    assert_eq!(t.remaining(), 0);
}

/// The OAuth link is a credential: it stays out of `Debug` and of decode
/// errors.
#[tokio::test]
async fn the_oauth_link_is_kept_out_of_debug_and_errors() {
    let link: PaymentOauthLink = serde_json::from_value(json!({
      "oauth_url": "https://www.facebook.com/payment/onboarding/init/?token=SENTINEL"
    }))
    .unwrap();
    assert!(!format!("{link:?}").contains("SENTINEL"));
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"oauth_url": "https://x/?token=SENTINEL", "success": "yes"}),
    );
    let err = configurations(&t)
        .create(&CreatePaymentConfiguration::new("c", PaymentProvider::Payu))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode { .. }));
    assert!(!format!("{err} {err:?}").contains("SENTINEL"));

    // No page says where the credential sits: the path is hidden too.
    let link: PaymentOauthLink = serde_json::from_value(json!({
      "oauth_url": "https://www.facebook.com/payment/onboarding/init/SENTINEL/?state=SENTINEL",
      "expiration": 1721687287
    }))
    .unwrap();
    let shown = format!("{link:?}");
    assert!(!shown.contains("SENTINEL"), "{shown}");
    assert!(shown.contains("https://www.facebook.com"), "{shown}");
    assert!(shown.contains("expiration"), "{shown}");
}

/// A refusal that is not Meta's envelope (a proxy's page, a cut-off
/// answer) may still hold the link: its body is withheld, from both
/// endpoints that answer one. Meta's envelope is kept.
#[tokio::test]
async fn an_oauth_endpoints_http_error_withholds_its_body() {
    let leaky = || {
        bytes::Bytes::from_static(
            br#"{"oauth_url": "https://www.facebook.com/payment/onboarding/init/?t=SENTINEL", "success"#,
        )
    };
    let t = ScriptedTransport::new();
    t.push_bytes(502, "application/json", leaky());
    t.push_bytes(502, "application/json", leaky());
    let errors = [
        configurations(&t)
            .create(&CreatePaymentConfiguration::new("c", PaymentProvider::Payu))
            .await
            .unwrap_err(),
        configurations(&t)
            .regenerate_oauth_link("c", None)
            .await
            .unwrap_err(),
    ];
    for err in errors {
        assert!(matches!(err, Error::Http { status: 502, .. }), "{err:?}");
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("SENTINEL"), "{shown}");
        assert!(shown.contains("withheld"), "{shown}");
    }
    t.push_json(
        400,
        json!({"error": {"message": "(#100) Invalid parameter", "code": 100}}),
    );
    let err = configurations(&t)
        .regenerate_oauth_link("c", None)
        .await
        .unwrap_err();
    assert_eq!(err.graph().map(|g| g.code), Some(100));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn create_refuses_success_false_and_is_never_replayed() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": false}));
    let request = CreatePaymentConfiguration::new("c", PaymentProvider::Zaakpay);
    let err = configurations(&t).create(&request).await.unwrap_err();
    assert!(matches!(err, Error::Http { status: 200, .. }));

    t.push_error(|| TransportError::Timeout);
    let err = retrying_client(&t)
        .payment_configurations(WabaId::new(WABA).unwrap())
        .create(&request)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(
        t.requests().len(),
        2,
        "one each: the timeout is not replayed"
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn create_is_checked_first() {
    let t = ScriptedTransport::new();
    let cases = [
        (
            CreatePaymentConfiguration::new("", PaymentProvider::Payu),
            "configuration_name",
        ),
        (
            CreatePaymentConfiguration::new("x".repeat(61), PaymentProvider::Payu),
            "configuration_name",
        ),
        (
            CreatePaymentConfiguration::new("c", PaymentProvider::Other(String::new())),
            "provider_name",
        ),
        (CreatePaymentConfiguration::upi_vpa("c", ""), "merchant_vpa"),
        (
            CreatePaymentConfiguration::new("c", PaymentProvider::Payu).redirect_url(""),
            "redirect_url",
        ),
    ];
    for (request, field) in cases {
        assert_eq!(request.validate().unwrap_err().field, field);
        let err = configurations(&t).create(&request).await.unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
    }
    assert!(t.requests().is_empty());
}

/// The onboarding page's refusal (terms of service pending, `131005`).
#[tokio::test]
async fn onboarding_refusal_is_a_permission_error() {
    let t = ScriptedTransport::new();
    t.push_json(
        403,
        json!({
          "error": {
            "message": "(#131005) Access denied",
            "type": "OAuthException",
            "code": 131005,
            "error_data": {
              "messaging_product": "whatsapp",
              "details": "WhatsApp Payments Terms of Service acceptance pending for this WhatsApp Business Account. Please use the following link to accept terms of service before using Business APIs: https://fb.me/12345"
            }
          }
        }),
    );
    let err = configurations(&t).list().await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Permission);
    assert_eq!(err.graph().map(|g| g.code), Some(131005));
}

/// "Regenerate payment configuration OAuth link".
#[tokio::test]
async fn regenerate_oauth_link_example() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({
          "oauth_url": "https://www.facebook.com/payment/onboarding/init/",
          "expiration": 1721687287
        }),
    );
    let link = configurations(&t)
        .regenerate_oauth_link(
            "test-payment-configuration",
            Some("https://test-redirect-url.com"),
        )
        .await
        .unwrap();
    assert_eq!(
        link.oauth_url.as_ref().map(SecretUrl::expose_secret),
        Some("https://www.facebook.com/payment/onboarding/init/")
    );
    assert_eq!(
        link.expiration.map(time::OffsetDateTime::unix_timestamp),
        Some(1_721_687_287)
    );
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(
        req.path(),
        format!("/v25.0/{WABA}/generate_payment_configuration_oauth_link")
    );
    assert_eq!(
        req.json(),
        Some(json!({
          "configuration_name": "test-payment-configuration",
          "redirect_url": "https://test-redirect-url.com"
        }))
    );
    assert_eq!(t.remaining(), 0);

    t.push_error(|| TransportError::Timeout);
    let err = retrying_client(&t)
        .payment_configurations(WabaId::new(WABA).unwrap())
        .regenerate_oauth_link("c", None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Transport(TransportError::Timeout)));
    assert_eq!(t.requests().len(), 2, "the timeout is not replayed");
    assert!(matches!(
        configurations(&t).regenerate_oauth_link("", None).await,
        Err(Error::Validation(_))
    ));
    assert_eq!(t.requests().len(), 2);
}

/// "Remove a payment configuration": a DELETE with a JSON body.
#[tokio::test]
async fn delete_example() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, json!({"success": true}));
    retrying_client(&t)
        .payment_configurations(WabaId::new(WABA).unwrap())
        .delete("test-payment-configuration")
        .await
        .unwrap();
    assert_eq!(t.requests().len(), 2, "a deletion is replayed");
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::DELETE);
    assert_eq!(req.path(), format!("/v25.0/{WABA}/payment_configuration"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({"configuration_name": "test-payment-configuration"}))
    );
    assert_eq!(t.remaining(), 0);
    assert!(matches!(
        configurations(&t).delete("").await,
        Err(Error::Validation(_))
    ));
    assert_eq!(t.requests().len(), 2);
}
