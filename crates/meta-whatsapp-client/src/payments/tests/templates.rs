//! The payment templates (roadmap P1c): every creation and send example of
//! `payments-in/orderdetailstemplate`, `payments-in/orderstatustemplate`,
//! `payments-br/orderdetailstemplate` and `payments-br/payment-request-cta`,
//! sent and compared as exact JSON, parsed back into the same value, and
//! the rules each part is checked by. Placeholders the examples leave
//! unfilled (`<TEMPLATE_NAME>`, `<ORDER_ITEM_NAME>`) are filled with a
//! plausible value; a button `index` the page prints as a number is
//! compared as the string this client sends (`templates/send.rs`, "Button
//! `index`"); everything else is copied.

use pretty_assertions::assert_eq;

use super::*;
use crate::templates::{
    Button, DisplayFormat, HeaderFormat, Parameter, ParameterAction, SendComponent,
    TemplateCategory, TemplateComponent, TemplateDefinition, TemplateMessage, TemplateSubCategory,
};

/// The examples' Pix code, copied (its `...` included).
const PIX_CODE: &str = "00020101021226700014br.gov.bcb.pix2548pix.example.com...";
/// The examples' Boleto digitable line.
const DIGITABLE_LINE: &str = "03399026944140000002628346101018898510000008848";

/// A button `index` printed as a number, as the string this client sends.
fn string_index(mut v: Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(map) => {
                for (k, val) in map.iter_mut() {
                    if k == "index"
                        && let Value::Number(n) = val
                    {
                        *val = Value::String(n.to_string());
                    } else {
                        walk(val);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(&mut v);
    v
}

/// Run `f` on a current-thread runtime.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

/// Create `definition` against Meta's `answer` and return the body that
/// went out, after checking the request and that nothing else was sent.
#[track_caller]
fn created(
    definition: &TemplateDefinition,
    answer: Value,
) -> (Value, crate::templates::TemplateCreated) {
    let t = ScriptedTransport::new();
    t.push_json(200, answer);
    let created = block_on(
        client(&t)
            .templates(WabaId::new(WABA).unwrap())
            .create(definition),
    )
    .unwrap_or_else(|e| panic!("documented example refused: {e}"));
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{WABA}/message_templates"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(t.remaining(), 0);
    (req.json().unwrap(), created)
}

/// Send `template` against Meta's `answer` and return the body that went
/// out, after checking the request and that nothing else was sent.
#[track_caller]
fn sent_template(template: TemplateMessage, answer: &Value) -> Value {
    let message = OutboundMessage::new(Recipient::phone(TO), template);
    if let Err(e) = message.validate() {
        panic!("documented example failed validation: {e}");
    }
    let t = ScriptedTransport::new();
    t.push_json(200, answer.clone());
    let response = block_on(
        client(&t)
            .messages(PhoneNumberId::new(PHONE_ID).unwrap())
            .send(&message),
    )
    .unwrap();
    assert_eq!(
        response
            .message_id()
            .map(|m| m.as_str().to_owned())
            .as_deref(),
        answer["messages"][0]["id"].as_str()
    );
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{PHONE_ID}/messages"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(t.remaining(), 0);
    req.json().unwrap()
}

/// The order details pages' send response.
fn wamid_response() -> Value {
    json!({
        "messaging_product": "whatsapp",
        "contacts": [{"input": TO, "wa_id": TO}],
        "messages": [{"id": "wamid.ID"}]
    })
}

/// The `template` of a send body parses back into `built`.
#[track_caller]
fn parses_back(body: &Value, built: &TemplateMessage) {
    let parsed: TemplateMessage = serde_json::from_value(body["template"].clone()).unwrap();
    assert_eq!(&parsed, built);
    parsed.validate().unwrap();
}

/// The validation error of `template`, as `(field, reason)`.
#[track_caller]
fn refused(template: &TemplateMessage) -> String {
    template.validate().unwrap_err().field
}

/// The validation error of `definition`'s field.
#[track_caller]
fn refused_definition(definition: &TemplateDefinition) -> String {
    definition.validate().unwrap_err().field
}

// ─── India: order details template ──────────────────────────────────────

#[test]
fn india_order_details_template_creation_example() {
    let definition = TemplateDefinition::new("order_bill", "en_US", TemplateCategory::Utility)
        .component(TemplateComponent::header_text("Your order"))
        .component(TemplateComponent::body("Review your order and pay."))
        .component(TemplateComponent::footer("Lucky Shrub"))
        .component(TemplateComponent::buttons([Button::order_details(
            "Review and Pay",
        )]));
    let doc = json!({
      "name": "order_bill",
      "language": "en_US",
      "category": "UTILITY",
      "components": [
        {"type": "HEADER", "format": "TEXT", "text": "Your order"},
        {"type": "BODY", "text": "Review your order and pay."},
        {"type": "FOOTER", "text": "Lucky Shrub"},
        {"type": "BUTTONS", "buttons": [{"type": "ORDER_DETAILS", "text": "Review and Pay"}]}
      ]
    });
    let (body, created) = created(
        &definition,
        json!({"id": "123455683495832", "status": "PENDING", "category": "UTILITY"}),
    );
    assert_eq!(body, doc);
    assert_eq!(created.id.as_str(), "123455683495832");
    // A `GET` of it parses into the typed button, not `Other`.
    let parsed: TemplateDefinition = serde_json::from_value(doc).unwrap();
    assert_eq!(parsed, definition);
    // The page's commented alternative: `MARKETING` with `display_format`
    // (spelled in lower case there; sent upper case).
    let marketing: TemplateDefinition = serde_json::from_value(json!({
        "name": "order_bill", "language": "en_US", "category": "MARKETING",
        "display_format": "order_details",
        "components": [
            {"type": "BODY", "text": "Review your order and pay."},
            {"type": "BUTTONS", "buttons": [{"type": "ORDER_DETAILS", "text": "Review and Pay"}]}
        ]
    }))
    .unwrap();
    assert_eq!(marketing.display_format, Some(DisplayFormat::OrderDetails));
    marketing.validate().unwrap();
    assert_eq!(
        serde_json::to_value(&marketing).unwrap()["display_format"],
        "ORDER_DETAILS"
    );
}

/// The India send example's order, typed.
fn india_template_parameters() -> OrderDetailsParameters {
    let item = OrderItem::new("Indoor plant", Amount::inr(400), 1)
        .retailer_id("SKU-PLANT-1")
        .import(
            "India",
            "Lucky Shrub Imports",
            ImporterAddress::new("B8/733 nand nagri", "East Delhi", "110093", "IN"),
        );
    let order = Order::new(
        [item],
        Amount::inr(400),
        DescribedAmount::new(Amount::inr(500)),
    )
    .shipping(DescribedAmount::new(Amount::inr(0)))
    .discount(Discount::new(Amount::inr(250)));
    let mut params = OrderDetailsParameters::inr(
        "reference_id_value",
        GoodsType::Digital,
        PaymentSetting::upi_intent_link(
            "upi://pay?pa=merchant_vpa&pn=merchant%20Name&mc=mc_code&purpose=purpose_code&tr=transaction_record",
        ),
        order,
    )
    .payment_type(PaymentType::Upi);
    params.payment_configuration = Some("unique_payment_config_id".to_owned());
    params
}

#[test]
fn india_order_details_template_send_example() {
    let template = TemplateMessage::new("order_bill", "en_US")
        .deterministic()
        .header(Parameter::image_link("https://the-url"))
        .order_details_button(0, india_template_parameters());
    let doc = json!({
      "messaging_product": "whatsapp",
      "recipient_type": "individual",
      "to": TO,
      "type": "template",
      "template": {
        "name": "order_bill",
        "language": {"policy": "deterministic", "code": "en_US"},
        "components": [
          {"type": "header", "parameters": [{"type": "image", "image": {"link": "https://the-url"}}]},
          {
            "type": "button",
            "sub_type": "order_details",
            "index": 0,
            "parameters": [{
              "type": "action",
              "action": {
                "order_details": {
                  "currency": "INR",
                  "order": {
                    "discount": {"offset": 100, "value": 250},
                    "items": [{
                      "amount": {"offset": 100, "value": 400},
                      "name": "Indoor plant",
                      "quantity": 1,
                      "retailer_id": "SKU-PLANT-1",
                      "country_of_origin": "India",
                      "importer_name": "Lucky Shrub Imports",
                      "importer_address": {
                        "address_line1": "B8/733 nand nagri",
                        "city": "East Delhi",
                        "country_code": "IN",
                        "postal_code": "110093"
                      }
                    }],
                    "shipping": {"offset": 100, "value": 0},
                    "status": "pending",
                    "subtotal": {"offset": 100, "value": 400},
                    "tax": {"offset": 100, "value": 500}
                  },
                  "payment_settings": [{
                    "type": "upi_intent_link",
                    "upi_intent_link": {
                      "link": "upi://pay?pa=merchant_vpa&pn=merchant%20Name&mc=mc_code&purpose=purpose_code&tr=transaction_record"
                    }
                  }],
                  "payment_configuration": "unique_payment_config_id",
                  "payment_type": "upi",
                  "reference_id": "reference_id_value",
                  "total_amount": {"offset": 100, "value": 650},
                  "type": "digital-goods"
                }
              }
            }]
          }
        ]
      }
    });
    let body = sent_template(template.clone(), &wamid_response());
    assert_eq!(body, string_index(doc.clone()));
    // Meta's numeric index parses too, into the same typed button.
    parses_back(&doc, &template);
    let SendComponent::Button { parameters, .. } = &template.components[1] else {
        panic!()
    };
    assert!(matches!(
        &parameters[0],
        Parameter::Action { action } if action.order_details.is_some()
    ));
}

// ─── India: order status template ───────────────────────────────────────

#[test]
fn india_order_status_template_creation_example() {
    let definition = TemplateDefinition::new("order_update", "en_US", TemplateCategory::Utility)
        .sub_category(TemplateSubCategory::OrderStatus)
        .component(TemplateComponent::body("Your order has an update."))
        .component(TemplateComponent::footer("Lucky Shrub"));
    let doc = json!({
      "name": "order_update",
      "language": "en_US",
      "category": "UTILITY",
      "sub_category": "ORDER_STATUS",
      "components": [
        {"type": "BODY", "text": "Your order has an update."},
        {"type": "FOOTER", "text": "Lucky Shrub"}
      ]
    });
    let (body, _) = created(
        &definition,
        json!({"id": "123455683495833", "status": "PENDING", "category": "UTILITY"}),
    );
    assert_eq!(body, doc);
    assert_eq!(
        serde_json::from_value::<TemplateDefinition>(doc).unwrap(),
        definition
    );
}

#[test]
fn india_order_status_template_send_example() {
    // The example lists the statuses as `processing | partially_shipped |
    // shipped | completed | canceled`; the page sends `shipped`.
    let template = TemplateMessage::new("order_update", "en_US")
        .deterministic()
        .order_status(
            "reference_id_value",
            OrderUpdate::new(OrderStatusValue::Shipped).description("Your plant is on its way"),
        );
    let doc = json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": TO,
        "type": "template",
        "template": {
            "name": "order_update",
            "language": {"policy": "deterministic", "code": "en_US"},
            "components": [{
                "type": "order_status",
                "parameters": [{
                    "type": "order_status",
                    "order_status": {
                        "reference_id": "reference_id_value",
                        "order": {"status": "shipped", "description": "Your plant is on its way"}
                    }
                }]
            }]
        }
    });
    let body = sent_template(template.clone(), &wamid_response());
    assert_eq!(body, doc);
    parses_back(&doc, &template);
    for status in ["processing", "partially_shipped", "completed", "canceled"] {
        let mut other = doc["template"].clone();
        other["components"][0]["parameters"][0]["order_status"]["order"]["status"] = status.into();
        let parsed: TemplateMessage = serde_json::from_value(other).unwrap();
        parsed.validate().unwrap();
        assert!(
            !format!("{parsed:?}").contains("Other("),
            "{status}: {parsed:?}"
        );
    }
}

// ─── Brazil: order details template ─────────────────────────────────────

#[test]
fn brazil_order_details_template_creation_example() {
    let definition = TemplateDefinition::new("pedido", "pt_BR", TemplateCategory::Utility)
        .display_format(DisplayFormat::OrderDetails)
        .component(TemplateComponent::header_text("Seu pedido"))
        .component(TemplateComponent::body("Confira seu pedido e pague."))
        .component(TemplateComponent::footer("Padaria"))
        .component(TemplateComponent::buttons([Button::order_details(
            "Copy Pix code",
        )]));
    let doc = json!({
      "name": "pedido",
      "language": "pt_BR",
      "category": "UTILITY",
      "display_format": "ORDER_DETAILS",
      "components": [
        {"type": "HEADER", "format": "TEXT", "text": "Seu pedido"},
        {"type": "BODY", "text": "Confira seu pedido e pague."},
        {"type": "FOOTER", "text": "Padaria"},
        {"type": "BUTTONS", "buttons": [{"type": "ORDER_DETAILS", "text": "Copy Pix code"}]}
      ]
    });
    let (body, _) = created(
        &definition,
        json!({"id": "123455683495834", "status": "PENDING", "category": "UTILITY"}),
    );
    assert_eq!(body, doc);
    assert_eq!(
        serde_json::from_value::<TemplateDefinition>(doc).unwrap(),
        definition
    );
}

#[test]
fn brazil_order_details_template_send_example() {
    let pix = PixDynamicCode::new(
        PIX_CODE,
        "Account holder name",
        "39580525000189",
        PixKeyType::Cnpj,
    );
    let order = Order::new(
        [OrderItem::new("Cake", Amount::brl(50000), 1).retailer_id("1234567")],
        Amount::brl(50000),
        DescribedAmount::new(Amount::brl(0)).description("optional text"),
    );
    let params =
        OrderDetailsParameters::brl("pedido-2026-000123-1", GoodsType::Digital, pix, order);
    let template = TemplateMessage::new("pedido", "pt_BR")
        .deterministic()
        .order_details_button(0, params);
    let doc = json!({
      "messaging_product": "whatsapp",
      "recipient_type": "individual",
      "to": TO,
      "type": "template",
      "template": {
        "name": "pedido",
        "language": {"policy": "deterministic", "code": "pt_BR"},
        "components": [{
          "type": "button",
          "sub_type": "order_details",
          "index": 0,
          "parameters": [{
            "type": "action",
            "action": {
              "order_details": {
                "reference_id": "pedido-2026-000123-1",
                "type": "digital-goods",
                "payment_type": "br",
                "payment_settings": [{
                  "type": "pix_dynamic_code",
                  "pix_dynamic_code": {
                    "code": PIX_CODE,
                    "merchant_name": "Account holder name",
                    "key": "39580525000189",
                    "key_type": "CNPJ"
                  }
                }],
                "currency": "BRL",
                "total_amount": {"value": 50000, "offset": 100},
                "order": {
                  "status": "pending",
                  "tax": {"value": 0, "offset": 100, "description": "optional text"},
                  "items": [{
                    "retailer_id": "1234567",
                    "name": "Cake",
                    "amount": {"value": 50000, "offset": 100},
                    "quantity": 1
                  }],
                  "subtotal": {"value": 50000, "offset": 100}
                }
              }
            }
          }]
        }]
      }
    });
    let body = sent_template(template.clone(), &wamid_response());
    assert_eq!(body, string_index(doc.clone()));
    parses_back(&doc, &template);
}

// ─── Brazil: payment request template ───────────────────────────────────

/// The creation example's three buttons.
fn payment_request_definition(buttons: impl IntoIterator<Item = Button>) -> TemplateDefinition {
    TemplateDefinition::new("cobranca", "pt_BR", TemplateCategory::Utility)
        .component(TemplateComponent::header_text("Header Text"))
        .component(TemplateComponent::body("Body Text"))
        .component(TemplateComponent::footer("Footer text"))
        .component(TemplateComponent::buttons(buttons))
}

#[test]
fn payment_request_template_creation_example() {
    let definition = payment_request_definition([
        Button::payment_request(PaymentRequestSetting::boleto(DIGITABLE_LINE)),
        Button::payment_request(PaymentRequestSetting::pix_dynamic_code(PIX_CODE)),
        Button::payment_request(PaymentRequestSetting::payment_link(
            "https://my-payment-link-url",
        )),
    ]);
    // The page prints the header format in lower case (`"text"`); the
    // client sends the reference's upper case, and parses either.
    let doc = json!({
      "name": "cobranca",
      "language": "pt_BR",
      "category": "UTILITY",
      "components": [
        {"type": "HEADER", "format": "text", "text": "Header Text"},
        {"type": "BODY", "text": "Body Text"},
        {"type": "FOOTER", "text": "Footer text"},
        {
          "type": "BUTTONS",
          "buttons": [
            {
              "type": "PAYMENT_REQUEST",
              "text": "Copy Boleto code",
              "payment_setting": {"type": "boleto", "boleto": {"digitable_line": DIGITABLE_LINE}}
            },
            {
              "type": "PAYMENT_REQUEST",
              "text": "Copy Pix code",
              "payment_setting": {"type": "pix_dynamic_code", "pix_dynamic_code": {"code": PIX_CODE}}
            },
            {
              "type": "PAYMENT_REQUEST",
              "text": "Open payment link",
              "payment_setting": {"type": "payment_link", "payment_link": {"uri": "https://my-payment-link-url"}}
            }
          ]
        }
      ]
    });
    let (body, created) = created(
        &definition,
        json!({"id": "123455683495832", "status": "Approved", "category": "UTILITY"}),
    );
    let mut upper = doc.clone();
    upper["components"][0]["format"] = "TEXT".into();
    assert_eq!(body, upper);
    assert_eq!(created.id.as_str(), "123455683495832");
    assert_eq!(
        created.status,
        Some(crate::templates::TemplateStatus::Approved)
    );
    assert_eq!(created.category, Some(TemplateCategory::Utility));
    assert_eq!(
        serde_json::from_value::<TemplateDefinition>(doc).unwrap(),
        definition
    );

    // "One-click payment request button": `type` alone, replacing one.
    let one_click = payment_request_definition([
        Button::payment_request(PaymentRequestSetting::boleto(DIGITABLE_LINE)),
        Button::payment_request(PaymentRequestSetting::pix_dynamic_code(PIX_CODE)),
        Button::payment_request(PaymentRequestSetting::offsite_card_pay()),
    ]);
    one_click.validate().unwrap();
    let button = json!({
      "type": "PAYMENT_REQUEST",
      "text": "Review payment",
      "payment_setting": {"type": "offsite_card_pay"}
    });
    assert_eq!(
        serde_json::to_value(&one_click).unwrap()["components"][3]["buttons"][2],
        button
    );
    assert_eq!(
        serde_json::from_value::<Button>(button).unwrap(),
        Button::payment_request(PaymentRequestSetting::offsite_card_pay())
    );
}

/// The send example's three button components.
fn payment_request_send() -> TemplateMessage {
    TemplateMessage::new("cobranca", "pt_BR")
        .payment_request_button(
            0,
            PaymentRequest::new(PaymentRequestSetting::boleto(DIGITABLE_LINE)),
        )
        .payment_request_button(
            1,
            PaymentRequest::new(PaymentRequestSetting::pix_dynamic_code(PIX_CODE)),
        )
        .payment_request_button(
            2,
            PaymentRequest::new(PaymentRequestSetting::payment_link(
                "https://my-payment-link-url",
            )),
        )
}

fn payment_request_component(index: &str, payment_request: &Value) -> Value {
    json!({
      "type": "button",
      "sub_type": "payment_request",
      "index": index,
      "parameters": [{"type": "action", "action": {"payment_request": payment_request}}]
    })
}

#[test]
fn payment_request_template_send_example() {
    let doc = json!({
      "messaging_product": "whatsapp",
      "recipient_type": "individual",
      "to": TO,
      "type": "template",
      "template": {
        "name": "cobranca",
        "language": {"code": "pt_BR"},
        "components": [
          payment_request_component("0", &json!({"payment_setting": {
              "type": "boleto", "boleto": {"digitable_line": DIGITABLE_LINE}}})),
          payment_request_component("1", &json!({"payment_setting": {
              "type": "pix_dynamic_code", "pix_dynamic_code": {"code": PIX_CODE}}})),
          payment_request_component("2", &json!({"payment_setting": {
              "type": "payment_link", "payment_link": {"uri": "https://my-payment-link-url"}}}))
        ]
      }
    });
    let answer = json!({
      "messaging_product": "whatsapp",
      "contacts": [{"input": TO, "wa_id": TO}],
      "messages": [{
        "id": "wamid.HBgNNTUxMTk4Nzg2NjI5NRUCABEYEjUxOTBGNEU0RDA2MjdCMUVBOQA=",
        "message_status": "accepted"
      }]
    });
    let template = payment_request_send();
    assert_eq!(sent_template(template.clone(), &answer), doc);
    parses_back(&doc, &template);
}

#[test]
fn one_click_and_pix_with_redirect_payment_request_examples() {
    // "One-click payment request button": `currency` and `total_amount`
    // next to `payment_setting`.
    let one_click = PaymentRequest::card(
        PaymentRequestCard::new("4242").credential_id("cred_abc123"),
        Amount::brl(1000),
    );
    let component = payment_request_component(
        "0",
        &json!({
          "currency": "BRL",
          "total_amount": {"value": 1000, "offset": 100},
          "payment_setting": {
            "type": "offsite_card_pay",
            "offsite_card_pay": {"last_four_digits": "4242", "credential_id": "cred_abc123"}
          }
        }),
    );
    let built = SendComponent::payment_request_button(0, one_click);
    assert_eq!(serde_json::to_value(&built).unwrap(), component);
    assert_eq!(
        serde_json::from_value::<SendComponent>(component).unwrap(),
        built
    );
    // "Pix with redirect payment request button".
    let redirect = PaymentRequest::new(PaymentRequestSetting::PixDynamicCode {
        code: PIX_CODE.to_owned(),
        flow_type: Some(PixFlowType::AppSwitch),
    });
    let component = payment_request_component(
        "1",
        &json!({"payment_setting": {
          "type": "pix_dynamic_code",
          "pix_dynamic_code": {"code": PIX_CODE, "flow_type": "APPSWITCH"}
        }}),
    );
    let built = SendComponent::payment_request_button(1, redirect);
    assert_eq!(serde_json::to_value(&built).unwrap(), component);
    assert_eq!(
        serde_json::from_value::<SendComponent>(component).unwrap(),
        built
    );
    // Each replaces one of the send example's components, and is sent.
    let mut template = payment_request_send();
    template.components[0] = SendComponent::payment_request_button(
        0,
        PaymentRequest::card(PaymentRequestCard::new("4242"), Amount::brl(1000)),
    );
    sent_template(template, &wamid_response());
}

// ─── The rules ──────────────────────────────────────────────────────────

const BUTTON: &str = "template.components[0].parameters[0].action";

#[test]
fn a_template_order_is_checked_by_its_currencys_rules() {
    let inr = |params: OrderDetailsParameters| {
        TemplateMessage::new("order_bill", "en_US").order_details_button(0, params)
    };
    // Fields are named in the template, not in an interactive message.
    let mut long = india_template_parameters();
    long.reference_id = "r".repeat(36);
    assert_eq!(
        refused(&inr(long)),
        format!("{BUTTON}.order_details.reference_id")
    );
    let mut total = india_template_parameters();
    total.total_amount = Amount::inr(651);
    assert_eq!(
        refused(&inr(total)),
        format!("{BUTTON}.order_details.total_amount.value")
    );
    // India's `payment_configuration` needs `payment_type` `upi`.
    let mut untyped = india_template_parameters();
    untyped.payment_type = None;
    assert_eq!(
        refused(&inr(untyped)),
        format!("{BUTTON}.order_details.payment_type")
    );
    // Brazil: 60 characters, Brazil's settings only, India's fields refused.
    let pix = PixDynamicCode::new(PIX_CODE, "Padaria", "39580525000189", PixKeyType::Cnpj);
    let brl = OrderDetailsParameters::brl_simplified(
        "r".repeat(60),
        GoodsType::Digital,
        pix.clone(),
        Amount::brl(50000),
    );
    inr(brl.clone()).validate().unwrap();
    let mut configured = brl.clone();
    configured.payment_configuration = Some("unique_payment_config_id".to_owned());
    assert_eq!(
        refused(&inr(configured)),
        format!("{BUTTON}.order_details.payment_configuration")
    );
    let in_inr = OrderDetailsParameters::inr(
        "BM345A-12",
        GoodsType::Digital,
        pix,
        india_template_parameters().order.unwrap(),
    );
    assert_eq!(
        refused(&inr(in_inr)),
        format!("{BUTTON}.order_details.payment_settings[0].type")
    );
}

#[test]
fn a_payment_request_is_checked_by_its_pages_rules() {
    let send = |request: PaymentRequest| {
        TemplateMessage::new("cobranca", "pt_BR").payment_request_button(0, request)
    };
    let at = format!("{BUTTON}.payment_request");
    // "Not used for `boleto` or `payment_link`".
    for setting in [
        PaymentRequestSetting::boleto(DIGITABLE_LINE),
        PaymentRequestSetting::payment_link("https://my-payment-link-url"),
    ] {
        let request = PaymentRequest::new(setting).total_amount(Amount::brl(1000));
        assert_eq!(refused(&send(request)), format!("{at}.currency"));
    }
    // One-click: the card and the amount are required.
    let card = PaymentRequest::new(PaymentRequestSetting::card(PaymentRequestCard::new("4242")));
    assert_eq!(refused(&send(card)), format!("{at}.currency"));
    let mut no_total = PaymentRequest::card(PaymentRequestCard::new("4242"), Amount::brl(1000));
    no_total.total_amount = None;
    assert_eq!(refused(&send(no_total)), format!("{at}.total_amount"));
    let no_card = PaymentRequest::new(PaymentRequestSetting::offsite_card_pay())
        .total_amount(Amount::brl(1000));
    assert_eq!(
        refused(&send(no_card)),
        format!("{at}.payment_setting.offsite_card_pay")
    );
    for digits in ["424", "42424", "42a2", "４２４２"] {
        let request = PaymentRequest::card(PaymentRequestCard::new(digits), Amount::brl(1000));
        assert_eq!(
            refused(&send(request)),
            format!("{at}.payment_setting.offsite_card_pay.last_four_digits"),
            "{digits}"
        );
    }
    let empty_id = PaymentRequest::card(
        PaymentRequestCard::new("4242").credential_id(""),
        Amount::brl(1000),
    );
    assert_eq!(
        refused(&send(empty_id)),
        format!("{at}.payment_setting.offsite_card_pay.credential_id")
    );
    // Pix: the amount is optional, with its currency; `BRL`, offset 100,
    // positive.
    let pix = || PaymentRequest::new(PaymentRequestSetting::pix_dynamic_code(PIX_CODE));
    send(pix()).validate().unwrap();
    send(pix().total_amount(Amount::brl(1000)))
        .validate()
        .unwrap();
    let mut no_currency = pix().total_amount(Amount::brl(1000));
    no_currency.currency = None;
    assert_eq!(refused(&send(no_currency)), format!("{at}.currency"));
    let mut no_amount = pix();
    no_amount.currency = Some(Currency::Brl);
    assert_eq!(refused(&send(no_amount)), format!("{at}.total_amount"));
    let mut rupees = pix().total_amount(Amount::brl(1000));
    rupees.currency = Some(Currency::Inr);
    assert_eq!(refused(&send(rupees)), format!("{at}.currency"));
    assert_eq!(
        refused(&send(pix().total_amount(Amount::new(1000, 1000)))),
        format!("{at}.total_amount.offset")
    );
    assert_eq!(
        refused(&send(pix().total_amount(Amount::brl(0)))),
        format!("{at}.total_amount.value")
    );
    let empty = PaymentRequest::new(PaymentRequestSetting::pix_dynamic_code(""));
    assert_eq!(
        refused(&send(empty)),
        format!("{at}.payment_setting.pix_dynamic_code.code")
    );
}

#[test]
fn an_order_status_template_is_checked_by_indias_rules() {
    let send = |reference_id: &str, order: OrderUpdate| {
        TemplateMessage::new("order_update", "en_US").order_status(reference_id, order)
    };
    let at = "template.components[0].parameters[0].order_status";
    let shipped = || OrderUpdate::new(OrderStatusValue::Shipped);
    send(&"r".repeat(35), shipped()).validate().unwrap();
    assert_eq!(
        refused(&send(&"r".repeat(36), shipped())),
        format!("{at}.reference_id")
    );
    assert_eq!(
        refused(&send("BM 345", shipped())),
        format!("{at}.reference_id")
    );
    // The example's list leaves `pending` out, as India's message does.
    assert_eq!(
        refused(&send(
            "BM345A-12",
            OrderUpdate::new(OrderStatusValue::Pending)
        )),
        format!("{at}.order.status")
    );
    send("BM345A-12", shipped().description("é".repeat(120)))
        .validate()
        .unwrap();
    assert_eq!(
        refused(&send("BM345A-12", shipped().description("é".repeat(121)))),
        format!("{at}.order.description")
    );
}

#[test]
fn payment_template_definitions_are_checked_by_their_pages_rules() {
    // The label each method requires.
    let mut wrong = payment_request_definition([Button::payment_request(
        PaymentRequestSetting::boleto(DIGITABLE_LINE),
    )]);
    let TemplateComponent::Buttons { buttons } = &mut wrong.components[3] else {
        panic!()
    };
    let Button::PaymentRequest { text, .. } = &mut buttons[0] else {
        panic!()
    };
    *text = "Copy Pix code".to_owned();
    assert_eq!(refused_definition(&wrong), "components[3].buttons[0].text");
    // No card when creating.
    let card = payment_request_definition([Button::payment_request(PaymentRequestSetting::card(
        PaymentRequestCard::new("4242"),
    ))]);
    assert_eq!(
        refused_definition(&card),
        "components[3].buttons[0].payment_setting.offsite_card_pay"
    );
    // Each a different method; three buttons in all.
    let twice = payment_request_definition([
        Button::payment_request(PaymentRequestSetting::boleto(DIGITABLE_LINE)),
        Button::payment_request(PaymentRequestSetting::boleto(DIGITABLE_LINE)),
    ]);
    assert_eq!(
        refused_definition(&twice),
        "components[3].buttons[1].payment_setting.type"
    );
    let four = payment_request_definition([
        Button::payment_request(PaymentRequestSetting::boleto(DIGITABLE_LINE)),
        Button::payment_request(PaymentRequestSetting::pix_dynamic_code(PIX_CODE)),
        Button::payment_request(PaymentRequestSetting::offsite_card_pay()),
        Button::url("Track", "https://shop.example/track"),
    ]);
    assert_eq!(refused_definition(&four), "components[3].buttons");
    let mixed = payment_request_definition([
        Button::payment_request(PaymentRequestSetting::boleto(DIGITABLE_LINE)),
        Button::url("Track", "https://shop.example/track"),
        Button::quick_reply("Stop"),
    ]);
    mixed.validate().unwrap();
    let sample = payment_request_definition([Button::payment_request(
        PaymentRequestSetting::payment_link(""),
    )]);
    assert_eq!(
        refused_definition(&sample),
        "components[3].buttons[0].payment_setting.payment_link.uri"
    );
    // An order details template: UTILITY or MARKETING, a TEXT, IMAGE or
    // DOCUMENT header, a label.
    let order = |category: TemplateCategory, header: TemplateComponent| {
        TemplateDefinition::new("order_bill", "en_US", category)
            .component(header)
            .component(TemplateComponent::body("Review your order and pay."))
            .component(TemplateComponent::buttons([Button::order_details(
                "Review and Pay",
            )]))
    };
    for header in [
        TemplateComponent::header_text("Your order"),
        TemplateComponent::header_image("4::aW1hZ2UvcG5n:ARb"),
        TemplateComponent::header_document("4::YXBwbGljYXRpb24vcGRm:ARb"),
    ] {
        order(TemplateCategory::Marketing, header)
            .validate()
            .unwrap();
    }
    assert_eq!(
        refused_definition(&order(
            TemplateCategory::Utility,
            TemplateComponent::header_media(HeaderFormat::Video, "4::dmlkZW8vbXA0:ARb")
        )),
        "components[2].buttons[0]"
    );
    assert_eq!(
        refused_definition(&order(
            TemplateCategory::FreeService,
            TemplateComponent::header_text("Your order")
        )),
        "components[2].buttons[0]"
    );
    let unlabelled = TemplateDefinition::new("order_bill", "en_US", TemplateCategory::Utility)
        .component(TemplateComponent::body("Review your order and pay."))
        .component(TemplateComponent::buttons([Button::order_details(" ")]));
    assert_eq!(
        refused_definition(&unlabelled),
        "components[1].buttons[0].text"
    );
}

/// What a page does not document still parses (a `GET` of a template Meta
/// extends, a send this client has no rules for), as the catch-all, so the
/// shape survives a round trip and `list()` never fails.
#[test]
fn an_unknown_payment_shape_lands_in_the_catch_all() {
    let button: Button = serde_json::from_value(json!({
        "type": "PAYMENT_REQUEST", "text": "Pay with crypto",
        "payment_setting": {"type": "crypto_wallet"}
    }))
    .unwrap();
    assert!(matches!(button, Button::Other(_)), "{button:?}");
    let parameter: Parameter = serde_json::from_value(json!({
        "type": "action",
        "action": {"order_details": {"currency": "USD", "reference_id": "r"}}
    }))
    .unwrap();
    assert!(matches!(parameter, Parameter::Other(_)), "{parameter:?}");
    assert_eq!(ParameterAction::default().order_details, None);

    // An order reads back only what it would send: `status` `pending`
    // (or none), `type` `quick_pay` (or none). Anything else is refused,
    // not silently rewritten to `pending`.
    let order = |extra: Value| {
        let mut v = json!({"items": [{"name": "Cake", "amount": {"value": 1, "offset": 100},
                                      "quantity": 1}],
                           "subtotal": {"value": 1, "offset": 100},
                           "tax": {"value": 0, "offset": 100}});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value::<Order>(v)
    };
    assert!(!order(json!({})).unwrap().quick_pay);
    assert!(order(json!({"status": "pending"})).is_ok());
    assert!(order(json!({"type": "quick_pay"})).unwrap().quick_pay);
    assert!(order(json!({"status": "processing"})).is_err());
    assert!(order(json!({"type": "slow_pay"})).is_err());

    // In a send, a payment button or order status component that did not
    // parse typed is refused, not sent unchecked.
    let usd: TemplateMessage = serde_json::from_value(json!({
        "name": "order_bill", "language": {"code": "en_US"},
        "components": [{"type": "button", "sub_type": "order_details", "index": "0",
                        "parameters": [{"type": "action", "action": {"order_details": {
                            "currency": "USD", "reference_id": "r"}}}]}]
    }))
    .unwrap();
    assert_eq!(refused(&usd), "template.components[0].parameters");
    let crypto: TemplateMessage = serde_json::from_value(json!({
        "name": "cobranca", "language": {"code": "pt_BR"},
        "components": [{"type": "button", "sub_type": "payment_request", "index": "0",
                        "parameters": [{"type": "action", "action": {"payment_request": {
                            "payment_setting": {"type": "crypto_wallet"}}}}]}]
    }))
    .unwrap();
    assert_eq!(refused(&crypto), "template.components[0].parameters");
    let empty = TemplateMessage::new("order_bill", "en_US").component(SendComponent::Button {
        sub_type: crate::templates::ButtonSubType::OrderDetails,
        index: Some(0),
        parameters: Vec::new(),
    });
    assert_eq!(refused(&empty), "template.components[0].parameters");
    let status: TemplateMessage = serde_json::from_value(json!({
        "name": "order_update", "language": {"code": "en_US"},
        "components": [{"type": "order_status", "parameters": [{"type": "order_status",
            "order_status": {"reference_id": "r"}}]}]
    }))
    .unwrap();
    assert_eq!(refused(&status), "template.components[0].parameters");
}
