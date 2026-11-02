//! The send union's other types (roadmap M5a; docs/design/server.md,
//! section 4.3): `pin`, Direct Send, and the `interactive` types
//! `location_request_message`, `request_contact_info`,
//! `call_permission_request`, `voice_call`, `flow`, `address_message`,
//! `carousel` (media and product cards), `product`, `product_list` and
//! `catalog_message`, through `POST /v1/numbers/{pn}/messages` and the
//! inbox reply. Each type's example is sent as Meta's page prints it, with
//! the tenant's token; a key the library would drop is refused on its
//! path before any request; a Flow or a catalog named by id is checked to
//! be the number's WABA's (the design's M5.2); tenant B's key on A's
//! number is `404` with no vault read (M5.1); each type meets the 24-hour
//! window as Meta and the library say.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test crate: a panic is the report

mod common;

use common::{Call, Harness, Reply};
use meta_whatsapp_rs::core::error::TransportError;
use meta_whatsapp_rs::webhooks::axum::http::{Method, StatusCode};
use meta_whatsapp_server::model::Scope;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};

const TENANT: &str = "merchant-42";
const WABA: &str = "102290129340398";
const PN: &str = "106540352242922";
const TOKEN: &str = "EAAG-merchant-token";
/// The operator's own store: a tenant of the platform's WABA.
const OPERATOR: &str = "platform-store";
const OPERATOR_WABA: &str = "102290129340399";
const OPERATOR_PN: &str = "106540352242923";
const OPERATOR_TOKEN: &str = "EAAG-operator-system-user-token";
/// The examples' customer, as E.164 (`messages/send-messages`).
const TO: &str = "+16505551234";
/// `business-scoped-user-ids`' example BSUID.
const BSUID: &str = "US.13491208655302741918";
/// `groups/groups-messaging`' example group.
const GROUP: &str = "Y2FwaV9ncm91cDoxOTUwNTU1MDA3OToxMjAzNjMzOTQzMjAdOTY0MTUZD";
/// The catalog connected to the WABA (the pages print `CATALOG_ID`).
const CATALOG: &str = "123456789";
/// A Flow of the WABA (`flows/guides/sendingaflow` prints `<FLOW_ID>`).
const FLOW: &str = "1234567890123456";
const BODY: &str =
    "Thanks for your order! Tell us what address you'd like this order delivered to.";
const CAROUSEL_BODY: &str = "Of course! Here are three of our latest arrivals, each under $25:";

async fn connected() -> (Harness, String) {
    let h = Harness::new();
    h.tenant(TENANT).await;
    h.connect(TENANT, WABA, &[PN], TOKEN).await;
    let key = h.tenant_key(TENANT, &[Scope::Send]).await;
    (h, key)
}

fn refused(reply: &Reply) -> (StatusCode, String, Option<String>) {
    let error = reply.json()["error"].clone();
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
        "contacts": [{"input": TO, "wa_id": "16505551234"}],
        "messages": [{"id": "wamid.HBgLMTY0NjcwNDM1OTUVAgARGBI1RjQyNUE3NEYxMzAzMzQ5MkEA"}]
    })
}

/// `GET /{flow_id}`'s answer for a Flow of `waba` (the page's example is
/// empty; built from its field table, as the library's test is).
fn flow_of(waba: &str) -> Value {
    json!({
        "id": FLOW,
        "name": "appointment_booking_v1",
        "status": "PUBLISHED",
        "categories": ["APPOINTMENT_BOOKING"],
        "validation_errors": [],
        "json_version": "6.0",
        "whatsapp_business_account": {"id": waba},
        "application": {"id": "1234567890", "name": "My app"}
    })
}

/// `GET /{waba_id}/product_catalogs`' answer with the connected catalog
/// (field names from the reference's product-catalog fields).
fn catalogs_with(catalog: &str) -> Value {
    json!({"data": [{"id": catalog, "name": "Lucky Shrub"}]})
}

/// What a test sends and what Meta must receive.
struct Example {
    name: &'static str,
    /// The request's `to`.
    to: Value,
    /// The Send API's recipient fields.
    recipient: Value,
    /// The request's `type`, and the key of its object.
    kind: &'static str,
    /// The object, as Meta's page prints it.
    object: Value,
    /// Direct Send's keys, as the page prints them.
    envelope: Value,
    /// What Meta is asked before the send (a Flow, the catalogs), and its
    /// answer.
    check: Option<(String, Value)>,
}

impl Example {
    fn interactive(name: &'static str, object: Value) -> Self {
        Self {
            name,
            to: json!({"phone": TO}),
            recipient: json!({"recipient_type": "individual", "to": TO}),
            kind: "interactive",
            object,
            envelope: json!({}),
            check: None,
        }
    }

    fn checked(mut self, path: String, answer: Value) -> Self {
        self.check = Some((path, answer));
        self
    }

    fn body(&self) -> Value {
        let mut body = json!({"to": self.to, "type": self.kind});
        body[self.kind] = self.object.clone();
        for (k, v) in self.envelope.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    }

    fn wire(&self) -> Value {
        let mut wire = json!({"messaging_product": "whatsapp", "type": self.kind});
        wire[self.kind] = self.object.clone();
        for source in [&self.recipient, &self.envelope] {
            for (k, v) in source.as_object().unwrap() {
                wire[k] = v.clone();
            }
        }
        wire
    }
}

fn media_card(i: usize, slug: &str, name: &str, text: &str, action: &Value) -> Value {
    json!({
        "card_index": i,
        "type": "cta_url",
        "header": {"type": "image", "image": {"link": format!("https://www.luckyshrub.com/assets/{slug}.jpeg")}},
        "body": {"text": format!("*{name}*\n\n{text}")},
        "action": action
    })
}

const CARDS: [(&str, &str, &str); 3] = [
    (
        "blue-echeveria",
        "Blue Echeveria",
        "A rosette-shaped succulent with powdery blue leaves, perfect for brightening up any space.",
    ),
    (
        "zebra-haworthia",
        "Zebra Haworthia",
        "Striking white stripes on deep green leaves give this compact succulent a bold, modern look.",
    ),
    (
        "panda-plant",
        "Panda Plant",
        "Soft, fuzzy leaves with chocolate-brown edges—adorable and easy to care for.",
    ),
];

fn url_carousel() -> Value {
    let cards: Vec<Value> = CARDS
        .iter()
        .enumerate()
        .map(|(i, (slug, name, text))| {
            media_card(
                i,
                slug,
                name,
                text,
                &json!({"name": "cta_url", "parameters": {
                    "display_text": "Buy now",
                    "url": format!("https://shop.luckyshrub.com/latest/{slug}")
                }}),
            )
        })
        .collect();
    json!({"type": "carousel", "body": {"text": CAROUSEL_BODY}, "action": {"cards": cards}})
}

fn quick_reply_carousel() -> Value {
    let cards: Vec<Value> = CARDS
        .iter()
        .enumerate()
        .map(|(i, (slug, name, text))| {
            media_card(
                i,
                slug,
                name,
                text,
                &json!({"buttons": [
                    {"type": "quick_reply", "quick_reply": {"id": format!("learn-{slug}"), "title": "Learn more"}},
                    {"type": "quick_reply", "quick_reply": {"id": format!("fav-{slug}"), "title": "Add to favorites"}}
                ]}),
            )
        })
        .collect();
    json!({"type": "carousel", "body": {"text": CAROUSEL_BODY}, "action": {"cards": cards}})
}

/// `flows/guides/sendingaflow`, "Cloud API Sample Request (with all
/// parameters)", `flow_name` variant.
fn flow_by_name() -> Value {
    json!({
        "type": "flow",
        "header": {"type": "text", "text": "Flow message header"},
        "body": {"text": "Flow message body"},
        "footer": {"text": "Flow message footer"},
        "action": {"name": "flow", "parameters": {
            "flow_message_version": "3",
            "flow_token": "AQAAAAACS5FpgQ_cAAAAAD0QI3s.",
            "flow_name": "appointment_booking_v1",
            "flow_cta": "Book!",
            "flow_action": "navigate",
            "flow_action_payload": {
                "screen": "<SCREEN_NAME>",
                "data": "{\"product_name\":\"name\",\"product_description\":\"description\",\"product_price\":100}"
            }
        }}
    })
}

/// The same page's `flow_id` variant, its draft, opened by data exchange.
fn flow_by_id() -> Value {
    json!({
        "type": "flow",
        "body": {"text": "Flow message body"},
        "action": {"name": "flow", "parameters": {
            "flow_message_version": "3",
            "flow_id": FLOW,
            "flow_cta": "Go",
            "mode": "draft",
            "flow_action": "data_exchange"
        }}
    })
}

/// `catalogs/single-product-messages`, step 1 (the catalog made real).
fn single_product() -> Value {
    json!({"type": "product", "body": {"text": "BODY_TEXT"}, "footer": {"text": "FOOTER_TEXT"},
           "action": {"catalog_id": CATALOG, "product_retailer_id": "ID_TEST_ITEM_1"}})
}

/// Every type of roadmap M5a, as its page prints it.
#[allow(clippy::too_many_lines)] // one example per type, read top to bottom
fn examples() -> Vec<Example> {
    // The catalog check: the WABA's connected catalogs.
    let catalog_path = format!("/v25.0/{WABA}/product_catalogs");
    let catalog_answer = catalogs_with(CATALOG);
    vec![
        // groups/groups-messaging, the pin request (its table's samples).
        Example {
            name: "pin",
            to: json!({"group_id": GROUP}),
            recipient: json!({"recipient_type": "group", "to": GROUP}),
            kind: "pin",
            object: json!({"type": "pin", "message_id": "wamid.HBgLM", "expiration_days": 4}),
            envelope: json!({}),
            check: None,
        },
        Example {
            name: "unpin",
            to: json!({"group_id": GROUP}),
            recipient: json!({"recipient_type": "group", "to": GROUP}),
            kind: "pin",
            object: json!({"type": "unpin", "message_id": "wamid.HBgLM"}),
            envelope: json!({}),
            check: None,
        },
        // business-scoped-user-ids, "Using interactive messages".
        Example {
            to: json!({"user_id": BSUID}),
            recipient: json!({"recipient_type": "individual", "recipient": BSUID}),
            ..Example::interactive(
                "request_contact_info",
                json!({"type": "request_contact_info", "body": {"text": "<BODY_TEXT>"},
                       "action": {"name": "request_contact_info"}}),
            )
        },
        // direct-send/send-utility-and-authentication-messages,
        // configure-message-ttl and business-named-templates.
        Example {
            name: "direct send utility",
            kind: "text",
            object: json!({"body": "<BODY_TEXT>"}),
            envelope: json!({"category": "utility"}),
            ..Example::interactive("", json!({}))
        },
        Example {
            name: "direct send ttl",
            kind: "text",
            object: json!({"body": "<BODY_TEXT>"}),
            envelope: json!({"category": "utility", "ttl_seconds": 600}),
            ..Example::interactive("", json!({}))
        },
        Example {
            name: "direct send business-named template",
            kind: "text",
            object: json!({"body": "Hi Jane, your order #12345 has been shipped and is expected to arrive on March 20."}),
            envelope: json!({"category": "utility",
                             "direct_send_config": {"template_name": "order_shipment_update"}}),
            ..Example::interactive("", json!({}))
        },
        // messages/interactive-media-carousel-messages, both examples.
        Example::interactive("media carousel, URL buttons", url_carousel()),
        Example::interactive("media carousel, quick replies", quick_reply_carousel()),
        // catalogs/interactive-product-carousel-messages.
        Example::interactive(
            "product carousel",
            json!({"type": "carousel", "body": {"text": "Check out our featured products!"},
                   "action": {"cards": [
                       {"card_index": 0, "type": "product",
                        "action": {"product_retailer_id": "abc123xyz", "catalog_id": CATALOG}},
                       {"card_index": 1, "type": "product",
                        "action": {"product_retailer_id": "def456uvw", "catalog_id": CATALOG}}
                   ]}}),
        )
        .checked(catalog_path.clone(), catalog_answer.clone()),
        // calling/call-button-messages-deep-links, "Request body".
        Example {
            to: json!({"phone": TO, "user_id": BSUID}),
            recipient: json!({"recipient_type": "individual", "to": TO, "recipient": BSUID}),
            ..Example::interactive(
                "voice_call",
                json!({"type": "voice_call",
                       "body": {"text": "You can call us on WhatsApp now for faster service!"},
                       "action": {"name": "voice_call", "parameters": {
                           "display_text": "Call on WhatsApp", "ttl_minutes": 100,
                           "payload": "payload data"}}}),
            )
        },
        // messages/location-request-messages, "Example request".
        Example::interactive(
            "location_request_message",
            json!({"type": "location_request_message",
                   "body": {"text": "Let's start with your pickup. You can either manually *enter an address* or *share your current location*."},
                   "action": {"name": "send_location"}}),
        ),
        // messages/address-messages, saved addresses and validation errors.
        Example::interactive(
            "address_message, saved addresses",
            json!({"type": "address_message", "body": {"text": BODY},
                   "action": {"name": "address_message", "parameters": {
                       "country": "IN",
                       "saved_addresses": [{"id": "address1", "value": {
                           "name": "<CUSTOMER_NAME>", "phone_number": "+91xxxxxxxxxx",
                           "in_pin_code": "400063", "floor_number": "8", "building_name": "",
                           "address": "Wing A, Cello Triumph,IB Patel Rd",
                           "landmark_area": "Goregaon", "city": "Mumbai"}}]}}}),
        ),
        Example::interactive(
            "address_message, validation errors",
            json!({"type": "address_message", "body": {"text": BODY},
                   "action": {"name": "address_message", "parameters": {
                       "country": "IN",
                       "values": {"name": "CUSTOMER_NAME", "phone_number": "+91xxxxxxxxxx",
                                  "in_pin_code": "666666", "address": "Some other location",
                                  "city": "Delhi"},
                       "validation_errors": {"in_pin_code": "We could not locate this pin code."}}}}),
        ),
        // calling/user-call-permissions, "Send free form call permission
        // request message".
        Example {
            to: json!({"phone": "+17863476655", "user_id": BSUID}),
            recipient: json!({"recipient_type": "individual", "to": "+17863476655",
                              "recipient": BSUID}),
            ..Example::interactive(
                "call_permission_request",
                json!({"type": "call_permission_request",
                       "action": {"name": "call_permission_request"},
                       "body": {"text": "We would like to call you to help support your query on Order No: ON-12853."}}),
            )
        },
        Example::interactive("flow, by name", flow_by_name()),
        Example::interactive("flow, by id", flow_by_id())
            .checked(format!("/v25.0/{FLOW}"), flow_of(WABA)),
        // catalogs/single-product-messages, multi-product-messages and
        // catalog-messages.
        Example::interactive("product", single_product())
            .checked(catalog_path.clone(), catalog_answer.clone()),
        Example::interactive(
            "product_list",
            json!({"type": "product_list",
                   "header": {"type": "text", "text": "HEADER_CONTENT"},
                   "body": {"text": "BODY_CONTENT"},
                   "footer": {"text": "FOOTER_CONTENT"},
                   "action": {"catalog_id": CATALOG, "sections": [
                       {"title": "SECTION_TITLE", "product_items": [
                           {"product_retailer_id": "PRODUCT-SKU"}, {"product_retailer_id": "PRODUCT-SKU"}]},
                       {"title": "SECTION_TITLE", "product_items": [
                           {"product_retailer_id": "PRODUCT-SKU"}, {"product_retailer_id": "PRODUCT-SKU"}]}
                   ]}}),
        )
        .checked(catalog_path, catalog_answer),
        Example::interactive(
            "catalog_message",
            json!({"type": "catalog_message",
                   "body": {"text": "Hello! Thanks for your interest. Ordering is easy. Just visit our catalog and add items to purchase."},
                   "action": {"name": "catalog_message", "parameters": {"thumbnail_product_retailer_id": "2lc20305pt"}},
                   "footer": {"text": "Best grocery deals on WhatsApp!"}}),
        ),
    ]
}

/// M5a's decisive case: every type's example is sent as its page prints
/// it, with the tenant's merchant token from the vault, never the
/// operator's (whose own WABA token is in the same vault); a Flow or a
/// catalog named by id is asked of Meta first, with the same token.
/// Decisive: each type in the send union (out of it, its example is
/// `422 unsupported_message_type`, or `invalid_request` for a Direct Send
/// key, and nothing reaches Meta).
#[tokio::test]
async fn each_types_example_is_sent_as_its_page_prints_it_with_the_tenants_token() {
    let (h, key) = connected().await;
    h.tenant(OPERATOR).await;
    h.connect(OPERATOR, OPERATOR_WABA, &[OPERATOR_PN], OPERATOR_TOKEN)
        .await;
    let examples = examples();
    for example in &examples {
        let before = h.graph.requests().len();
        if let Some((_, answer)) = &example.check {
            h.graph.push_json(200, answer.clone());
        }
        h.graph.push_json(200, accepted());
        let reply = h.call(send(PN, &key, &example.body())).await;
        assert_eq!(
            reply.status,
            StatusCode::ACCEPTED,
            "{}: {}",
            example.name,
            reply.text
        );
        let requests = &h.graph.requests()[before..];
        let (checked, sent) = if let Some((path, _)) = &example.check {
            assert_eq!(requests.len(), 2, "{}", example.name);
            assert_eq!(requests[0].method, Method::GET, "{}", example.name);
            assert_eq!(requests[0].path(), *path, "{}", example.name);
            (Some(&requests[0]), &requests[1])
        } else {
            assert_eq!(requests.len(), 1, "{}", example.name);
            (None, &requests[0])
        };
        for request in checked.into_iter().chain([sent]) {
            assert_eq!(request.bearer(), Some(TOKEN), "{}", example.name);
        }
        assert_eq!(sent.method, Method::POST);
        assert_eq!(sent.path(), format!("/v25.0/{PN}/messages"));
        assert_eq!(sent.json().unwrap(), example.wire(), "{}", example.name);
    }
    // The control: the operator's own number sends with its own token.
    h.graph.push_json(200, accepted());
    let own = h
        .call(send(
            OPERATOR_PN,
            &key_of(&h, OPERATOR).await,
            &examples[2].body(),
        ))
        .await;
    assert_eq!(own.status, StatusCode::ACCEPTED, "{}", own.text);
    assert_eq!(
        h.graph.last_request().unwrap().bearer(),
        Some(OPERATOR_TOKEN)
    );
    assert_eq!(h.graph.remaining(), 0);
}

async fn key_of(h: &Harness, tenant: &str) -> String {
    h.tenant_key(tenant, &[Scope::Send]).await
}

/// A key the library would drop is refused on its path, and nothing is
/// sent: a header or a footer the type has not, a key of another type's
/// action, a key the library does not know in a parameters object, a
/// card's media by `id`, an `unpin`'s `expiration_days`. And the values
/// the library writes itself are checked: the action's `name`, a card's
/// `card_index` and `type`, the Flow's `flow_message_version`. Decisive:
/// the comparison with what the library sends
/// (`interactive_compared`, `pin`), and each value check.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one case per check, read top to bottom
async fn a_key_the_library_would_drop_is_refused_on_its_path_before_any_request() {
    let (h, key) = connected().await;
    let interactive =
        |object: Value| json!({"to": {"phone": TO}, "type": "interactive", "interactive": object});
    let edit = |object: Value, f: &dyn Fn(&mut Value)| {
        let mut object = object;
        f(&mut object);
        interactive(object)
    };
    let location = json!({"type": "location_request_message", "body": {"text": "Where?"},
                          "action": {"name": "send_location"}});
    let call = json!({"type": "voice_call", "body": {"text": "Call us"},
                      "action": {"name": "voice_call", "parameters": {"ttl_minutes": 100}}});
    let cases: Vec<(Value, &str)> = vec![
        (
            edit(location.clone(), &|o| o["footer"] = json!({"text": "f"})),
            "interactive.footer",
        ),
        (
            edit(location.clone(), &|o| {
                o["header"] = json!({"type": "text", "text": "h"});
            }),
            "interactive.header",
        ),
        (
            edit(location.clone(), &|o| {
                o["action"]["buttons"] =
                    json!([{"type": "reply", "reply": {"id": "a", "title": "A"}}]);
            }),
            "interactive.action.buttons",
        ),
        (
            edit(location.clone(), &|o| {
                o["action"]["name"] = json!("send_location_now");
            }),
            "interactive.action.name",
        ),
        (
            edit(call.clone(), &|o| {
                o["action"]["parameters"]["ttl_minute"] = json!(5);
            }),
            "interactive.action.parameters.ttl_minute",
        ),
        (
            edit(call.clone(), &|o| {
                o["action"]["parameters"]["ttl_minutes"] = json!(43_201);
            }),
            "interactive.action.parameters.ttl_minutes",
        ),
        (
            edit(call, &|o| o["footer"] = json!({"text": "f"})),
            "interactive.footer",
        ),
        (
            edit(url_carousel(), &|o| o["footer"] = json!({"text": "f"})),
            "interactive.footer",
        ),
        (
            edit(url_carousel(), &|o| {
                o["action"]["cards"][1]["header"]["image"] = json!({"id": "1479537139650973"});
            }),
            "interactive.action.cards[1].header.image.link",
        ),
        (
            edit(url_carousel(), &|o| {
                o["action"]["cards"][1]["header"]["image"]["id"] = json!("1479537139650973");
            }),
            "interactive.action.cards[1].header.image.id",
        ),
        (
            edit(url_carousel(), &|o| {
                o["action"]["cards"][2]["card_index"] = json!(1);
            }),
            "interactive.action.cards[2].card_index",
        ),
        (
            edit(url_carousel(), &|o| {
                o["action"]["cards"][2]["type"] = json!("product");
            }),
            "interactive.action.cards[2].type",
        ),
        (
            edit(quick_reply_carousel(), &|o| {
                o["action"]["cards"][0]["action"]["name"] = json!("cta_url");
            }),
            "interactive.action.cards[0].action.name",
        ),
        (
            edit(url_carousel(), &|o| {
                o["action"]["cards"][0]["header"]["image"]["link"] =
                    json!("http://www.luckyshrub.com/a.jpeg");
            }),
            "interactive.action.cards[0].header.image.link",
        ),
        (
            edit(flow_by_name(), &|o| {
                o["action"]["parameters"]["flow_tokn"] = json!("x");
            }),
            "interactive.action.parameters.flow_tokn",
        ),
        (
            edit(flow_by_name(), &|o| {
                o["action"]["parameters"]["flow_message_version"] = json!("2");
            }),
            "interactive.action.parameters.flow_message_version",
        ),
        (
            edit(flow_by_name(), &|o| {
                o["action"]["parameters"]["flow_id"] = json!(FLOW);
            }),
            "interactive.action.parameters.flow_name",
        ),
        (
            edit(flow_by_id(), &|o| {
                o["action"]["parameters"]["flow_id"] = json!("../me");
            }),
            "interactive.action.parameters.flow_id",
        ),
        (
            edit(flow_by_name(), &|o| {
                o["header"] = json!({"type": "image", "image": {"link": "https://www.luckyshrub.com/a.png",
                                                                 "filename": "a.png"}});
            }),
            "interactive.header.image.filename",
        ),
        (
            edit(single_product(), &|o| {
                o["header"] = json!({"type": "text", "text": "h"});
            }),
            "interactive.header",
        ),
        (
            edit(single_product(), &|o| {
                o["action"]["name"] = json!("product");
            }),
            "interactive.action.name",
        ),
        (
            interactive(json!({"type": "address_message", "body": {"text": BODY},
                "action": {"name": "address_message", "parameters": {"country": "IN",
                "saved_addresses": [{"id": "a", "value": {"city": "Mumbai"}, "label": "Home"}]}}})),
            "interactive.action.parameters.saved_addresses[0].label",
        ),
        (
            interactive(json!({"type": "catalog_message", "body": {"text": "b"},
                "action": {"name": "catalog_message", "parameters": {
                    "thumbnail_product_retailer_id": "2lc20305pt", "thumbnail": "x"}}})),
            "interactive.action.parameters.thumbnail",
        ),
        (
            interactive(
                json!({"type": "call_permission_request", "header": {"type": "text", "text": "h"},
                "action": {"name": "call_permission_request"}}),
            ),
            "interactive.header",
        ),
        (
            interactive(json!({"type": "product_list", "body": {"text": "b"},
                "header": {"type": "image", "image": {"link": "https://x.example/a.png"}},
                "action": {"catalog_id": CATALOG, "sections": [{"product_items": [{"product_retailer_id": "s"}]}]}})),
            "interactive.header",
        ),
        (
            interactive(json!({"type": "product_list", "body": {"text": "b"},
                "header": {"type": "text", "text": "h"},
                "action": {"catalog_id": CATALOG, "sections": [{"title": "t", "rows": [{"id": "r", "title": "R"}]}]}})),
            "interactive.action.sections[0]",
        ),
        // A key the types before M5a never had is refused on them too.
        (
            interactive(json!({"type": "button", "body": {"text": "b"},
                "action": {"buttons": [{"type": "reply", "reply": {"id": "a", "title": "A"}}],
                           "catalog_id": CATALOG}})),
            "interactive.action.catalog_id",
        ),
        (
            json!({"to": {"group_id": GROUP}, "type": "pin",
                   "pin": {"type": "unpin", "message_id": "wamid.HBgLM", "expiration_days": 4}}),
            "pin.expiration_days",
        ),
        (
            json!({"to": {"group_id": GROUP}, "type": "pin",
                   "pin": {"type": "pin", "message_id": "wamid.HBgLM"}}),
            "pin.expiration_days",
        ),
        (
            json!({"to": {"phone": TO}, "type": "pin",
                   "pin": {"type": "pin", "message_id": "wamid.HBgLM", "expiration_days": 4}}),
            "to",
        ),
    ];
    for (body, field) in cases {
        let reply = h.call(send(PN, &key, &body)).await;
        assert_eq!(
            refused(&reply),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request".to_owned(),
                Some(field.to_owned())
            ),
            "{body}: {}",
            reply.text
        );
        assert_eq!(reply.json()["error"]["may_have_been_sent"], false);
    }
    assert!(h.graph.requests().is_empty(), "nothing reached Meta");
}

/// Direct Send follows the library's rules, checked before any request: a
/// TTL or a business-named template only where Meta takes them, the
/// documented ranges, the name's alphabet, no `authentication` message to
/// a BSUID alone, and only Meta's three categories (an unknown one Meta
/// refuses with `100`). Decisive: the library's checks reached through
/// the request's fields, and the closed category.
#[tokio::test]
async fn direct_send_is_checked_by_the_librarys_rules_before_any_request() {
    let (h, key) = connected().await;
    let text = |to: Value, envelope: Value| {
        let mut body = json!({"to": to, "type": "text", "text": {"body": "<BODY_TEXT>"}});
        for (k, v) in envelope.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };
    let phone = json!({"phone": TO});
    for (body, field) in [
        (
            text(phone.clone(), json!({"ttl_seconds": 600})),
            "ttl_seconds",
        ),
        (
            text(
                phone.clone(),
                json!({"category": "service", "ttl_seconds": 600}),
            ),
            "ttl_seconds",
        ),
        (
            text(
                phone.clone(),
                json!({"category": "utility", "ttl_seconds": 43_201}),
            ),
            "ttl_seconds",
        ),
        (
            text(
                phone.clone(),
                json!({"category": "authentication", "ttl_seconds": 901}),
            ),
            "ttl_seconds",
        ),
        (
            text(
                phone.clone(),
                json!({"category": "authentication",
                                       "direct_send_config": {"template_name": "x"}}),
            ),
            "direct_send_config",
        ),
        (
            text(
                phone.clone(),
                json!({"category": "utility",
                                       "direct_send_config": {"template_name": "Order-Update"}}),
            ),
            "direct_send_config.template_name",
        ),
        (
            text(
                json!({"user_id": BSUID}),
                json!({"category": "authentication"}),
            ),
            "to",
        ),
        (
            json!({"to": phone, "type": "text", "text": {"body": "x".repeat(1025)},
                   "category": "utility"}),
            "text.body",
        ),
        (
            text(phone.clone(), json!({"category": "marketing"})),
            "body",
        ),
    ] {
        let reply = h.call(send(PN, &key, &body)).await;
        assert_eq!(
            refused(&reply),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request".to_owned(),
                Some(field.to_owned())
            ),
            "{body}: {}",
            reply.text
        );
    }
    assert!(h.graph.requests().is_empty(), "nothing reached Meta");
    // An authentication message to a BSUID with a phone number goes.
    h.graph.push_json(200, accepted());
    let both = h
        .call(send(
            PN,
            &key,
            &text(
                json!({"phone": TO, "user_id": BSUID}),
                json!({"category": "authentication"}),
            ),
        ))
        .await;
    assert_eq!(both.status, StatusCode::ACCEPTED, "{}", both.text);
    assert_eq!(h.graph.remaining(), 0);
}

/// The design's M5.2 for the send union: a Flow or a catalog a message
/// names by id is the number's WABA's, asked of Meta with the tenant's
/// token before anything is sent; another WABA's (a token can reach other
/// tenants' WABAs) is `404 not_found` and nothing is sent, as is an id
/// Meta refuses. A check Meta fails was not sent: `may_have_been_sent`
/// false, the `Idempotency-Key` released. Decisive:
/// `messages::admit_named_object` in `Messages::send`.
#[tokio::test]
async fn a_flow_or_a_catalog_of_another_waba_is_not_found_and_nothing_is_sent() {
    let (h, key) = connected().await;
    let flow = json!({"to": {"phone": TO}, "type": "interactive", "interactive": flow_by_id()});
    let product =
        json!({"to": {"phone": TO}, "type": "interactive", "interactive": single_product()});
    // Another tenant's WABA's Flow; a WABA with no catalog (the
    // reference's example), and one with another catalog.
    h.graph.push_json(200, flow_of(OPERATOR_WABA));
    h.graph.push_json(200, json!({"data": []}));
    h.graph.push_json(200, catalogs_with("987654321"));
    // Meta refusing the Flow's id for this token.
    h.graph.push_json(
        400,
        json!({"error": {"message": "Unsupported get request. Object with ID '1234567890123456' does not exist, cannot be loaded due to missing permissions, or does not support this operation.",
                         "type": "GraphMethodException", "code": 100, "error_subcode": 33,
                         "fbtrace_id": "AbCdEf"}}),
    );
    for body in [&flow, &product, &product, &flow] {
        let reply = h.call(send(PN, &key, body)).await;
        assert_eq!(
            refused(&reply),
            (StatusCode::NOT_FOUND, "not_found".to_owned(), None),
            "{}",
            reply.text
        );
        assert!(reply.json()["error"]["graph"].is_null(), "{}", reply.text);
    }
    let requests = h.graph.requests();
    assert_eq!(requests.len(), 4);
    assert!(
        requests.iter().all(|r| r.method == Method::GET),
        "nothing sent"
    );
    assert!(requests.iter().all(|r| r.bearer() == Some(TOKEN)));
    // A check cut by a timeout sent nothing, and frees its key.
    let keyed = || send(PN, &key, &flow).header("idempotency-key", "flow:checked-first");
    h.graph.push_error(|| TransportError::Timeout);
    let cut = h.call(keyed()).await;
    assert_eq!(cut.status, StatusCode::GATEWAY_TIMEOUT, "{}", cut.text);
    assert_eq!(cut.json()["error"]["may_have_been_sent"], false);
    h.graph.push_json(200, flow_of(WABA));
    h.graph.push_json(200, accepted());
    let sent = h.call(keyed()).await;
    assert_eq!(sent.status, StatusCode::ACCEPTED, "{}", sent.text);
    assert_eq!(h.graph.requests().len(), 7);
    assert_eq!(h.graph.remaining(), 0);
}

/// The design's M5.1 for the send union: tenant B's key sending any of
/// these types to tenant A's number is `404` before anything is read: no
/// vault read, nothing asked of Meta (M1.3's table covers the route; this
/// covers each type's body, the checked ones among them).
#[tokio::test]
async fn another_tenants_number_is_not_found_before_the_vault_whatever_the_type() {
    let (h, _) = connected().await;
    h.tenant("merchant-b").await;
    h.connect(
        "merchant-b",
        "102290129340400",
        &["106540352242924"],
        "EAAG-b",
    )
    .await;
    let b_key = h
        .tenant_key("merchant-b", &[Scope::Send, Scope::Inbox])
        .await;
    for example in examples() {
        let before = h.kv.vault_reads();
        let reply = h.call(send(PN, &b_key, &example.body())).await;
        assert_eq!(
            refused(&reply),
            (StatusCode::NOT_FOUND, "not_found".to_owned(), None),
            "{}: {}",
            example.name,
            reply.text
        );
        assert_eq!(h.kv.vault_reads(), before, "{}", example.name);
        let mut reply_body = example.body();
        reply_body.as_object_mut().unwrap().remove("to");
        let reply = h
            .call(
                Call::new(
                    Method::POST,
                    format!("/v1/numbers/{PN}/conversations/16505551234/messages"),
                )
                .key(&b_key)
                .json(&reply_body),
            )
            .await;
        assert_eq!(
            refused(&reply),
            (StatusCode::NOT_FOUND, "not_found".to_owned(), None),
            "{} (inbox): {}",
            example.name,
            reply.text
        );
        assert_eq!(h.kv.vault_reads(), before, "{} (inbox)", example.name);
    }
    assert!(h.graph.requests().is_empty(), "no call reached Meta");
}

/// Each type meets the 24-hour window as Meta and the library say, on the
/// inbox reply: within the window each is sent (a Flow named by id asked
/// of Meta first); outside it every free-form type is `409
/// customer_service_window_closed` with nothing asked of Meta, the Flow
/// check included, while a Direct Send `utility` or `authentication`
/// message, which Meta sends as a template, goes, and a `service` one does
/// not. A pin to a conversation that is not a group's is refused on
/// `contact`. Decisive: the window check before the named object's
/// (`Conversations::reply`), and the library's exemption of the two
/// categories.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one scenario, read top to bottom
async fn an_inbox_reply_meets_each_types_window_rule() {
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
    let reply = |body: &Value| {
        Call::new(
            Method::POST,
            format!("/v1/numbers/{EXAMPLE_PN}/conversations/{EXAMPLE_WA_ID}/messages"),
        )
        .key(&key)
        .json(body)
    };
    let free_form: Vec<Value> = examples()
        .into_iter()
        .filter(|e| e.kind == "interactive")
        .map(|e| json!({"type": "interactive", "interactive": e.object}))
        .collect();
    // Within the window: a Flow by id, its check first, with the tenant's
    // token; then a text by Direct Send `service`.
    h.graph.push_json(
        200,
        json!({"id": FLOW, "whatsapp_business_account": {"id": EXAMPLE_WABA}}),
    );
    h.graph.push_json(200, replied("wamid.FLOW-REPLY"));
    let flow = json!({"type": "interactive", "interactive": flow_by_id()});
    let sent = h.call(reply(&flow)).await;
    assert_eq!(sent.status, StatusCode::ACCEPTED, "{}", sent.text);
    let requests = h.graph.requests();
    assert_eq!(requests[0].path(), format!("/v25.0/{FLOW}"));
    assert_eq!(requests[1].bearer(), Some(INBOX_TOKEN));
    let mut expected = json!({"messaging_product": "whatsapp", "recipient_type": "individual",
                              "to": CONTACT, "type": "interactive"});
    expected["interactive"] = flow_by_id();
    assert_eq!(requests[1].json().unwrap(), expected);
    // Another WABA's Flow is not found on a reply too, nothing sent
    // (M5.2: `Conversations::reply`'s check).
    h.graph.push_json(
        200,
        json!({"id": FLOW, "whatsapp_business_account": {"id": OPERATOR_WABA}}),
    );
    let elsewhere = h.call(reply(&flow)).await;
    assert_eq!(
        refused(&elsewhere),
        (StatusCode::NOT_FOUND, "not_found".to_owned(), None),
        "{}",
        elsewhere.text
    );
    assert_eq!(h.graph.requests().len(), 3);
    assert_eq!(h.graph.last_request().unwrap().method, Method::GET);
    let service = json!({"type": "text", "text": {"body": "On its way"}, "category": "service"});
    h.graph.push_json(200, replied("wamid.SERVICE-REPLY"));
    let sent = h.call(reply(&service)).await;
    assert_eq!(sent.status, StatusCode::ACCEPTED, "{}", sent.text);
    let pin = json!({"type": "pin", "pin": {"type": "pin", "message_id": EXAMPLE_WAMID,
                                            "expiration_days": 4}});
    let not_a_group = h.call(reply(&pin)).await;
    assert_eq!(
        refused(&not_a_group),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request".to_owned(),
            Some("contact".to_owned())
        ),
        "{}",
        not_a_group.text
    );
    assert_eq!(h.graph.requests().len(), 4);
    // 25 hours on: closed for every free-form type, Direct Send `service`
    // included, with nothing asked of Meta.
    h.clock
        .set(time::OffsetDateTime::now_utc() + time::Duration::hours(25));
    for body in free_form.iter().chain([&service]) {
        let closed = h.call(reply(body)).await;
        assert_eq!(
            (closed.status, closed.code().as_str()),
            (StatusCode::CONFLICT, "customer_service_window_closed"),
            "{body}: {}",
            closed.text
        );
    }
    assert_eq!(h.graph.requests().len(), 4, "refused with zero requests");
    // A Direct Send `utility` or `authentication` message is a template to
    // Meta: it goes.
    for category in ["utility", "authentication"] {
        h.graph.push_json(200, replied("wamid.DIRECT-SEND"));
        let body = json!({"type": "text", "text": {"body": "<BODY_TEXT>"}, "category": category});
        let sent = h.call(reply(&body)).await;
        assert_eq!(
            sent.status,
            StatusCode::ACCEPTED,
            "{category}: {}",
            sent.text
        );
        let mut expected = body.clone();
        for (k, v) in [
            ("messaging_product", json!("whatsapp")),
            ("recipient_type", json!("individual")),
            ("to", json!(CONTACT)),
        ] {
            expected[k] = v;
        }
        assert_eq!(h.graph.last_request().unwrap().json().unwrap(), expected);
    }
    assert_eq!(h.graph.remaining(), 0);
}
