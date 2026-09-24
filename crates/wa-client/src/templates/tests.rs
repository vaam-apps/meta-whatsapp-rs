//! Request/response tests against the examples on Meta's pages. Example
//! payloads are copied verbatim; where a page prints the same enum in
//! lower case or a button index as a number, the comparison normalizes it
//! (see `normalize`) and says so.

use futures::StreamExt;
use http::Method;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wa_core::ErrorKind;
use wa_core::ids::TemplateId;
use wa_core::testing::ScriptedTransport;

use super::*;
use crate::RetryPolicy;

fn client(t: &ScriptedTransport) -> Client {
    Client::builder()
        .transport(t.clone())
        .access_token("TOKEN")
        .retry(RetryPolicy::NONE)
        .build()
        .unwrap()
}

/// Definition enums appear in either case on Meta's pages (Meta accepts
/// both and returns upper case); compare them upper-cased.
fn normalize(mut v: Value) -> Value {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(map) => {
                for (k, val) in map.iter_mut() {
                    match (k.as_str(), &mut *val) {
                        (
                            "type" | "format" | "category" | "parameter_format" | "otp_type",
                            Value::String(s),
                        ) => {
                            *s = s.to_uppercase();
                        }
                        _ => walk(val),
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

/// Send examples write `index` as a number or a string; we send strings.
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

/// Our definition serializes to the page's example, parses back from it,
/// and passes local validation.
fn assert_definition(def: &TemplateDefinition, doc: Value) {
    assert_eq!(
        normalize(serde_json::to_value(def).unwrap()),
        normalize(doc.clone())
    );
    let parsed: TemplateDefinition = serde_json::from_value(doc).unwrap();
    assert_eq!(&parsed, def, "the docs' casing parses into the same value");
    def.validate().unwrap();
}

/// Our invocation serializes to the page's `template` object and parses
/// back from it.
fn assert_invocation(message: &TemplateMessage, doc_send_body: &Value) {
    let doc = doc_send_body["template"].clone();
    assert_eq!(
        serde_json::to_value(message).unwrap(),
        string_index(doc.clone())
    );
    let parsed: TemplateMessage = serde_json::from_value(doc).unwrap();
    assert_eq!(&parsed, message);
    message.validate().unwrap();
}

// ---------------------------------------------------------------- contract

#[test]
fn minimal_shape_is_stable() {
    let t = TemplateMessage::new("hello_world", "en_US");
    assert_eq!(
        serde_json::to_value(&t).unwrap(),
        serde_json::json!({"name": "hello_world", "language": {"code": "en_US"}})
    );
}

#[test]
fn contract_fields_and_round_trip() {
    let t = TemplateMessage::new("hello_world", "en_US");
    assert_eq!(t.name, "hello_world");
    assert_eq!(t.language.code, "en_US");
    assert_eq!(t.language.policy, None);
    assert!(t.components.is_empty());
    let back: TemplateMessage =
        serde_json::from_value(json!({"name": "hello_world", "language": {"code": "en_US"}}))
            .unwrap();
    assert_eq!(back, t);
}

// ------------------------------------------------------------ definitions

#[test]
fn marketing_template_with_image_header_and_url_button() {
    // templates/marketing-templates/custom-marketing-templates, example request.
    let doc = json!({
      "name": "welcome_discount_template",
      "language": "en_US",
      "category": "marketing",
      "parameter_format": "named",
      "components": [
        {"type": "header", "format": "image", "example": {"header_handle": ["4::aW..."]}},
        {
          "type": "body",
          "text": "Welcome to Lucky Shrub, {{first_name}}!\n\nUse code *{{discount_code}}* to get {{discount_amount}} off of your first purchase!",
          "example": {
            "body_text_named_params": [
              {"param_name": "first_name", "example": "Pablo"},
              {"param_name": "discount_code", "example": "WELCOME20"},
              {"param_name": "discount_amount", "example": "20%"}
            ]
          }
        },
        {"type": "footer", "text": "Lucky Shrub: Your gateway to succulents!"},
        {
          "type": "buttons",
          "buttons": [
            {"type": "url", "text": "View deals", "url": "https://www.luckyshrub.com/deals"},
            {"type": "phone_number", "text": "Call us", "phone_number": "+15550051310"},
            {"type": "quick_reply", "text": "Unsubscribe"}
          ]
        }
      ]
    });
    let def = TemplateDefinition::new("welcome_discount_template", "en_US", TemplateCategory::Marketing)
        .parameter_format(ParameterFormat::Named)
        .component(TemplateComponent::header_image("4::aW..."))
        .component(TemplateComponent::body_named(
            "Welcome to Lucky Shrub, {{first_name}}!\n\nUse code *{{discount_code}}* to get {{discount_amount}} off of your first purchase!",
            [
                ("first_name", "Pablo"),
                ("discount_code", "WELCOME20"),
                ("discount_amount", "20%"),
            ],
        ))
        .component(TemplateComponent::footer("Lucky Shrub: Your gateway to succulents!"))
        .component(TemplateComponent::buttons([
            Button::url("View deals", "https://www.luckyshrub.com/deals"),
            Button::phone_number("Call us", "+15550051310"),
            Button::quick_reply("Unsubscribe"),
        ]));
    assert_definition(&def, doc);
}

#[test]
fn named_params_utility_template() {
    // templates/overview#named-parameters, creation payload.
    let doc = json!({
      "name": "order_confirmation",
      "language": "en_US",
      "category": "utility",
      "parameter_format": "named",
      "components": [
        {
          "type": "body",
          "text": "Thank you, {{first_name}}! Your order number is {{order_number}}.",
          "example": {
            "body_text_named_params": [
              {"param_name": "first_name", "example": "Pablo"},
              {"param_name": "order_number", "example": "860198-230332"}
            ]
          }
        }
      ]
    });
    let def = TemplateDefinition::new("order_confirmation", "en_US", TemplateCategory::Utility)
        .parameter_format(ParameterFormat::Named)
        .component(TemplateComponent::body_named(
            "Thank you, {{first_name}}! Your order number is {{order_number}}.",
            [("first_name", "Pablo"), ("order_number", "860198-230332")],
        ));
    assert_definition(&def, doc);
}

#[test]
fn positional_template_with_text_header() {
    // templates/components, "Seasonal promotion".
    let doc = json!({
      "name": "seasonal_promotion",
      "language": "en_US",
      "category": "MARKETING",
      "components": [
        {"type": "HEADER", "format": "TEXT", "text": "Our {{1}} is on!", "example": {"header_text": ["Summer Sale"]}},
        {
          "type": "BODY",
          "text": "Shop now through {{1}} and use code {{2}} to get {{3}} off of all merchandise.",
          "example": {"body_text": [["the end of August","25OFF","25%"]]}
        },
        {"type": "FOOTER", "text": "Use the buttons below to manage your marketing subscriptions"},
        {"type":"BUTTONS", "buttons": [
          {"type": "QUICK_REPLY", "text": "Unsubscribe from Promos"},
          {"type":"QUICK_REPLY", "text": "Unsubscribe from All"}
        ]}
      ]
    });
    let def = TemplateDefinition::new("seasonal_promotion", "en_US", TemplateCategory::Marketing)
        .component(TemplateComponent::header_text_positional(
            "Our {{1}} is on!",
            "Summer Sale",
        ))
        .component(TemplateComponent::body_positional(
            "Shop now through {{1}} and use code {{2}} to get {{3}} off of all merchandise.",
            ["the end of August", "25OFF", "25%"],
        ))
        .component(TemplateComponent::footer(
            "Use the buttons below to manage your marketing subscriptions",
        ))
        .component(TemplateComponent::buttons([
            Button::quick_reply("Unsubscribe from Promos"),
            Button::quick_reply("Unsubscribe from All"),
        ]));
    assert_definition(&def, doc);
}

#[test]
fn location_header_utility_template() {
    // templates/utility-templates/location-templates, example request.
    let doc = json!({
        "name": "order_delivery_update",
        "language": "en_US",
        "category": "UTILITY",
        "parameter_format": "named",
        "components": [
          {"type": "HEADER", "format": "LOCATION"},
          {
            "type": "BODY",
            "text": "Good news {{customer_name}}! Your order #{{order_number}} is on its way to the location above. Thank you for your order!",
            "example": {"body_text_named_params": [
              {"param_name": "customer_name", "example": "Mark"},
              {"param_name": "order_number", "example": "566701"}
            ]}
          },
          {"type": "FOOTER", "text": "To stop receiving delivery updates, tap the button below."},
          {"type": "BUTTONS", "buttons": [{"type": "QUICK_REPLY", "text": "Stop Delivery Updates"}]}
        ]
    });
    let def = TemplateDefinition::new("order_delivery_update", "en_US", TemplateCategory::Utility)
        .parameter_format(ParameterFormat::Named)
        .component(TemplateComponent::header_location())
        .component(TemplateComponent::body_named(
            "Good news {{customer_name}}! Your order #{{order_number}} is on its way to the location above. Thank you for your order!",
            [("customer_name", "Mark"), ("order_number", "566701")],
        ))
        .component(TemplateComponent::footer(
            "To stop receiving delivery updates, tap the button below.",
        ))
        .component(TemplateComponent::buttons([Button::quick_reply(
            "Stop Delivery Updates",
        )]));
    assert_definition(&def, doc);
}

#[test]
fn media_card_carousel_template() {
    // templates/marketing-templates/media-card-carousel-templates, example request.
    let card = |example: &str, url: &str| {
        json!({"components": [
            {"type": "header", "format": "image", "example": {"header_handle": ["4::an..."]}},
            {"type": "buttons", "buttons": [
                {"type": "quick_reply", "text": "Send me more like this!"},
                {"type": "url", "text": "Shop", "url": url, "example": [example]}
            ]}
        ]})
    };
    let doc = json!({
      "name": "carousel_template_media_cards_v1",
      "language": "en_US",
      "category": "marketing",
      "components": [
        {
          "type": "body",
          "text": "Rare succulents for sale! {{1}}, add these unique plants to your collection. Each of these rare succulents are {{2}} if you checkout using code {{3}}. Shop now and add some unique and beautiful plants to your collection!",
          "example": {"body_text": [["Pablo", "30%", "30OFF"]]}
        },
        {"type": "carousel", "cards": [
          card("BLUE_ELF", "https://www.luckyshrub.com/rare-succulents/{{1}}"),
          card("BUDDHA", "https://www.luckyshrub.com/rare-succulents{{1}}"),
          card("BLACK_PRINCE", "https://www.luckyshrub.com/rare-succulents{{1}}")
        ]}
      ]
    });
    let card = |example: &str, url: &str| {
        CarouselCard::new([
            TemplateComponent::header_image("4::an..."),
            TemplateComponent::buttons([
                Button::quick_reply("Send me more like this!"),
                Button::url_with_example("Shop", url, example),
            ]),
        ])
    };
    let def = TemplateDefinition::new("carousel_template_media_cards_v1", "en_US", TemplateCategory::Marketing)
        .component(TemplateComponent::body_positional(
            "Rare succulents for sale! {{1}}, add these unique plants to your collection. Each of these rare succulents are {{2}} if you checkout using code {{3}}. Shop now and add some unique and beautiful plants to your collection!",
            ["Pablo", "30%", "30OFF"],
        ))
        .component(TemplateComponent::carousel([
            card("BLUE_ELF", "https://www.luckyshrub.com/rare-succulents/{{1}}"),
            card("BUDDHA", "https://www.luckyshrub.com/rare-succulents{{1}}"),
            card("BLACK_PRINCE", "https://www.luckyshrub.com/rare-succulents{{1}}"),
        ]));
    assert_definition(&def, doc);
}

#[test]
fn product_card_carousel_template() {
    // catalogs/product-card-carousel-template-messages, example request. It
    // writes the single example as `"body_text": "Pablo"`; we accept that and
    // send the equivalent `[["Pablo"]]` (the page allows both).
    let card = json!({"components": [
        {"type": "header", "format": "product"},
        {"type": "buttons", "buttons": [{"type": "spm", "text": "View"}]}
    ]});
    let text = "Rare succulents for sale! {{1}}, add these unique plants to your collection. All three of these rare succulents are available for purchase on our website, and they come with a 100% satisfaction guarantee. Whether you're a seasoned succulent enthusiast or just starting your plant collection, these rare succulents are sure to impress. Shop now and add some unique and beautiful plants to your collection!";
    let doc = json!({
      "name": "carousel_template_product_cards_v1",
      "language": "en_US",
      "category": "marketing",
      "components": [
        {"type": "body", "text": text, "example": {"body_text": "Pablo"}},
        {"type": "carousel", "cards": [card, card]}
      ]
    });
    let card = || {
        CarouselCard::new([
            TemplateComponent::header_product(),
            TemplateComponent::buttons([Button::spm("View")]),
        ])
    };
    let def = TemplateDefinition::new(
        "carousel_template_product_cards_v1",
        "en_US",
        TemplateCategory::Marketing,
    )
    .component(TemplateComponent::body_positional(text, ["Pablo"]))
    .component(TemplateComponent::carousel([card(), card()]));
    let parsed: TemplateDefinition = serde_json::from_value(doc.clone()).unwrap();
    assert_eq!(parsed, def);
    let mut expected = doc;
    expected["components"][0]["example"]["body_text"] = json!([["Pablo"]]);
    assert_eq!(
        normalize(serde_json::to_value(&def).unwrap()),
        normalize(expected)
    );
    def.validate().unwrap();
}

#[test]
fn limited_time_offer_template() {
    // templates/marketing-templates/limited-time-offer-templates, example request.
    let doc = json!({
      "name": "limited_time_offer_caribbean_pkg_2023",
      "language": "en_US",
      "category": "marketing",
      "components": [
        {"type": "header", "format": "image", "example": {"header_handle": ["4::aW..."]}},
        {"type": "limited_time_offer", "limited_time_offer": {"text": "Expiring offer!", "has_expiration": true}},
        {
          "type": "body",
          "text": "Good news, {{1}}! Use code {{2}} to get 25% off all Caribbean Destination packages!",
          "example": {"body_text": [["Pablo", "CARIBE25"]]}
        },
        {"type": "buttons", "buttons": [
          {"type": "copy_code", "example": "CARIBE25"},
          {"type": "url", "text": "Book now!", "url": "https://awesomedestinations.com/offers?code={{1}}",
           "example": ["https://awesomedestinations.com/offers?ref=n3mtql"]}
        ]}
      ]
    });
    let def = TemplateDefinition::new(
        "limited_time_offer_caribbean_pkg_2023",
        "en_US",
        TemplateCategory::Marketing,
    )
    .component(TemplateComponent::header_image("4::aW..."))
    .component(TemplateComponent::limited_time_offer(
        "Expiring offer!",
        Some(true),
    ))
    .component(TemplateComponent::body_positional(
        "Good news, {{1}}! Use code {{2}} to get 25% off all Caribbean Destination packages!",
        ["Pablo", "CARIBE25"],
    ))
    .component(TemplateComponent::buttons([
        Button::copy_code("CARIBE25"),
        Button::url_with_example(
            "Book now!",
            "https://awesomedestinations.com/offers?code={{1}}",
            "https://awesomedestinations.com/offers?ref=n3mtql",
        ),
    ]));
    assert_definition(&def, doc);
}

#[test]
fn coupon_template() {
    // templates/marketing-templates/coupon-templates, example request.
    let doc = json!({
      "name": "winter_sale_coupon",
      "language": "en_US",
      "category": "MARKETING",
      "parameter_format": "named",
      "components": [
        {"type": "HEADER", "format": "TEXT", "text": "Our Winter Sale is on!"},
        {
          "type": "BODY",
          "text": "Shop now through the end of December and use the one-time use code {{coupon_code}} to get {{discount}} off of your entire order!",
          "example": {"body_text_named_params": [
            {"param_name": "coupon_code", "example": "WINTER25"},
            {"param_name": "discount", "example": "30%"}
          ]}
        },
        {"type": "BUTTONS", "buttons": [
          {"type": "QUICK_REPLY", "text": "Unsubscribe"},
          {"type": "COPY_CODE", "example": "WINTER25"}
        ]}
      ]
    });
    let def = TemplateDefinition::new("winter_sale_coupon", "en_US", TemplateCategory::Marketing)
        .parameter_format(ParameterFormat::Named)
        .component(TemplateComponent::header_text("Our Winter Sale is on!"))
        .component(TemplateComponent::body_named(
            "Shop now through the end of December and use the one-time use code {{coupon_code}} to get {{discount}} off of your entire order!",
            [("coupon_code", "WINTER25"), ("discount", "30%")],
        ))
        .component(TemplateComponent::buttons([
            Button::quick_reply("Unsubscribe"),
            Button::copy_code("WINTER25"),
        ]));
    assert_definition(&def, doc);
}

#[test]
fn catalog_template() {
    // catalogs/catalog-template-messages, example request.
    let doc = json!({
      "name": "intro_catalog_offer",
      "language": "en_US",
      "category": "MARKETING",
      "components": [
        {
          "type": "BODY",
          "text": "Now shop for your favorite products right here on WhatsApp! Get Rs {{1}} off on all orders above {{2}}Rs! Valid for your first {{3}} orders placed on WhatsApp!",
          "example": {"body_text": [["100", "400", "3"]]}
        },
        {"type": "FOOTER", "text": "Best grocery deals on WhatsApp!"},
        {"type": "BUTTONS", "buttons": [{"type": "CATALOG", "text": "View catalog"}]}
      ]
    });
    let def = TemplateDefinition::new("intro_catalog_offer", "en_US", TemplateCategory::Marketing)
        .component(TemplateComponent::body_positional(
            "Now shop for your favorite products right here on WhatsApp! Get Rs {{1}} off on all orders above {{2}}Rs! Valid for your first {{3}} orders placed on WhatsApp!",
            ["100", "400", "3"],
        ))
        .component(TemplateComponent::footer("Best grocery deals on WhatsApp!"))
        .component(TemplateComponent::buttons([Button::catalog("View catalog")]));
    assert_definition(&def, doc);
}

#[test]
fn mpm_template() {
    // catalogs/mpm-template-messages, example request.
    let doc = json!({
      "name": "abandoned_cart",
      "language": "en_US",
      "category": "MARKETING",
      "components": [
        {"type": "HEADER", "format": "TEXT", "text": "Forget something, {{1}}?", "example": {"header_text": ["Pablo"]}},
        {
          "type": "BODY",
          "text": "Looks like you left these items in your cart, still interested? Use code {{1}} to get 10% off!",
          "example": {"body_text": [["10OFF"]]}
        },
        {"type":"BUTTONS", "buttons": [{"type": "MPM", "text": "View items"}]}
      ]
    });
    let def = TemplateDefinition::new("abandoned_cart", "en_US", TemplateCategory::Marketing)
        .component(TemplateComponent::header_text_positional("Forget something, {{1}}?", "Pablo"))
        .component(TemplateComponent::body_positional(
            "Looks like you left these items in your cart, still interested? Use code {{1}} to get 10% off!",
            ["10OFF"],
        ))
        .component(TemplateComponent::buttons([Button::mpm("View items")]));
    assert_definition(&def, doc);
}

#[test]
fn spm_template() {
    // catalogs/spm-template-messages, example request.
    let doc = json!({
      "name": "spm_template_named_params",
      "language": "en_US",
      "category": "marketing",
      "parameter_format": "named",
      "components": [
        {"type": "header", "format": "product"},
        {
          "type": "body",
          "text": "Use code {{code}} to get {{percent}} off our newest succulent!",
          "example": {"body_text_named_params": [
            {"param_name": "code", "example": "15OFF"},
            {"param_name": "percent", "example": "15%"}
          ]}
        },
        {"type": "footer", "text": "Offer ends September 22, 2024"},
        {"type": "buttons", "buttons": [{"type": "spm", "text": "View"}]}
      ]
    });
    let def = TemplateDefinition::new(
        "spm_template_named_params",
        "en_US",
        TemplateCategory::Marketing,
    )
    .parameter_format(ParameterFormat::Named)
    .component(TemplateComponent::header_product())
    .component(TemplateComponent::body_named(
        "Use code {{code}} to get {{percent}} off our newest succulent!",
        [("code", "15OFF"), ("percent", "15%")],
    ))
    .component(TemplateComponent::footer("Offer ends September 22, 2024"))
    .component(TemplateComponent::buttons([Button::spm("View")]));
    assert_definition(&def, doc);
}

#[test]
fn call_permission_request_template() {
    // templates/marketing-templates/call-permission-request-message-template.
    let doc = json!({
        "name": "vip_early_access_call",
        "language": "en_US",
        "category": "MARKETING",
        "parameter_format": "named",
        "components": [
          {
            "type": "body",
            "text": "Hi {{first_name}}, as a Lucky Shrub VIP, get a first look at our rare new succulents before anyone else. Can we give you a quick call?",
            "example": {"body_text_named_params": [{"param_name": "first_name", "example": "Pablo"}]}
          },
          {"type": "call_permission_request"}
       ]
    });
    let def = TemplateDefinition::new("vip_early_access_call", "en_US", TemplateCategory::Marketing)
        .parameter_format(ParameterFormat::Named)
        .component(TemplateComponent::body_named(
            "Hi {{first_name}}, as a Lucky Shrub VIP, get a first look at our rare new succulents before anyone else. Can we give you a quick call?",
            [("first_name", "Pablo")],
        ))
        .component(TemplateComponent::call_permission_request());
    assert_definition(&def, doc.clone());
    // Sent exactly as the page spells it.
    assert_eq!(
        serde_json::to_value(&def).unwrap()["components"][1],
        json!({"type": "call_permission_request"})
    );
}

#[test]
fn voice_call_button_components() {
    // calling/call-button-messages-deep-links, "Create call button message
    // template" request body (name/category/language are placeholders there).
    let doc = json!([
        {"type": "BODY", "text": "You can call us on WhatsApp now for faster service!"},
        {"type": "BUTTONS", "buttons": [
            {"type": "voice_call", "text": "Call Now", "ttl_minutes": 1440},
            {"type": "URL", "text": "Contact Support", "url": "https://www.luckyshrub.com/support"}
        ]}
    ]);
    let components = vec![
        TemplateComponent::body("You can call us on WhatsApp now for faster service!"),
        TemplateComponent::buttons([
            Button::voice_call(Some("Call Now".into()), Some(1440)),
            Button::url("Contact Support", "https://www.luckyshrub.com/support"),
        ]),
    ];
    assert_eq!(
        normalize(serde_json::to_value(&components).unwrap()),
        normalize(doc.clone())
    );
    let parsed: Vec<TemplateComponent> = serde_json::from_value(doc).unwrap();
    assert_eq!(parsed, components);
    let mut def = TemplateDefinition::new("call_us", "en", TemplateCategory::Marketing);
    def.components = components;
    def.validate().unwrap();
}

#[test]
fn request_contact_info_button() {
    // business-scoped-user-ids#using-templates.
    let doc = json!({"type": "buttons", "buttons": [{"type": "REQUEST_CONTACT_INFO"}]});
    let c = TemplateComponent::buttons([Button::request_contact_info()]);
    assert_eq!(
        normalize(serde_json::to_value(&c).unwrap()),
        normalize(doc.clone())
    );
    assert_eq!(serde_json::from_value::<TemplateComponent>(doc).unwrap(), c);
}

#[test]
fn flow_button_uses_the_reference_field_names() {
    // No complete creation example in the docs mirror; field names from the
    // reference `Buttons` schema and flows/guides/flows-templates.
    let b = Button::Flow(FlowButton::by_id("Book now", "1234567890").navigate("WELCOME_SCREEN"));
    assert_eq!(
        serde_json::to_value(&b).unwrap(),
        json!({"type": "FLOW", "text": "Book now", "flow_id": "1234567890",
               "flow_action": "navigate", "navigate_screen": "WELCOME_SCREEN"})
    );
    let mut def = TemplateDefinition::new("book", "en_US", TemplateCategory::Marketing);
    def.components = vec![
        TemplateComponent::body("Book a table."),
        TemplateComponent::buttons([b]),
    ];
    def.validate().unwrap();
    // Exactly one flow source.
    let neither = Button::Flow(FlowButton {
        text: "x".into(),
        ..FlowButton::default()
    });
    def.components[1] = TemplateComponent::buttons([neither]);
    assert!(def.validate().is_err());
}

#[test]
fn ttl_example_contradicts_its_own_table() {
    // templates/time-to-live example: MARKETING with 120 s, which the page's
    // table (43200–2592000 for marketing) forbids. Shape is right; our
    // validation follows the table.
    let doc = json!({
        "name": "test_template",
        "language": "en_US",
        "category": "MARKETING",
        "message_send_ttl_seconds": 120,
        "components": [
          {"type": "BODY", "text": "Shop now through {{1}} and use code {{2}} to get {{3}} off of all merchandise.",
           "example": {"body_text": [["the end of August","25OFF","25%"]]}},
          {"type": "FOOTER", "text": "Use the buttons below to manage your marketing subscriptions"}
        ]
    });
    let def: TemplateDefinition = serde_json::from_value(doc.clone()).unwrap();
    assert_eq!(serde_json::to_value(&def).unwrap(), doc);
    let e = def.validate().unwrap_err();
    assert!(e.field == "message_send_ttl_seconds", "{e}");
}

#[test]
fn unknown_shapes_survive_parsing_and_round_trip() {
    let raw = json!([
        {"type": "HEADER", "format": "HOLOGRAM", "text": "x"},
        {"type": "SPARKLES", "intensity": 3},
        {"type": "BUTTONS", "buttons": [
            {"type": "TELEPORT", "text": "Go"},
            {"type": "URL", "text": "missing url"},
            {"type": "OTP", "otp_type": "FOUR_TAP"}
        ]}
    ]);
    let parsed: Vec<TemplateComponent> = serde_json::from_value(raw.clone()).unwrap();
    assert!(
        matches!(&parsed[0], TemplateComponent::Header(h) if h.format == HeaderFormat::Other("HOLOGRAM".into()))
    );
    assert!(matches!(parsed[1], TemplateComponent::Other(_)));
    let TemplateComponent::Buttons { buttons } = &parsed[2] else {
        panic!("{parsed:?}")
    };
    assert!(buttons.iter().all(|b| matches!(b, Button::Other(_))));
    assert_eq!(serde_json::to_value(&parsed).unwrap(), raw);
}

// -------------------------------------------------------------- invocations

#[test]
fn named_parameters_send() {
    // templates/overview#named-parameters, send payload.
    let doc = json!({
      "messaging_product": "whatsapp", "recipient_type": "individual", "to": "+16505551234",
      "type": "template",
      "template": {
        "name": "order_confirmation",
        "language": {"code": "en_US"},
        "components": [{"type": "body", "parameters": [
          {"type": "text", "parameter_name": "first_name", "text": "Jessica"},
          {"type": "text", "parameter_name": "order_number", "text": "SKBUP2-4CPIG9"}
        ]}]
      }
    });
    let m = TemplateMessage::new("order_confirmation", "en_US").body([
        Parameter::named("first_name", "Jessica"),
        Parameter::named("order_number", "SKBUP2-4CPIG9"),
    ]);
    assert_invocation(&m, &doc);
}

#[test]
fn positional_parameters_send() {
    // templates/overview#positional-parameters, send payload.
    let doc = json!({"template": {
        "name": "order_confirmation",
        "language": {"code": "en_US"},
        "components": [{"type": "body", "parameters": [
          {"type": "text", "text": "Jessica"},
          {"type": "text", "text": "SKBUP2-4CPIG9"}
        ]}]
    }});
    let m = TemplateMessage::new("order_confirmation", "en_US")
        .body([Parameter::text("Jessica"), Parameter::text("SKBUP2-4CPIG9")]);
    assert_invocation(&m, &doc);
}

#[test]
fn marketing_send_with_image_header() {
    // templates/marketing-templates/custom-marketing-templates, send example.
    let doc = json!({"template": {
        "name": "welcome_discount_template",
        "language": {"code": "en_US"},
        "components": [
          {"type": "header", "parameters": [{"type": "image", "image": {"id": "1339522734477770"}}]},
          {"type": "body", "parameters": [
            {"type": "text", "parameter_name": "first_name", "text": "Jessica"},
            {"type": "text", "parameter_name": "discount_code", "text": "WELCOME25"},
            {"type": "text", "parameter_name": "discount_amount", "text": "25%"}
          ]}
        ]
    }});
    let m = TemplateMessage::new("welcome_discount_template", "en_US")
        .header(Parameter::image_id("1339522734477770"))
        .body([
            Parameter::named("first_name", "Jessica"),
            Parameter::named("discount_code", "WELCOME25"),
            Parameter::named("discount_amount", "25%"),
        ]);
    assert_invocation(&m, &doc);
}

#[test]
fn media_parameters_currency_and_date_time() {
    // templates/template-media, send syntax (placeholders filled in).
    let doc = json!({"template": {
        "name": "TEMPLATE_NAME",
        "language": {"code": "LANGUAGE_AND_LOCALE_CODE"},
        "components": [
          {"type": "header", "parameters": [{"type": "image", "image": {"link": "https://URL"}}]},
          {"type": "body", "parameters": [
            {"type": "text", "text": "TEXT-STRING"},
            {"type": "currency", "currency": {"fallback_value": "VALUE", "code": "USD", "amount_1000": 100990}},
            {"type": "date_time", "date_time": {"fallback_value": "MONTH DAY, YEAR"}}
          ]}
        ]
    }});
    let m = TemplateMessage {
        name: "TEMPLATE_NAME".into(),
        ..TemplateMessage::new("x", "LANGUAGE_AND_LOCALE_CODE")
    }
    .header(Parameter::image_link("https://URL"))
    .body([
        Parameter::text("TEXT-STRING"),
        Parameter::currency("VALUE", "USD", 100_990),
        Parameter::date_time("MONTH DAY, YEAR"),
    ]);
    assert_eq!(serde_json::to_value(&m).unwrap(), doc["template"]);
    let parsed: TemplateMessage = serde_json::from_value(doc["template"].clone()).unwrap();
    assert_eq!(parsed, m);
    assert!(
        m.validate().is_err(),
        "`TEMPLATE_NAME` is not a valid template name"
    );
}

#[test]
fn document_parameter_with_filename() {
    let p = Parameter::document_id("1602186516975000", Some("receipt.pdf".into()));
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(
        v,
        json!({"type": "document", "document": {"id": "1602186516975000", "filename": "receipt.pdf"}})
    );
    assert_eq!(serde_json::from_value::<Parameter>(v).unwrap(), p);
}

#[test]
fn media_card_carousel_send() {
    // templates/marketing-templates/media-card-carousel-templates, send example.
    let card = |i: u32, media: &str, payload: &str, suffix: &str| {
        json!({"card_index": i, "components": [
            {"type": "header", "parameters": [{"type": "image", "image": {"id": media}}]},
            {"type": "button", "sub_type": "quick_reply", "index": "0", "parameters": [{"type": "payload", "payload": payload}]},
            {"type": "button", "sub_type": "url", "index": "1", "parameters": [{"type": "text", "text": suffix}]}
        ]})
    };
    let doc = json!({"template": {
        "name": "carousel_template_media_cards_v1",
        "language": {"code": "en_US"},
        "components": [
          {"type": "body", "parameters": [
            {"type": "text", "text": "Pablo"}, {"type": "text", "text": "20%"}, {"type": "text", "text": "20OFF"}
          ]},
          {"type": "carousel", "cards": [
            card(0, "1558081531584829", "more-aloes", "blue-elf"),
            card(1, "861236878885705", "more-crassulas", "buddhas-temple"),
            card(2, "1587064918516321", "more-echeverias", "black-prince")
          ]}
        ]
    }});
    let card = |i: u32, media: &str, payload: &str, suffix: &str| {
        CarouselCardParameters::new(
            i,
            [
                SendComponent::header(Parameter::image_id(media)),
                SendComponent::quick_reply_button(0, payload),
                SendComponent::url_button(1, suffix),
            ],
        )
    };
    let m = TemplateMessage::new("carousel_template_media_cards_v1", "en_US")
        .body([
            Parameter::text("Pablo"),
            Parameter::text("20%"),
            Parameter::text("20OFF"),
        ])
        .carousel([
            card(0, "1558081531584829", "more-aloes", "blue-elf"),
            card(1, "861236878885705", "more-crassulas", "buddhas-temple"),
            card(2, "1587064918516321", "more-echeverias", "black-prince"),
        ]);
    assert_invocation(&m, &doc);
}

#[test]
fn send_carousels_take_at_most_ten_cards() {
    // media-card-carousel-templates and product-card-carousel-template-messages:
    // up to 10 cards.
    let cards = |n: u32| {
        TemplateMessage::new("carousel_template_media_cards_v1", "en_US").carousel((0..n).map(
            |i| CarouselCardParameters::new(i, [SendComponent::header(Parameter::image_id("1"))]),
        ))
    };
    cards(10).validate().unwrap();
    let e = cards(11).validate().unwrap_err();
    assert!(e.field == "template.components[0].cards", "{e}");
}

#[test]
fn product_card_carousel_send() {
    // catalogs/product-card-carousel-template-messages, send example.
    let card = |i: u32, id: &str| {
        json!({"card_index": i, "components": [{"type": "header", "parameters": [
            {"type": "product", "product": {"product_retailer_id": id, "catalog_id": "194836987003835"}}
        ]}]})
    };
    let doc = json!({"template": {
        "name": "carousel_template_product_cards_v1",
        "language": {"code": "en_US"},
        "components": [
          {"type": "body", "parameters": [{"type": "text", "text": "Pablo"}]},
          {"type": "carousel", "cards": [card(0, "vrpj01fvwp"), card(1, "va2l5ioeat"), card(2, "sqpjv0mgde")]}
        ]
    }});
    let card = |i: u32, id: &str| {
        CarouselCardParameters::new(
            i,
            [SendComponent::header(Parameter::product(
                id,
                "194836987003835",
            ))],
        )
    };
    let m = TemplateMessage::new("carousel_template_product_cards_v1", "en_US")
        .body([Parameter::text("Pablo")])
        .carousel([
            card(0, "vrpj01fvwp"),
            card(1, "va2l5ioeat"),
            card(2, "sqpjv0mgde"),
        ]);
    assert_invocation(&m, &doc);
}

#[test]
fn limited_time_offer_send() {
    // templates/marketing-templates/limited-time-offer-templates, send
    // example (numeric indexes there).
    let doc = json!({"template": {
        "name": "limited_time_offer_caribbean_pkg_2023",
        "language": {"code": "en_US"},
        "components": [
          {"type": "header", "parameters": [{"type": "image", "image": {"id": "1602186516975000"}}]},
          {"type": "body", "parameters": [{"type": "text", "text": "Pablo"}, {"type": "text", "text": "CARIBE25"}]},
          {"type": "limited_time_offer", "parameters": [
            {"type": "limited_time_offer", "limited_time_offer": {"expiration_time_ms": 1209600000}}
          ]},
          {"type": "button", "sub_type": "copy_code", "index": 0, "parameters": [{"type": "coupon_code", "coupon_code": "CARIBE25"}]},
          {"type": "button", "sub_type": "url", "index": 1, "parameters": [{"type": "text", "text": "n3mtql"}]}
        ]
    }});
    let m = TemplateMessage::new("limited_time_offer_caribbean_pkg_2023", "en_US")
        .header(Parameter::image_id("1602186516975000"))
        .body([Parameter::text("Pablo"), Parameter::text("CARIBE25")])
        .limited_time_offer(1_209_600_000)
        .copy_code_button(0, "CARIBE25")
        .url_button(1, "n3mtql");
    assert_invocation(&m, &doc);
}

#[test]
fn coupon_send() {
    // templates/marketing-templates/coupon-templates, send example.
    let doc = json!({"template": {
        "name": "winter_sale_coupon",
        "language": {"code": "en_US"},
        "components": [
          {"type": "body", "parameters": [
            {"type": "text", "parameter_name": "coupon_code", "text": "WINTER25"},
            {"type": "text", "parameter_name": "discount", "text": "30%"}
          ]},
          {"type": "button", "sub_type": "copy_code", "index": 1, "parameters": [{"type": "coupon_code", "coupon_code": "WINTER25"}]}
        ]
    }});
    let m = TemplateMessage::new("winter_sale_coupon", "en_US")
        .body([
            Parameter::named("coupon_code", "WINTER25"),
            Parameter::named("discount", "30%"),
        ])
        .copy_code_button(1, "WINTER25");
    assert_invocation(&m, &doc);
    let long =
        TemplateMessage::new("winter_sale_coupon", "en_US").copy_code_button(1, "x".repeat(21));
    assert!(
        long.validate().is_err(),
        "coupon codes are at most 20 characters"
    );
}

#[test]
fn catalog_send() {
    // catalogs/catalog-template-messages, send example (`sub_type: CATALOG`).
    let doc = json!({"template": {
        "name": "intro_catalog_offer",
        "language": {"code": "en_US"},
        "components": [
          {"type": "body", "parameters": [
            {"type": "text", "text": "100"}, {"type": "text", "text": "400"}, {"type": "text", "text": "3"}
          ]},
          {"type": "button", "sub_type": "CATALOG", "index": 0, "parameters": [
            {"type": "action", "action": {"thumbnail_product_retailer_id": "2lc20305pt"}}
          ]}
        ]
    }});
    let m = TemplateMessage::new("intro_catalog_offer", "en_US")
        .body([
            Parameter::text("100"),
            Parameter::text("400"),
            Parameter::text("3"),
        ])
        .catalog_button(0, Some("2lc20305pt".into()));
    assert_invocation(&m, &doc);
}

#[test]
fn mpm_send_and_limits() {
    // catalogs/mpm-template-messages, send example.
    let doc = json!({"template": {
        "name": "abandoned_cart",
        "language": {"code": "en_US"},
        "components": [
          {"type": "header", "parameters": [{"type": "text", "text": "Pablo"}]},
          {"type": "body", "parameters": [{"type": "text", "text": "10OFF"}]},
          {"type": "button", "sub_type": "mpm", "index": 0, "parameters": [{"type": "action", "action": {
            "thumbnail_product_retailer_id": "2lc20305pt",
            "sections": [
              {"title": "Popular Bundles", "product_items": [
                {"product_retailer_id": "2lc20305pt"}, {"product_retailer_id": "nseiw1x3ch"}
              ]},
              {"title": "Premium Packages", "product_items": [{"product_retailer_id": "n6k6x0y7oe"}]}
            ]
          }}]}
        ]
    }});
    let m = TemplateMessage::new("abandoned_cart", "en_US")
        .header(Parameter::text("Pablo"))
        .body([Parameter::text("10OFF")])
        .mpm_button(
            0,
            "2lc20305pt",
            [
                MpmSection::new("Popular Bundles", ["2lc20305pt", "nseiw1x3ch"]),
                MpmSection::new("Premium Packages", ["n6k6x0y7oe"]),
            ],
        );
    assert_invocation(&m, &doc);

    let too_many_sections = TemplateMessage::new("abandoned_cart", "en_US").mpm_button(
        0,
        "a",
        (0..11).map(|i| MpmSection::new(format!("s{i}"), ["x"])),
    );
    assert!(too_many_sections.validate().is_err());
    let long_title = TemplateMessage::new("abandoned_cart", "en_US").mpm_button(
        0,
        "a",
        [MpmSection::new("x".repeat(25), ["x"])],
    );
    assert!(long_title.validate().is_err());
    let too_many_products = TemplateMessage::new("abandoned_cart", "en_US").mpm_button(
        0,
        "a",
        [
            MpmSection::new("a", (0..16).map(|i| format!("p{i}"))),
            MpmSection::new("b", (0..15).map(|i| format!("q{i}"))),
        ],
    );
    assert!(
        too_many_products.validate().is_err(),
        "30 products across all sections"
    );
}

#[test]
fn spm_send() {
    // catalogs/spm-template-messages, send example.
    let doc = json!({"template": {
        "name": "spm_template_named_params",
        "language": {"code": "en_US"},
        "components": [
          {"type": "header", "parameters": [{"type": "product", "product": {
            "product_retailer_id": "nqryix03ez", "catalog_id": "194836987003835"}}]},
          {"type": "body", "parameters": [
            {"type": "text", "parameter_name": "code", "text": "25OFF"},
            {"type": "text", "parameter_name": "percent", "text": "25%"}
          ]}
        ]
    }});
    let m = TemplateMessage::new("spm_template_named_params", "en_US")
        .header(Parameter::product("nqryix03ez", "194836987003835"))
        .body([
            Parameter::named("code", "25OFF"),
            Parameter::named("percent", "25%"),
        ]);
    assert_invocation(&m, &doc);
}

#[test]
fn flow_button_send() {
    // flows/guides/flows-templates: the page's `flow_action_data` is `{ ... }`.
    let m = TemplateMessage::new("book_table", "en_US").flow_button(
        0,
        Some("FLOW_TOKEN".into()),
        Some(json!({"guests": 2})),
    );
    assert_eq!(
        serde_json::to_value(&m).unwrap()["components"][0],
        json!({"type": "button", "sub_type": "flow", "index": "0", "parameters": [
            {"type": "action", "action": {"flow_token": "FLOW_TOKEN", "flow_action_data": {"guests": 2}}}
        ]})
    );
}

#[test]
fn location_send_with_deterministic_policy() {
    // templates/utility-templates/location-templates, send example.
    let doc = json!({"template": {
        "name": "order_delivery_update",
        "language": {"policy": "deterministic", "code": "en_US"},
        "components": [
          {"type": "header", "parameters": [{"type": "location", "location": {
            "latitude": "37.44211676562361", "longitude": "-122.16155960083124",
            "name": "Philz Coffee", "address": "101 Forest Ave, Palo Alto, CA 94301"}}]},
          {"type": "body", "parameters": [
            {"type": "text", "parameter_name": "customer_name", "text": "Jane"},
            {"type": "text", "parameter_name": "order_number", "text": "892104"}
          ]}
        ]
    }});
    let m = TemplateMessage::new("order_delivery_update", "en_US")
        .deterministic()
        .header(Parameter::location(TemplateLocation {
            latitude: "37.44211676562361".into(),
            longitude: "-122.16155960083124".into(),
            name: Some("Philz Coffee".into()),
            address: Some("101 Forest Ave, Palo Alto, CA 94301".into()),
        }))
        .body([
            Parameter::named("customer_name", "Jane"),
            Parameter::named("order_number", "892104"),
        ]);
    assert_invocation(&m, &doc);
}

#[test]
fn tap_target_send() {
    // templates/tap-target-url-title-override, example request.
    let doc = json!({"template": {
        "name": "august_promotion",
        "language": {"code": "en"},
        "components": [
          {"type": "header", "parameters": [{"type": "image", "image": {"link": "https://www.luckyshrubs.com"}}]},
          {"type": "body", "parameters": [{"type": "text", "text": "Hello Andy..."}]},
          {"type": "tap_target_configuration", "parameters": [{"type": "tap_target_configuration",
            "tap_target_configuration": [{"url": "https://www.luckyshrubs.com/", "title": "Offer Details"}]}]}
        ]
    }});
    let m = TemplateMessage::new("august_promotion", "en")
        .header(Parameter::image_link("https://www.luckyshrubs.com"))
        .body([Parameter::text("Hello Andy...")])
        .tap_target("https://www.luckyshrubs.com/", "Offer Details");
    assert_invocation(&m, &doc);
}

#[test]
fn voice_call_send_has_no_index() {
    // calling/call-button-messages-deep-links, send request body.
    let doc = json!({"template": {
        "name": "wa_voice_call",
        "language": {"code": "en"},
        "components": [{"type": "button", "sub_type" : "voice_call", "parameters": [
            {"type": "ttl_minutes", "ttl_minutes": 100},
            {"type": "payload", "payload": "payload data"}
        ]}]
    }});
    let m = TemplateMessage::new("wa_voice_call", "en")
        .voice_call_button(Some(100), Some("payload data".into()));
    assert_invocation(&m, &doc);
    let zero = TemplateMessage::new("wa_voice_call", "en").voice_call_button(Some(0), None);
    assert!(zero.validate().is_err());
}

#[test]
fn url_button_with_named_parameter() {
    // templates/components#url-encoding.
    let doc = json!({"type": "button", "sub_type": "url", "index": "0", "parameters": [
        {"type": "text", "parameter_name": "customer_name", "text": "Gon%C3%A7alves"}]});
    let c = SendComponent::Button {
        sub_type: ButtonSubType::Url,
        index: Some(0),
        parameters: vec![Parameter::named("customer_name", "Gon%C3%A7alves")],
    };
    assert_eq!(serde_json::to_value(&c).unwrap(), doc);
    assert_eq!(serde_json::from_value::<SendComponent>(doc).unwrap(), c);
}

#[test]
fn parameter_debug_redacts_text_and_codes() {
    let m = TemplateMessage::new("otp", "en_US")
        .body([Parameter::text("483920")])
        .url_button(0, "483920")
        .copy_code_button(1, "483920");
    let debug = format!("{m:?}");
    assert!(!debug.contains("483920"), "{debug}");
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn bad_button_index_is_a_parse_error() {
    let raw = json!({"type": "button", "sub_type": "url", "index": "first", "parameters": []});
    // The known variant fails, so the value is kept verbatim.
    assert!(matches!(
        serde_json::from_value::<SendComponent>(raw).unwrap(),
        SendComponent::Other(_)
    ));
}

// ---------------------------------------------------------------- endpoints

fn management_list_example() -> Value {
    // templates/template-management, "Get all templates" response (the
    // page's `...` truncation removed).
    json!({
      "data": [
        {
          "name": "reservation_confirmation",
          "parameter_format": "NAMED",
          "components": [
            {"type": "HEADER", "format": "IMAGE", "example": {"header_handle": ["https://scontent.whatsapp.net/v/t61..."]}},
            {
              "type": "BODY",
              "text": "*You're all set!*\n\nYour reservation for {{number_of_guests}} at Lucky Shrub Eatery on {{day}}, {{date}}, at {{time}}, is confirmed. See you then!",
              "example": {"body_text_named_params": [
                {"param_name": "number_of_guests", "example": "4"},
                {"param_name": "day", "example": "Saturday"},
                {"param_name": "date", "example": "August 30th, 2025"},
                {"param_name": "time", "example": "7:30 pm"}
              ]}
            },
            {"type": "FOOTER", "text": "Lucky Shrub Eatery: The Luckiest Eatery in Town!"},
            {"type": "BUTTONS", "buttons": [
              {"type": "URL", "text": "Change reservation", "url": "https://www.luckyshrubeater.com/reservations"},
              {"type": "PHONE_NUMBER", "text": "Call us", "phone_number": "+16467043595"},
              {"type": "QUICK_REPLY", "text": "Cancel reservation"}
            ]}
          ],
          "language": "en_US",
          "status": "APPROVED",
          "category": "UTILITY",
          "id": "1387372356726668"
        },
        {
          "name": "coupon_expiration_reminder_number_vars",
          "parameter_format": "POSITIONAL",
          "components": [
            {"type": "HEADER", "format": "TEXT", "text": "Act fast, {{1}}!", "example": {"header_text": ["Pablo"]}},
            {
              "type": "BODY",
              "text": "Just a quick reminder—your exclusive coupon code, {{1}}, *expires in only {{2}} days!* Don't miss out on our special deals. Use your code at checkout before it's too late.\n\nHappy shopping! 😃",
              "example": {"body_text": [["SUMMER20", "10"]]}
            },
            {"type": "FOOTER", "text": "Lucky Shrub Succulents"},
            {"type": "BUTTONS", "buttons": [
              {"type": "URL", "text": "See deals", "url": "https://www.luckyshrub.com/deals"},
              {"type": "QUICK_REPLY", "text": "Unsubscribe"}
            ]}
          ],
          "language": "en",
          "status": "APPROVED",
          "category": "MARKETING",
          "sub_category": "CUSTOM",
          "id": "1304694804498707"
        }
      ],
      "paging": {
        "cursors": {"before": "QVFIU...", "after": "QVFIU..."},
        "next": "https://graph.facebook.com/v23.0/10229..."
      }
    })
}

#[tokio::test]
async fn list_parses_the_documented_response() {
    let t = ScriptedTransport::new();
    t.push_json(200, management_list_example());
    let page = client(&t)
        .templates("102290129340398")
        .list(&TemplateListQuery::new())
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/102290129340398/message_templates");
    assert_eq!(req.url.query(), None);
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(t.remaining(), 0);

    assert_eq!(page.data.len(), 2);
    let first = &page.data[0];
    assert_eq!(first.id, TemplateId::new("1387372356726668"));
    assert_eq!(first.status, Some(TemplateStatus::Approved));
    assert_eq!(first.category, Some(TemplateCategory::Utility));
    assert_eq!(first.parameter_format, Some(ParameterFormat::Named));
    assert_eq!(first.components.len(), 4);
    assert!(
        matches!(&first.components[0], TemplateComponent::Header(h) if h.format == HeaderFormat::Image)
    );
    let TemplateComponent::Buttons { buttons } = &first.components[3] else {
        panic!("{:?}", first.components[3])
    };
    assert_eq!(buttons[1], Button::phone_number("Call us", "+16467043595"));
    assert_eq!(page.data[1].sub_category, Some(TemplateSubCategory::Custom));
    assert_eq!(page.next_cursor(), Some("QVFIU..."));
}

#[tokio::test]
async fn list_sends_the_documented_filters() {
    // templates/template-management, "specific fields" and "approved" examples.
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"data": [
      {"name": "reservation_confirmation", "category": "UTILITY", "status": "APPROVED", "id": "1387372356726668"},
      {"name": "address_update", "category": "UTILITY", "status": "PENDING", "id": "1137051647947973"},
      {"name": "reservation_confirmation_short_banner", "category": "UTILITY", "status": "REJECTED", "id": "1166414785519855"}
    ], "paging": {"cursors": {"before": "QVFIU...", "after": "QVFIU..."}, "next": "https://graph.facebook.com/v23.0/10229..."}}));
    let q = TemplateListQuery::new()
        .fields(["name", "category", "status"])
        .limit(5)
        .status(TemplateStatus::Approved)
        .name("reservation_confirmation");
    let page = client(&t)
        .templates("102290129340398")
        .list(&q)
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.query("fields").as_deref(), Some("name,category,status"));
    assert_eq!(req.query("limit").as_deref(), Some("5"));
    assert_eq!(req.query("status").as_deref(), Some("APPROVED"));
    assert_eq!(
        req.query("name").as_deref(),
        Some("reservation_confirmation")
    );
    assert_eq!(page.data[1].status, Some(TemplateStatus::Pending));
    assert!(page.data[0].components.is_empty());
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn list_sends_the_callers_cursors() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"data": []}));
    t.push_json(200, json!({"data": []}));
    let templates = client(&t).templates("102290129340398");
    let next = TemplateListQuery {
        after: Some("QVFIU...".into()),
        ..TemplateListQuery::new().limit(5)
    };
    let previous = TemplateListQuery {
        before: Some("QVFIB...".into()),
        ..TemplateListQuery::new()
    };
    templates.list(&next).await.unwrap();
    templates.list(&previous).await.unwrap();
    let reqs = t.requests();
    assert_eq!(reqs[0].query("after").as_deref(), Some("QVFIU..."));
    assert_eq!(reqs[0].query("limit").as_deref(), Some("5"));
    assert_eq!(reqs[1].query("before").as_deref(), Some("QVFIB..."));
    assert_eq!(reqs[1].query("after"), None);
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn list_stream_follows_cursors_and_limit_zero_is_refused() {
    let t = ScriptedTransport::new();
    t.push_json(
        200,
        json!({"data": [{"id": "1"}, {"id": "2"}],
        "paging": {"cursors": {"after": "c1"}, "next": "https://graph.facebook.com/v25.0/x"}}),
    );
    t.push_json(
        200,
        json!({"data": [{"id": "3"}], "paging": {"cursors": {"after": "c2"}}}),
    );
    let templates = client(&t).templates("102290129340398");
    let q = TemplateListQuery::new().limit(2);
    let ids: Vec<String> = templates
        .list_stream(&q)
        // Bounded: a broken paginator must fail the test, not loop forever.
        .take(10)
        .map(|r| r.unwrap().id.into_inner())
        .collect()
        .await;
    assert_eq!(ids, ["1", "2", "3"]);
    let reqs = t.requests();
    assert_eq!(reqs[0].query("after"), None);
    assert_eq!(reqs[1].query("after").as_deref(), Some("c1"));
    assert_eq!(t.remaining(), 0);

    let zero = TemplateListQuery::new().limit(0);
    assert!(templates.list(&zero).await.is_err());
    let items: Vec<_> = templates.list_stream(&zero).take(10).collect().await;
    assert!(matches!(
        items.as_slice(),
        [Err(wa_core::Error::Validation(_))]
    ));
    // The stream manages the cursors: a caller's is refused, not ignored.
    let resumed = TemplateListQuery {
        after: Some("c1".into()),
        ..TemplateListQuery::new().limit(2)
    };
    let items: Vec<_> = templates.list_stream(&resumed).take(10).collect().await;
    assert!(
        matches!(items.as_slice(), [Err(wa_core::Error::Validation(v))] if v.field == "after"),
        "{items:?}"
    );
    assert_eq!(t.requests().len(), 2, "no request for an invalid query");
}

#[tokio::test]
async fn get_status_and_quality_score() {
    let t = ScriptedTransport::new();
    // templates/overview#template-status example response.
    t.push_json(200, json!({"status": "APPROVED", "id": "1259544702043867"}));
    // templates/template-quality example response.
    t.push_json(
        200,
        json!({"quality_score": {"score": "GREEN", "date": 1758754645}, "id": "1387372356726668"}),
    );
    let templates = client(&t).templates("102290129340398");
    let s = templates
        .get_fields(&TemplateId::new("1259544702043867"), &["status"])
        .await
        .unwrap();
    assert_eq!(s.status, Some(TemplateStatus::Approved));
    assert_eq!(s.name, None);
    let req = &t.requests()[0];
    assert_eq!(req.path(), "/v25.0/1259544702043867");
    assert_eq!(req.query("fields").as_deref(), Some("status"));

    let q = templates
        .get_fields(&TemplateId::new("1105258428396250"), &["quality_score"])
        .await
        .unwrap();
    let score = q.quality_score.unwrap();
    assert_eq!(score.score, Some(QualityRating::Green));
    assert_eq!(score.date, Some(1_758_754_645));
    assert_eq!(t.requests()[1].path(), "/v25.0/1105258428396250");
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn get_maps_template_not_found() {
    let t = ScriptedTransport::new();
    // Reference 404 example.
    t.push_json(
        404,
        json!({"error": {"message": "Template not found", "type": "GraphMethodException",
        "code": 803, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let err = client(&t)
        .templates("1")
        .get(&TemplateId::new("42"))
        .await
        .unwrap_err();
    assert_eq!(err.graph().map(|g| g.code), Some(803));
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn create_posts_the_definition_and_parses_the_response() {
    let t = ScriptedTransport::new();
    // custom-marketing-templates example response.
    t.push_json(
        200,
        json!({"id": "1627019861106475", "status": "PENDING", "category": "MARKETING"}),
    );
    let def = TemplateDefinition::new(
        "welcome_discount_template",
        "en_US",
        TemplateCategory::Marketing,
    )
    .component(TemplateComponent::header_image("4::aW..."))
    .component(TemplateComponent::body("Welcome to Lucky Shrub!"))
    .component(TemplateComponent::buttons([Button::url(
        "View deals",
        "https://www.luckyshrub.com/deals",
    )]));
    let created = client(&t)
        .templates("102290129340398")
        .create(&def)
        .await
        .unwrap();
    assert_eq!(created.id, TemplateId::new("1627019861106475"));
    assert_eq!(created.status, Some(TemplateStatus::Pending));
    assert_eq!(created.category, Some(TemplateCategory::Marketing));
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/102290129340398/message_templates");
    assert_eq!(req.bearer(), Some("TOKEN"));
    assert_eq!(
        req.json(),
        Some(json!({
            "name": "welcome_discount_template",
            "language": "en_US",
            "category": "MARKETING",
            "components": [
                {"type": "HEADER", "format": "IMAGE", "example": {"header_handle": ["4::aW..."]}},
                {"type": "BODY", "text": "Welcome to Lucky Shrub!"},
                {"type": "BUTTONS", "buttons": [{"type": "URL", "text": "View deals", "url": "https://www.luckyshrub.com/deals"}]}
            ]
        }))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn invalid_definitions_never_reach_meta() {
    let t = ScriptedTransport::new();
    let def = TemplateDefinition::new("Bad Name", "en_US", TemplateCategory::Marketing)
        .component(TemplateComponent::body("x"));
    let err = client(&t).templates("1").create(&def).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    assert!(matches!(err, wa_core::Error::Validation(ref v) if v.field == "name"));
    assert!(t.requests().is_empty());
}

#[tokio::test]
async fn create_surfaces_graph_errors() {
    let t = ScriptedTransport::new();
    // Reference 400 example.
    t.push_json(400, json!({"error": {
        "message": "Invalid parameter: name must contain only lowercase alphanumeric characters and underscores",
        "type": "OAuthException", "code": 100, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}));
    let def = TemplateDefinition::new("ok_name", "en_US", TemplateCategory::Utility)
        .component(TemplateComponent::body("Hello there."));
    let err = client(&t).templates("1").create(&def).await.unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    assert_eq!(t.requests().len(), 1, "a create is not replayed");
}

#[tokio::test]
async fn edit_category_and_components() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    t.push_json(200, json!({"success": true}));
    let templates = client(&t).templates("102290129340398");
    // templates/template-management#edit-template-category.
    templates
        .edit(
            &TemplateId::new("1252715608684590"),
            &TemplateEdit::category(TemplateCategory::Marketing),
        )
        .await
        .unwrap();
    let req = &t.requests()[0];
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/1252715608684590");
    assert_eq!(req.json(), Some(json!({"category": "MARKETING"})));

    // templates/template-management#edit-template-components.
    let doc = json!({"components": [
        {"type": "HEADER", "format": "TEXT", "text": "Our {{1}} is on!", "example": {"header_text": ["Spring Sale"]}},
        {"type": "BODY", "text": "Shop now through {{1}} and use code {{2}} to get {{3}} off of all merchandise.",
         "example": {"body_text": [["the end of April", "25OFF", "25%"]]}},
        {"type": "FOOTER", "text": "Use the buttons below to manage your marketing subscriptions"},
        {"type": "BUTTONS", "buttons": [
            {"type": "QUICK_REPLY", "text": "Unsubscribe from Promos"},
            {"type": "QUICK_REPLY", "text": "Unsubscribe from All"}
        ]}
    ]});
    let edit: TemplateEdit = serde_json::from_value(doc.clone()).unwrap();
    templates
        .edit(&TemplateId::new("564750795574598"), &edit)
        .await
        .unwrap();
    let req = &t.requests()[1];
    assert_eq!(req.path(), "/v25.0/564750795574598");
    assert_eq!(req.json(), Some(doc));
    assert_eq!(t.remaining(), 0);

    assert!(
        templates
            .edit(&TemplateId::new("1"), &TemplateEdit::default())
            .await
            .is_err()
    );
    assert_eq!(t.requests().len(), 2);
}

#[tokio::test]
async fn delete_by_name_id_and_ids() {
    let t = ScriptedTransport::new();
    for _ in 0..3 {
        t.push_json(200, json!({"success": true}));
    }
    let templates = client(&t).templates("102290129340398");
    templates
        .delete_by_name("order_confirmation")
        .await
        .unwrap();
    templates
        .delete_by_id("order_confirmation", &TemplateId::new("1407680676729941"))
        .await
        .unwrap();
    templates
        .delete_by_ids(&[
            TemplateId::new("1387372356726668"),
            TemplateId::new("1304694804498707"),
        ])
        .await
        .unwrap();
    let reqs = t.requests();
    for r in &reqs {
        assert_eq!(r.method, Method::DELETE);
        assert_eq!(r.path(), "/v25.0/102290129340398/message_templates");
        assert_eq!(r.bearer(), Some("TOKEN"));
    }
    assert_eq!(reqs[0].url.query(), Some("name=order_confirmation"));
    assert_eq!(reqs[1].query("hsm_id").as_deref(), Some("1407680676729941"));
    assert_eq!(reqs[1].query("name").as_deref(), Some("order_confirmation"));
    assert_eq!(
        reqs[2].query("hsm_ids").as_deref(),
        Some("[1387372356726668,1304694804498707]")
    );
    assert_eq!(t.remaining(), 0);

    let too_many: Vec<TemplateId> = (0..101).map(|i| TemplateId::new(i.to_string())).collect();
    assert!(templates.delete_by_ids(&too_many).await.is_err());
    assert!(templates.delete_by_ids(&[]).await.is_err());
    assert_eq!(t.requests().len(), 3);
}

#[tokio::test]
async fn library_browse_and_create() {
    let t = ScriptedTransport::new();
    // templates/template-library example responses (single objects on the
    // page; wrapped in the usual `data` list here).
    t.push_json(200, json!({"data": [
      {
        "name": "low_balance_warning_1",
        "language": "en_US",
        "category": "UTILITY",
        "topic": "PAYMENTS",
        "usecase": "LOW_BALANCE_WARNING",
        "industry": ["FINANCIAL_SERVICES"],
        "header": "Your account balance is low",
        "body": "Hi {{1}},\nThis is to notify you that your {{2}} in your {{3}} account, ending in {{4}} is below your pre-set {{5}} of {{6}}.\nClick the button to deposit more {{7}}.\n{{8}}",
        "body_params": ["Jim", "available funds", "CS Mutual checking plus", "1234", "limit", "$75.00", "funds", "CS Mutual"],
        "buttons": [
          {"type": "URL", "text": "Make a deposit", "url": "https://www.example.com/"},
          {"type": "PHONE_NUMBER", "text": "Call us", "phone_number": "+18005551234"}
        ],
        "id": "7147013345418927"
      },
      {
        "name": "delivery_failed_2_form",
        "language": "en_US",
        "category": "UTILITY",
        "topic": "ORDER_MANAGEMENT",
        "usecase": "DELIVERY_FAILED",
        "industry": ["E_COMMERCE"],
        "body": "We were unable to deliver order {{1}} today.\n\nPlease {{2}} to schedule another delivery attempt.",
        "body_params": ["#12345", "try a redelivery"],
        "body_param_types": ["TEXT", "TEXT"],
        "buttons": [{"type": "FLOW", "text": "Reschedule"}],
        "id": "7138055039625658"
      }
    ]}));
    // Example response of "Creating templates".
    t.push_json(
        200,
        json!({"id": "954638012257287", "status": "APPROVED", "category": "UTILITY"}),
    );
    let templates = client(&t).templates("102290129340398");
    let page = templates
        .library(&LibraryQuery {
            search: Some("payments".into()),
            ..LibraryQuery::default()
        })
        .await
        .unwrap();
    let req = &t.requests()[0];
    assert_eq!(req.path(), "/v25.0/message_template_library");
    assert_eq!(req.url.query(), Some("search=payments"));
    assert_eq!(page.data[0].body_params.len(), 8);
    assert_eq!(
        page.data[0].buttons[0],
        Button::url("Make a deposit", "https://www.example.com/")
    );
    assert!(matches!(&page.data[1].buttons[0], Button::Flow(f) if f.text == "Reschedule"));

    let created = templates
        .create_from_library(&LibraryTemplateRequest {
            name: "my_delivery_update".into(),
            language: "en_US".into(),
            category: TemplateCategory::Utility,
            library_template_name: "delivery_update_1".into(),
            library_template_button_inputs: vec![LibraryButtonInput::url(
                "https://www.example.com/{{1}}",
                Some("https://www.example.com/order_update".into()),
            )],
            library_template_body_inputs: None,
        })
        .await
        .unwrap();
    assert_eq!(created.status, Some(TemplateStatus::Approved));
    let req = &t.requests()[1];
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/102290129340398/message_templates");
    assert_eq!(
        req.json(),
        Some(json!({
            "name": "my_delivery_update",
            "language": "en_US",
            "category": "UTILITY",
            "library_template_name": "delivery_update_1",
            "library_template_button_inputs": [
                {"type": "URL", "url": {"base_url": "https://www.example.com/{{1}}",
                                        "url_suffix_example": "https://www.example.com/order_update"}}
            ]
        }))
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn migrate_between_wabas() {
    let t = ScriptedTransport::new();
    // templates/template-migration example response.
    t.push_json(
        200,
        json!({
          "migrated_templates": ["1473688840035974", "6162904357082268", "6147830171896170"],
          "failed_templates": {
            "1019496902803242": "Incorrect category",
            "259672276895259": "Formatting error - dangling parameter",
            "572279198452421": "Incorrect category"
          }
        }),
    );
    let result = client(&t)
        .templates("104996122399160")
        .migrate_from(
            &"102290129340398".into(),
            &MigrationOptions {
                page_number: Some(0),
                ..MigrationOptions::default()
            },
        )
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(
        req.path(),
        "/v25.0/104996122399160/migrate_message_templates"
    );
    assert_eq!(
        req.json(),
        Some(json!({"source_waba_id": "102290129340398", "page_number": 0}))
    );
    assert_eq!(result.migrated_templates.len(), 3);
    assert_eq!(
        result
            .failed_templates
            .get(&TemplateId::new("259672276895259"))
            .map(String::as_str),
        Some("Formatting error - dangling parameter")
    );
    assert_eq!(t.remaining(), 0);

    let big = MigrationOptions {
        count: Some(501),
        ..MigrationOptions::default()
    };
    assert!(
        client(&t)
            .templates("1")
            .migrate_from(&"2".into(), &big)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn compare_two_templates() {
    let t = ScriptedTransport::new();
    // templates/template-comparison example response.
    t.push_json(200, json!({"data": [
      {"metric": "BLOCK_RATE", "type": "RELATIVE", "order_by_relative_metric": ["1533406637136032", "5289179717853347"]},
      {"metric": "MESSAGE_SENDS", "type": "NUMBER_VALUES", "number_values": [
        {"key": "5289179717853347", "value": 1273}, {"key": "1533406637136032", "value": 1042}]},
      {"metric": "TOP_BLOCK_REASON", "type": "STRING_VALUES", "string_values": [
        {"key": "5289179717853347", "value": "UNKNOWN_BLOCK_REASON"},
        {"key": "1533406637136032", "value": "UNKNOWN_BLOCK_REASON"}]}
    ]}));
    let metrics = client(&t)
        .templates("102290129340398")
        .compare(
            &TemplateId::new("5289179717853347"),
            &TemplateId::new("1533406637136032"),
            1_674_844_791_182,
            1_674_845_395_982,
        )
        .await
        .unwrap();
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::GET);
    assert_eq!(req.path(), "/v25.0/5289179717853347/compare");
    assert_eq!(
        req.query("template_ids").as_deref(),
        Some("[1533406637136032]")
    );
    assert_eq!(req.query("start").as_deref(), Some("1674844791182"));
    assert_eq!(req.query("end").as_deref(), Some("1674845395982"));
    assert_eq!(metrics[0].metric, ComparisonMetricKind::BlockRate);
    assert_eq!(metrics[1].number_values.as_ref().unwrap()[0].value, 1273);
    assert_eq!(
        metrics[2].string_values.as_ref().unwrap()[1].value,
        "UNKNOWN_BLOCK_REASON"
    );
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn unpause_posts_to_the_template() {
    let t = ScriptedTransport::new();
    t.push_json(200, json!({"success": true}));
    let v = client(&t)
        .templates("1")
        .unpause(&TemplateId::new("1105258428396250"))
        .await
        .unwrap();
    assert_eq!(v, json!({"success": true}));
    let req = t.last_request().unwrap();
    assert_eq!(req.method, Method::POST);
    assert_eq!(req.path(), "/v25.0/1105258428396250/unpause");
    assert_eq!(t.remaining(), 0);
}

#[tokio::test]
async fn ids_cannot_escape_their_path_segment() {
    // An id is one path segment, whatever it contains: `123/subscribed_apps`
    // must not become a request to another object with our token.
    let t = ScriptedTransport::new();
    for _ in 0..6 {
        t.push_json(200, json!({"id": "1", "success": true, "data": []}));
    }
    let hostile = TemplateId::new("123/subscribed_apps");
    let templates = client(&t).templates("456/phone_numbers");
    let _ = templates.get(&hostile).await.unwrap();
    let _ = templates.get_fields(&hostile, &["status"]).await.unwrap();
    templates
        .edit(&hostile, &TemplateEdit::category(TemplateCategory::Utility))
        .await
        .unwrap();
    let _ = templates.unpause(&hostile).await.unwrap();
    let _ = templates
        .compare(&hostile, &TemplateId::new("1"), 1, 2)
        .await
        .unwrap();
    templates.delete_by_name("x").await.unwrap();
    let paths: Vec<String> = t.requests().iter().map(|r| r.path().to_owned()).collect();
    assert_eq!(
        paths,
        [
            "/v25.0/123%2Fsubscribed_apps",
            "/v25.0/123%2Fsubscribed_apps",
            "/v25.0/123%2Fsubscribed_apps",
            "/v25.0/123%2Fsubscribed_apps/unpause",
            "/v25.0/123%2Fsubscribed_apps/compare",
            "/v25.0/456%2Fphone_numbers/message_templates",
        ]
    );
    assert_eq!(t.remaining(), 0);

    // `..` would be resolved away by URL normalization: refused locally.
    let e = client(&t)
        .templates("..")
        .list(&TemplateListQuery::new())
        .await
        .unwrap_err();
    assert!(matches!(e, wa_core::Error::Validation(_)), "{e}");
    let e = client(&t)
        .authentication("..")
        .previews(&crate::authentication::PreviewQuery::default())
        .await
        .unwrap_err();
    assert!(matches!(e, wa_core::Error::Validation(_)), "{e}");
    assert_eq!(t.requests().len(), 6, "refused before the transport");
}

#[test]
fn id_lists_quote_non_numeric_ids() {
    assert_eq!(
        id_list(&[TemplateId::new("12"), TemplateId::new("34")]),
        "[12,34]"
    );
    assert_eq!(id_list(&[TemplateId::new("a\"b")]), r#"["a\"b"]"#);
}

#[test]
fn module_doc_example_is_valid() {
    // Mirrors the `no_run` example in the module docs, which compiles but
    // never executes there.
    let definition = TemplateDefinition::new("order_update", "en_US", TemplateCategory::Utility)
        .component(TemplateComponent::body_positional(
            "Hi {{1}}, order {{2}} has shipped.",
            ["Pablo", "860198"],
        ))
        .component(TemplateComponent::buttons([Button::url_with_example(
            "Track",
            "https://shop.example/track/{{1}}",
            "860198",
        )]));
    definition.validate().unwrap();
    let message = TemplateMessage::new("order_update", "en_US")
        .body([Parameter::text("Jessica"), Parameter::text("SKBUP2")])
        .url_button(0, "SKBUP2");
    message.validate().unwrap();
}

#[test]
fn template_info_tolerates_nulls_empties_and_new_fields() {
    let info: TemplateInfo = serde_json::from_value(json!({
        "id": "1",
        "components": null,
        "quality_score": {"date": 1758754645},
        "correct_category": "",
        "previous_category": "UTILITY",
        "status": "SOMETHING_NEW",
        "brand_new_field": {"x": 1}
    }))
    .unwrap();
    assert!(info.components.is_empty());
    assert_eq!(info.quality_score.unwrap().score, None);
    assert_eq!(info.correct_category, None);
    assert_eq!(info.previous_category, Some(TemplateCategory::Utility));
    assert_eq!(
        info.status,
        Some(TemplateStatus::Other("SOMETHING_NEW".into()))
    );

    let lib: LibraryTemplate = serde_json::from_value(json!({
        "id": "7", "name": "x", "industry": null, "buttons": null
    }))
    .unwrap();
    assert!(lib.industry.is_empty() && lib.buttons.is_empty());
}
