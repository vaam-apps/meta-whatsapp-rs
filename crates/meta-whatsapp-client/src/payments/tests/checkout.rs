//! India's checkout button templates (roadmap P1d): every example of
//! `payments-in/checkout-button-templates` (the creation and send bodies,
//! and each checkout endpoint sample request decrypted and answered with
//! its sample response through the Flows endpoint's crypto), the data
//! endpoint link of `payments-in/onboarding-apis`, and the two third party
//! validation payloads of `payments-in/pg`, as exact JSON.
//!
//! The four endpoint requests and their answers repeat one order and
//! differ in a few fields (the coupon, the shipping, the selected address,
//! the total): [`endpoint_order`] writes that order once, with those
//! fields as arguments, copied from the page. The creation example spells
//! its `type`, `format` and `category` values in lower case: they are sent
//! upper case, as every template (P1c's tests do the same), and the lower
//! case example parses into the same definition.

use pretty_assertions::assert_eq;

use super::templates::{block_on, created, string_index, wamid_response};
use super::*;
use crate::templates::{
    Button, CarouselCard, CarouselCardParameters, Parameter, SendComponent, TemplateCategory,
    TemplateComponent, TemplateDefinition, TemplateMessage,
};

/// The send example's recipient.
const SEND_TO: &str = "+16505551234";
/// The page's configuration name.
const CONFIG: &str = "prod-razor-pay-config-05";

/// `type`, `format` and `category` values upper-cased, at any depth.
fn upper_case_enums(mut v: Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(map) => {
                for (k, val) in map.iter_mut() {
                    if matches!(k.as_str(), "type" | "format" | "category")
                        && let Value::String(s) = val
                    {
                        *s = s.to_uppercase();
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

// ─── Creation ───────────────────────────────────────────────────────────

/// "Creating checkout button templates", the example request's body.
fn creation_example() -> Value {
    json!({
      "name": "item_back_in_stock_v1",
      "language": "en_US",
      "category": "marketing",
      "components": [
        {
          "type": "header",
          "format": "image",
          "example": {
            "header_handle": [
              "3:NDU..."
            ]
          }
        },
        {
          "type": "body",
          "text": "Hi {{1}}! The {{2}} is back in stock! Order now before it's gone!",
          "example": {
            "body_text": [
              [
                "Pablo",
                "Blue Elf Aloe"
              ]
            ]
          }
        },
        {
          "type": "footer",
          "text": "Tap 'Stop' below to stop back-in-stock reminders."
        },
        {
          "type": "buttons",
          "buttons": [
            {
              "type": "order_details",
              "text": "Buy now"
            },
            {
              "type": "quick_reply",
              "text": "Stop"
            }
          ]
        }
      ]
    })
}

fn creation_definition() -> TemplateDefinition {
    TemplateDefinition::new(
        "item_back_in_stock_v1",
        "en_US",
        TemplateCategory::Marketing,
    )
    .component(TemplateComponent::header_image("3:NDU..."))
    .component(TemplateComponent::body_positional(
        "Hi {{1}}! The {{2}} is back in stock! Order now before it's gone!",
        ["Pablo", "Blue Elf Aloe"],
    ))
    .component(TemplateComponent::footer(
        "Tap 'Stop' below to stop back-in-stock reminders.",
    ))
    .component(TemplateComponent::buttons([
        Button::checkout(),
        Button::quick_reply("Stop"),
    ]))
}

#[test]
fn checkout_template_creation_example() {
    let definition = creation_definition();
    // The page shows no answer: the template API's.
    let (body, created) = created(
        &definition,
        json!({"id": "594425479261596", "status": "PENDING", "category": "MARKETING"}),
    );
    assert_eq!(body, upper_case_enums(creation_example()));
    assert_eq!(created.id.as_str(), "594425479261596");
    // The page's spelling parses into the same definition, the button
    // typed (not `Button::Other`).
    let parsed: TemplateDefinition = serde_json::from_value(creation_example()).unwrap();
    assert_eq!(parsed, definition);
    // The checkout button's own syntax, as the "Checkout buttons" section
    // prints it.
    assert_eq!(
        upper_case_enums(json!({"type": "order_details", "text": "Buy now"})),
        serde_json::to_value(Button::checkout()).unwrap()
    );
}

#[test]
fn a_checkout_template_is_marketing_with_an_image_or_video_header() {
    // A video header is the other documented format.
    let mut video = creation_definition();
    video.components[0] = TemplateComponent::header_video("4::anBlZw==");
    video.validate().unwrap();
    // Up to 9 quick replies.
    let with_replies = |n: usize| {
        let mut d = creation_definition();
        let mut buttons = vec![Button::checkout()];
        buttons.extend((0..n).map(|i| Button::quick_reply(format!("Reply {i}"))));
        d.components[3] = TemplateComponent::buttons(buttons);
        d
    };
    with_replies(9).validate().unwrap();
    assert_eq!(
        super::templates::refused_definition(&with_replies(10)),
        "components[3].buttons"
    );

    let refused = |d: &TemplateDefinition| d.validate().unwrap_err();
    let mut utility = creation_definition();
    utility.category = TemplateCategory::Utility;
    assert!(
        refused(&utility).reason.contains("MARKETING"),
        "{:?}",
        refused(&utility)
    );

    let mut text_header = creation_definition();
    text_header.components[0] = TemplateComponent::header_text("Back in stock");
    let e = refused(&text_header);
    assert_eq!(e.field, "components");
    assert!(e.reason.contains("IMAGE or VIDEO"), "{e:?}");

    let mut no_header = creation_definition();
    no_header.components.remove(0);
    assert!(refused(&no_header).reason.contains("IMAGE or VIDEO"));

    let mut two = creation_definition();
    two.components[3] = TemplateComponent::buttons([Button::checkout(), Button::checkout()]);
    assert!(refused(&two).reason.contains("one checkout button"));

    let mut url = creation_definition();
    url.components[3] =
        TemplateComponent::buttons([Button::checkout(), Button::url("Shop", "https://x.test")]);
    let e = refused(&url);
    assert_eq!(e.field, "components[3].buttons[1]");
    assert!(e.reason.contains("quick replies"), "{e:?}");

    // An order details button of another label keeps P1c's rules: its
    // video header is refused, as before.
    let mut review = creation_definition();
    review.components[0] = TemplateComponent::header_video("4::anBlZw==");
    review.components[3] = TemplateComponent::buttons([Button::order_details("Review and Pay")]);
    assert!(refused(&review).reason.contains("TEXT, IMAGE or DOCUMENT"));
    // Only the exact label marks a checkout button: another case or
    // spacing is an order details button (decided 2026-11-02, 1).
    for near in ["buy now", "Buy Now", "Buy now ", " Buy now", "Buy  now"] {
        let mut d = review.clone();
        d.components[3] = TemplateComponent::buttons([Button::order_details(near)]);
        assert!(
            refused(&d).reason.contains("TEXT, IMAGE or DOCUMENT"),
            "{near:?}: {:?}",
            refused(&d)
        );
        // Nor is it held to a checkout button's rules: beside a URL button,
        // under an IMAGE header, it is P1c's order details button.
        let mut d = creation_definition();
        d.components[3] = TemplateComponent::buttons([
            Button::order_details(near),
            Button::url("Shop", "https://x.test"),
        ]);
        d.validate().unwrap_or_else(|e| panic!("{near:?}: {e:?}"));
    }
}

/// "Multiple products": a media card carousel whose cards each start with
/// a checkout button.
fn carousel_definition(card_buttons: impl Fn() -> Vec<Button>) -> TemplateDefinition {
    let card = |handle: &str| {
        CarouselCard::new([
            TemplateComponent::header_image(handle),
            TemplateComponent::body("Blue Elf Aloe"),
            TemplateComponent::buttons(card_buttons()),
        ])
    };
    TemplateDefinition::new("plants_back_in_stock", "en_US", TemplateCategory::Marketing)
        .component(TemplateComponent::body("Back in stock!"))
        .component(TemplateComponent::carousel([card("4::a"), card("4::b")]))
}

#[test]
fn a_carousel_card_starts_with_its_checkout_button() {
    carousel_definition(|| vec![Button::checkout(), Button::quick_reply("Stop")])
        .validate()
        .unwrap();
    carousel_definition(|| vec![Button::checkout()])
        .validate()
        .unwrap();

    let second = carousel_definition(|| vec![Button::quick_reply("Stop"), Button::checkout()]);
    let e = second.validate().unwrap_err();
    assert_eq!(e.field, "components[1].cards[0].components[2].buttons[1]");
    assert!(e.reason.contains("first button"), "{e:?}");

    // "Checkout button templates are marketing templates", carousels too
    // (as every carousel).
    let mut utility = carousel_definition(|| vec![Button::checkout()]);
    utility.category = TemplateCategory::Utility;
    let e = utility.validate().unwrap_err();
    assert_eq!(e.field, "components");
    assert!(e.reason.contains("MARKETING"), "{e:?}");

    // A card takes a checkout button, not another order details button,
    // nor one labelled almost as one.
    for label in ["Review and Pay", "buy now", "Buy now "] {
        let review = carousel_definition(|| vec![Button::order_details(label)]);
        assert_eq!(
            review.validate().unwrap_err().field,
            "components[1].cards[0].components[2].buttons[0]",
            "{label:?}"
        );
    }
    // Every card has the same buttons (one with a checkout button and one
    // without differ).
    let mut mixed = carousel_definition(|| vec![Button::checkout()]);
    if let TemplateComponent::Carousel { cards } = &mut mixed.components[1] {
        cards[1].components[2] = TemplateComponent::buttons([Button::quick_reply("Stop")]);
    }
    assert!(
        mixed
            .validate()
            .unwrap_err()
            .reason
            .contains("same components")
    );
}

// ─── Send ───────────────────────────────────────────────────────────────

/// The send example's address, used throughout the page.
fn address() -> ShippingAddress {
    ShippingAddress::new(
        "Nidhi Tripathi",
        "919000090000",
        "Bandra Kurla Complex",
        "Mumbai",
        "Maharastra",
        "400051",
    )
    .house_number("12")
    .tower_number("5")
    .building_name("One BKC")
    .landmark_area("Near BKC Circle")
}

fn address_json() -> Value {
    json!({
      "name": "Nidhi Tripathi",
      "phone_number": "919000090000",
      "address": "Bandra Kurla Complex",
      "city": "Mumbai",
      "state": "Maharastra",
      "in_pin_code": "400051",
      "house_number": "12",
      "tower_number": "5",
      "building_name": "One BKC",
      "landmark_area": "Near BKC Circle"
    })
}

/// The page's item, its sale price counted in the subtotal.
fn item() -> OrderItem {
    OrderItem::new("Blue Elf Aloe", Amount::inr(200_000), 1)
        .sale_amount(Amount::inr(150_000))
        .import(
            "India",
            "Lucky Shrub Imports and Exports",
            ImporterAddress::new("One BKC", "Mumbai", "400051", "IN")
                .address_line2("Bandra Kurla Complex")
                .zone_code("MH"),
        )
}

/// The page's order, with `shipping` and `expiration` as given.
fn order(shipping: u64, expiration: OrderExpiration) -> Order {
    Order::new(
        [item()],
        Amount::inr(150_000),
        DescribedAmount::new(Amount::inr(10_000)),
    )
    .shipping(DescribedAmount::new(Amount::inr(shipping)))
    .discount(Discount::new(Amount::inr(15_000)).description("Additional 10% off"))
    .expiration(expiration)
}

fn send_parameters() -> OrderDetailsParameters {
    let gateway = PaymentGateway::new(GatewayProvider::Razorpay(RazorpayFields::default()), CONFIG);
    OrderDetailsParameters::inr(
        "abc.123_xyz-1",
        GoodsType::Physical,
        gateway,
        order(20_000, OrderExpiration::at(1_726_627_150)),
    )
    .shipping_info(ShippingInfo::india([address()]))
}

fn send_template() -> TemplateMessage {
    TemplateMessage::new("item_back_in_stock_v2", "en_US")
        .deterministic()
        .header(Parameter::image_id(
            MediaId::new("1558081531584829").unwrap(),
        ))
        .body([Parameter::text("Nidhi"), Parameter::text("Blue Elf Aloe")])
        .order_details_button(0, send_parameters())
}

/// "Send a checkout button template", the example request's body.
#[allow(clippy::too_many_lines)] // the page's example, copied
fn send_example() -> Value {
    // The button's order apart: the whole is too deep for one `json!`.
    let order_details = json!({
      "reference_id": "abc.123_xyz-1",
      "type": "physical-goods",
      "currency": "INR",
      "payment_settings": [
        {
          "type": "payment_gateway",
          "payment_gateway": {
            "type": "razorpay",
            "configuration_name": "prod-razor-pay-config-05"
          }
        }
      ],
      "shipping_info": {
        "country": "IN",
        "addresses": [
          {
            "name": "Nidhi Tripathi",
            "phone_number": "919000090000",
            "address": "Bandra Kurla Complex",
            "city": "Mumbai",
            "state": "Maharastra",
            "in_pin_code": "400051",
            "house_number": "12",
            "tower_number": "5",
            "building_name": "One BKC",
            "landmark_area": "Near BKC Circle"
          }
        ]
      },
      "order": {
        "items": [
          {
            "amount": {
              "offset": 100,
              "value": 200000
            },
            "sale_amount": {
              "offset": 100,
              "value": 150000
            },
            "name": "Blue Elf Aloe",
            "quantity": 1,
            "country_of_origin": "India",
            "importer_name": "Lucky Shrub Imports and Exports",
            "importer_address": {
              "address_line1": "One BKC",
              "address_line2": "Bandra Kurla Complex",
              "city": "Mumbai",
              "zone_code": "MH",
              "postal_code": "400051",
              "country_code": "IN"
            }
          }
        ],
        "subtotal": {
          "offset": 100,
          "value": 150000
        },
        "shipping": {
          "offset": 100,
          "value": 20000
        },
        "tax": {
          "offset": 100,
          "value": 10000
        },
        "discount": {
          "offset": 100,
          "value": 15000,
          "description": "Additional 10% off"
        },
        "status": "pending",
        "expiration": {
          "timestamp": "1726627150"
        }
      },
      "total_amount": {
        "offset": 100,
        "value": 165000
      }
    });
    json!({
      "messaging_product": "whatsapp",
      "recipient_type": "individual",
      "to": "+16505551234",
      "type": "template",
      "template": {
        "name": "item_back_in_stock_v2",
        "language": {
          "policy": "deterministic",
          "code": "en_US"
        },
        "components": [
          {
            "type": "header",
            "parameters": [
              {
                "type": "image",
                "image": {
                  "id": "1558081531584829"
                }
              }
            ]
          },
          {
            "type": "body",
            "parameters": [
              {
                "type": "text",
                "text": "Nidhi"
              },
              {
                "type": "text",
                "text": "Blue Elf Aloe"
              }
            ]
          },
          {
            "type": "button",
            "sub_type": "order_details",
            "index": 0,
            "parameters": [
              {
                "type": "action",
                "action": {
                  "order_details": order_details
                }
              }
            ]
          }
        ]
      }
    })
}

/// Send `template` to [`SEND_TO`] against the order details pages' answer
/// and return the body, after checking the request.
#[track_caller]
fn sent_to_example_recipient(template: TemplateMessage) -> Value {
    let message = OutboundMessage::new(Recipient::phone(SEND_TO), template);
    if let Err(e) = message.validate() {
        panic!("documented example failed validation: {e}");
    }
    let t = ScriptedTransport::new();
    t.push_json(200, wamid_response());
    block_on(
        client(&t)
            .messages(PhoneNumberId::new(PHONE_ID).unwrap())
            .send(&message),
    )
    .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), format!("/v25.0/{PHONE_ID}/messages"));
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(t.remaining(), 0);
    req.json().unwrap()
}

#[test]
fn checkout_template_send_example() {
    let built = send_template();
    let body = sent_to_example_recipient(built.clone());
    assert_eq!(body, string_index(send_example()));
    // The page's body parses back into the same message (the button index
    // as a number included), and passes the same checks.
    let parsed: TemplateMessage =
        serde_json::from_value(send_example()["template"].clone()).unwrap();
    assert_eq!(parsed, built);
    parsed.validate().unwrap();
    assert_eq!(
        built.components[2],
        SendComponent::order_details_button(0, send_parameters())
    );
}

#[test]
fn a_checkout_carousel_sends_one_order_per_card_each_its_own_reference() {
    let card = |i: u32, reference: &str| {
        let mut params = send_parameters();
        params.reference_id = reference.to_owned();
        CarouselCardParameters::new(
            i,
            [
                SendComponent::header(Parameter::image_id(
                    MediaId::new("1558081531584829").unwrap(),
                )),
                SendComponent::order_details_button(0, params),
            ],
        )
    };
    let carousel = |second: &str| {
        TemplateMessage::new("plants_back_in_stock", "en_US")
            .carousel([card(0, "abc.123_xyz-1"), card(1, second)])
    };
    carousel("abc.123_xyz-2").validate().unwrap();
    let e = carousel("abc.123_xyz-1").validate().unwrap_err();
    assert_eq!(
        e.field,
        "template.components[0].cards[1].components[1].parameters[0].action.order_details.reference_id"
    );
    // Two order details buttons of one message, outside a carousel too.
    let twice = send_template().order_details_button(1, send_parameters());
    assert!(twice.validate().is_err());
}

#[test]
fn shipping_info_is_a_template_buttons_and_physical_goods_only() {
    // Refused in an `order_details` message, which documents none.
    let message = OrderDetails::new("Your order", send_parameters());
    assert_eq!(
        message.validate().unwrap_err().field,
        "interactive.action.parameters.shipping_info"
    );
    // "required for physical-goods type, else omit".
    let mut digital = send_parameters();
    digital.goods_type = GoodsType::Digital;
    let template =
        |p: OrderDetailsParameters| TemplateMessage::new("t", "en_US").order_details_button(0, p);
    let at = "template.components[0].parameters[0].action.order_details";
    assert_eq!(
        template(digital).validate().unwrap_err().field,
        format!("{at}.shipping_info")
    );
    // The page's address, every field of it.
    assert_eq!(
        serde_json::to_value(ShippingInfo::india([address()])).unwrap(),
        json!({"country": "IN", "addresses": [address_json()]})
    );
    // No known address: an empty array, sent as such.
    let mut none_known = send_parameters();
    none_known.shipping_info = Some(ShippingInfo::india([]));
    template(none_known.clone()).validate().unwrap();
    assert_eq!(
        serde_json::to_value(&none_known).unwrap()["shipping_info"],
        json!({"country": "IN", "addresses": []})
    );
    // The selected address and the coupon are the endpoint's, not a send's.
    let mut selected = send_parameters();
    selected.shipping_info = Some(ShippingInfo::india([address()]).selected_address(address()));
    assert_eq!(
        template(selected).validate().unwrap_err().field,
        format!("{at}.shipping_info.selected_address")
    );
    let couponed = send_parameters().coupon(AppliedCoupon::new("TRYNEW10", Amount::inr(16_500)));
    assert_eq!(
        template(couponed).validate().unwrap_err().field,
        format!("{at}.coupon")
    );
    // India's only: a BRL order of physical goods that passes without it
    // is refused with it.
    let brl = OrderDetailsParameters::brl_simplified(
        "pedido-1",
        GoodsType::Physical,
        PaymentSetting::payment_link("https://my-payment-link-url"),
        Amount::brl(50_000),
    );
    template(brl.clone()).validate().unwrap();
    assert_eq!(
        template(brl.shipping_info(ShippingInfo::india([address()])))
            .validate()
            .unwrap_err()
            .field,
        format!("{at}.shipping_info")
    );
    // It names its country.
    let mut no_country = send_parameters();
    no_country.shipping_info = Some(ShippingInfo::new("", [address()]));
    assert_eq!(
        template(no_country).validate().unwrap_err().field,
        format!("{at}.shipping_info.country")
    );
}

#[test]
fn every_address_limit_is_the_pages() {
    type Set<'a> = &'a dyn Fn(&mut ShippingAddress, String);
    let at = "template.components[0].parameters[0].action.order_details.shipping_info.addresses[0]";
    let with = |f: &dyn Fn(&mut ShippingAddress)| {
        let mut a = address();
        f(&mut a);
        let mut p = send_parameters();
        p.shipping_info = Some(ShippingInfo::india([a]));
        TemplateMessage::new("t", "en_US").order_details_button(0, p)
    };
    let chars = |n: usize| "é".repeat(n);
    let limits: [(&str, usize, Set<'_>); 10] = [
        ("name", 256, &|a, v| a.name = v),
        ("phone_number", 12, &|a, v| a.phone_number = v),
        ("address", 512, &|a, v| a.address = v),
        ("city", 100, &|a, v| a.city = v),
        ("state", 100, &|a, v| a.state = v),
        ("in_pin_code", 6, &|a, v| a.in_pin_code = v),
        ("house_number", 8, &|a, v| a.house_number = Some(v)),
        ("tower_number", 8, &|a, v| a.tower_number = Some(v)),
        ("building_name", 128, &|a, v| a.building_name = Some(v)),
        ("landmark_area", 128, &|a, v| a.landmark_area = Some(v)),
    ];
    for (name, max, set) in limits {
        with(&|a| set(a, chars(max))).validate().unwrap();
        assert_eq!(
            with(&|a| set(a, chars(max + 1)))
                .validate()
                .unwrap_err()
                .field,
            format!("{at}.{name}"),
            "{name}"
        );
        assert_eq!(
            with(&|a| set(a, String::new()))
                .validate()
                .unwrap_err()
                .field,
            format!("{at}.{name}"),
            "{name} empty"
        );
    }
}

#[test]
fn an_expiration_needs_its_description_in_a_message_only() {
    let params = |expiration| {
        let gateway =
            PaymentGateway::new(GatewayProvider::Razorpay(RazorpayFields::default()), CONFIG);
        OrderDetailsParameters::inr(
            "abc.123_xyz-1",
            GoodsType::Physical,
            gateway,
            order(20_000, expiration),
        )
    };
    let bare = OrderExpiration::at(1_726_627_150);
    assert_eq!(
        OrderDetails::new("Your order", params(bare.clone()))
            .validate()
            .unwrap_err()
            .field,
        "interactive.action.parameters.order.expiration.description"
    );
    OrderDetails::new(
        "Your order",
        params(OrderExpiration::new(
            1_726_627_150,
            "order expires in 5 min",
        )),
    )
    .validate()
    .unwrap();
    TemplateMessage::new("t", "en_US")
        .order_details_button(0, params(bare))
        .validate()
        .unwrap();
}

// ─── Third party validation ─────────────────────────────────────────────

/// `payments-in/pg`, "Updates to order details payload to support TPV",
/// for `gateway` (`razorpay` or `payu`).
fn tpv_example(gateway: &str) -> Value {
    json!({
      "messaging_product": "whatsapp",
      "interactive": {
        "action": {
          "name": "review_and_pay",
          "parameters": {
            "payment_settings": [
              {
                "type": gateway,
                gateway: {
                  "encrypted_payment_gateway_data": "encrypted-data"
                }
              }
            ],
            "order": {}
          }
        }
      }
    })
}

#[test]
fn the_two_third_party_validation_payloads() {
    for (gateway, name) in [
        (TpvGateway::Razorpay, "razorpay"),
        (TpvGateway::Payu, "payu"),
    ] {
        let doc = tpv_example(name);
        let setting = PaymentSetting::third_party_validation(
            gateway,
            EncryptedPaymentGatewayData::new("encrypted-data"),
        );
        let settings = &doc["interactive"]["action"]["parameters"]["payment_settings"];
        assert_eq!(
            serde_json::to_value([&setting]).unwrap(),
            *settings,
            "{name}"
        );
        let parsed: Vec<PaymentSetting> = serde_json::from_value(settings.clone()).unwrap();
        assert_eq!(parsed, std::slice::from_ref(&setting), "{name}");

        // Sent in an order, it is the page's update: the same envelope,
        // action and settings (the page elides the order as `{}`).
        let order = Order::new(
            [item()],
            Amount::inr(150_000),
            DescribedAmount::new(Amount::inr(10_000)),
        );
        let params =
            OrderDetailsParameters::inr("abc.123_xyz-1", GoodsType::Digital, setting, order);
        let body = sent(OrderDetails::new("Your order", params));
        assert_eq!(body["messaging_product"], doc["messaging_product"]);
        assert_eq!(
            body["interactive"]["action"]["name"],
            doc["interactive"]["action"]["name"]
        );
        assert_eq!(
            body["interactive"]["action"]["parameters"]["payment_settings"], *settings,
            "{name}"
        );
    }
}

#[test]
fn third_party_validation_data_is_a_secret_in_inr_only() {
    let secret = "ciphertext-of-the-buyers-accounts";
    let setting = PaymentSetting::third_party_validation(
        TpvGateway::Payu,
        EncryptedPaymentGatewayData::new(secret),
    );
    let order = Order::new(
        [item()],
        Amount::inr(150_000),
        DescribedAmount::new(Amount::inr(10_000)),
    );
    let message = OrderDetails::new(
        "Your order",
        OrderDetailsParameters::inr("abc.123_xyz-1", GoodsType::Digital, setting.clone(), order),
    );
    message.validate().unwrap();
    // In a template's order details button too, and in the error a send
    // refused for its order reports.
    let mut button_order = message.parameters.clone();
    button_order.goods_type = GoodsType::Physical;
    button_order.shipping_info = Some(ShippingInfo::india([]));
    let template = TemplateMessage::new("t", "en_US").order_details_button(0, button_order.clone());
    template.validate().unwrap();
    button_order.total_amount = Amount::inr(1);
    let refused = TemplateMessage::new("t", "en_US")
        .order_details_button(0, button_order)
        .validate()
        .unwrap_err();
    for shown in [
        format!("{setting:?}"),
        format!("{message:?}"),
        format!("{template:?}"),
    ] {
        assert!(!shown.contains(secret), "{shown}");
        assert!(shown.contains("[REDACTED]"), "{shown}");
    }
    assert_eq!(
        refused.field,
        "template.components[0].parameters[0].action.order_details.total_amount.value"
    );
    let shown = format!("{refused:?} {refused}");
    assert!(!shown.contains(secret), "{shown}");
    // Serialized, it is what the send carries.
    assert_eq!(
        serde_json::to_value(&setting).unwrap()["payu"]["encrypted_payment_gateway_data"],
        secret
    );

    let empty = PaymentSetting::third_party_validation(
        TpvGateway::Razorpay,
        EncryptedPaymentGatewayData::new(""),
    );
    let mut m = message.clone();
    m.parameters.payment_settings = vec![empty];
    assert_eq!(
        m.validate().unwrap_err().field,
        "interactive.action.parameters.payment_settings[0].razorpay.encrypted_payment_gateway_data"
    );
    // India's page only: refused in a BRL order.
    let brl = OrderDetailsParameters::brl_simplified(
        "pedido-1",
        GoodsType::Digital,
        setting,
        Amount::brl(100),
    );
    assert_eq!(
        OrderDetails::new("Seu pedido", brl)
            .validate()
            .unwrap_err()
            .field,
        "interactive.action.parameters.payment_settings[0].type"
    );
    // A setting `type` without its object is refused, as every setting's.
    assert!(serde_json::from_value::<PaymentSetting>(json!({"type": "payu"})).is_err());
}

// ─── Link the data endpoint ─────────────────────────────────────────────

/// "Link or update data endpoint": the example request. The page shows no
/// answer; `{"success": true}`, Graph's usual, and an empty object both
/// pass.
#[tokio::test]
async fn link_data_endpoint_example() {
    for answer in [json!({"success": true}), json!({})] {
        let t = ScriptedTransport::new();
        t.push_json(200, answer);
        configurations(&t)
            .link_data_endpoint(
                "test-payment-configuration",
                "https://test-data-endpoint-url.com",
            )
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            format!("/v25.0/{WABA}/payment_configuration/test-payment-configuration")
        );
        assert_eq!(req.query("data_endpoint_url"), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({"data_endpoint_url": "https://test-data-endpoint-url.com"}))
        );
        assert_eq!(t.remaining(), 0);
    }
}

#[tokio::test]
async fn link_data_endpoint_checks_first_and_reads_a_refusal() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": false}));
    let err = configurations(&t)
        .link_data_endpoint("c", "https://shop.example/checkout")
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Http { status: 200, .. }), "{err:?}");

    t.push_json(
        400,
        json!({"error": {"message": "(#131005) Access denied", "type": "OAuthException", "code": 131005}}),
    );
    let err = configurations(&t)
        .link_data_endpoint("c", "https://shop.example/checkout")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Permission);
    assert_eq!(t.requests().len(), 2);

    for (name, url) in [
        ("", "https://shop.example/checkout"),
        ("c", "http://shop.example/checkout"),
        ("c", "shop.example/checkout"),
        ("c", "https://user:pw@shop.example/checkout"),
        ("c", "https://shop.example/checkout#x"),
        ("c", "https://shop.example/check out"),
        ("c", " https://shop.example/checkout"),
        // What the WHATWG parser repairs into `https://shop.example/…`,
        // but the string sent would still say: refused, not repaired.
        ("c", "https:shop.example/checkout"),
        ("c", "https:/shop.example/checkout"),
        ("c", "https:///shop.example/checkout"),
        ("c", "https:\\\\shop.example\\checkout"),
        ("c", "https://shop.example\\checkout"),
        // User info, even empty or percent-encoded.
        ("c", "https://@shop.example/checkout"),
        ("c", "https://:@shop.example/checkout"),
        ("c", "https://user%40x@shop.example/checkout"),
        ("c", "https://shop.example@evil.example/checkout"),
        // No host.
        ("c", "https://"),
        ("c", "https://?x=1"),
        ("c", "https://:8443/checkout"),
        ("c", "httpsx://shop.example/checkout"),
    ] {
        let err = configurations(&t)
            .link_data_endpoint(name, url)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{name:?} {url:?}");
    }
    assert_eq!(t.requests().len(), 2, "nothing sent for a refused input");

    // The name is one path segment.
    t.push_json(200, json!({"success": true}));
    configurations(&t)
        .link_data_endpoint("a/b", "https://shop.example/checkout")
        .await
        .unwrap();
    assert_eq!(
        t.last_request().unwrap().path(),
        format!("/v25.0/{WABA}/payment_configuration/a%2Fb")
    );
}

/// The host is the deployment's to judge: an upper case scheme, an IP
/// literal (a private one too), a port, an IDN, an `@` past the host,
/// all pass and are sent exactly as given.
#[tokio::test]
async fn link_data_endpoint_takes_any_https_host_as_given() {
    for url in [
        "HTTPS://shop.example/checkout",
        "Https://shop.example:8443/checkout",
        "https://203.0.113.7/checkout",
        "https://10.0.0.5:8443/checkout",
        "https://[2001:db8::1]/checkout",
        "https://bücher.example/checkout",
        "https://xn--bcher-kva.example/checkout",
        "https://shop.example/a@b?next=@c",
        "https://shop.example",
    ] {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        configurations(&t)
            .link_data_endpoint("c", url)
            .await
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        assert_eq!(
            t.last_request().unwrap().json(),
            Some(json!({"data_endpoint_url": url}))
        );
        assert_eq!(t.remaining(), 0);
    }
}

/// Setting a URL is replayed after a timeout: a replay sets it again.
#[tokio::test]
async fn link_data_endpoint_is_replayed() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    t.push_json(200, json!({"success": true}));
    retrying_client(&t)
        .payment_configurations(WabaId::new(WABA).unwrap())
        .link_data_endpoint("c", "https://shop.example/checkout")
        .await
        .unwrap();
    assert_eq!(t.requests().len(), 2);
    assert_eq!(t.remaining(), 0);
}

#[cfg(feature = "flows-endpoint")]
mod endpoint;
