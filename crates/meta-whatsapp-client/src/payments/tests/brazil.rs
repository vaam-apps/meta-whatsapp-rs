//! Brazil (`payments/payments-br/*`): every example of the `orders`,
//! `offsite-pix`, `payment-links`, `boleto` and `one-click-payments`
//! pages, sent and compared as exact JSON, and the rules a `BRL` order is
//! checked by, next to the `INR` ones. Placeholders the examples leave
//! unfilled (`<PHONE_NUMBER>`, `<UNIQUE_REFERENCE_ID>`) are filled with a
//! plausible value; everything else is copied.

use pretty_assertions::assert_eq;

use super::*;

/// The examples' Pix code, copied (its `...` included).
const PIX_CODE: &str = "00020101021226700014br.gov.bcb.pix2548pix.example.com...";
/// The examples' Boleto digitable line.
const DIGITABLE_LINE: &str = "03399026944140000002628346101018898510000008848";

/// The examples' `order`: one cake, no tax.
fn brl_order() -> Order {
    Order::new(
        [OrderItem::new("Cake", Amount::brl(50000), 1).retailer_id("1234567")],
        Amount::brl(50000),
        DescribedAmount::new(Amount::brl(0)).description("optional text"),
    )
}

fn brl_order_json() -> Value {
    json!({
      "status": "pending",
      "tax": {"value": 0, "offset": 100, "description": "optional text"},
      "items": [{
        "retailer_id": "1234567",
        "name": "Cake",
        "amount": {"value": 50000, "offset": 100},
        "quantity": 1
      }],
      "subtotal": {"value": 50000, "offset": 100}
    })
}

/// The examples' full `order_details` message, paid through `setting`.
fn brl_full(reference_id: &str, setting: impl Into<PaymentSetting>) -> OrderDetails {
    OrderDetails::new(
        "Your message content",
        OrderDetailsParameters::brl(reference_id, GoodsType::Digital, setting, brl_order()),
    )
}

/// The examples' simplified `order_details` message (no `order`).
fn brl_simplified(reference_id: &str, setting: impl Into<PaymentSetting>) -> OrderDetails {
    OrderDetails::new(
        "Your message content",
        OrderDetailsParameters::brl_simplified(
            reference_id,
            GoodsType::Digital,
            setting,
            Amount::brl(50000),
        ),
    )
}

/// The examples' `interactive` object around `setting`, with `order` or
/// without (simplified).
fn brl_json(reference_id: &str, setting: &Value, order: Option<Value>) -> Value {
    let mut parameters = json!({
      "reference_id": reference_id,
      "type": "digital-goods",
      "payment_type": "br",
      "payment_settings": [setting],
      "currency": "BRL",
      "total_amount": {"value": 50000, "offset": 100}
    });
    if let Some(order) = order {
        parameters = with(parameters, json!({ "order": order }));
    }
    envelope(&json!({
      "type": "order_details",
      "body": {"text": "Your message content"},
      "action": {"name": "review_and_pay", "parameters": parameters}
    }))
}

/// Both examples of a page (full and simplified), sent.
#[track_caller]
fn check_both(reference_id: &str, setting: &PaymentSetting, setting_json: &Value) {
    assert_eq!(
        sent(brl_full(reference_id, setting.clone())),
        brl_json(reference_id, setting_json, Some(brl_order_json()))
    );
    assert_eq!(
        sent(brl_simplified(reference_id, setting.clone())),
        brl_json(reference_id, setting_json, None)
    );
}

fn pix() -> PixDynamicCode {
    PixDynamicCode::new(
        PIX_CODE,
        "Account holder name",
        "39580525000189",
        PixKeyType::Cnpj,
    )
}

// ─── order_details: the pages' examples ──────────────────────────────────

/// `payments-br/orders` ("Order details example", "Simplified order
/// details message") and `payments-br/payment-links` ("Payload example",
/// "Simplified payload example"): the same four bodies.
#[test]
fn payment_link_examples() {
    let setting = PaymentSetting::payment_link("https://my-payment-link-url");
    assert_eq!(
        brl_full("unique-reference-id", setting.clone())
            .parameters
            .total_amount,
        Amount::brl(50000)
    );
    check_both(
        "unique-reference-id",
        &setting,
        &json!({
          "type": "payment_link",
          "payment_link": {"uri": "https://my-payment-link-url"}
        }),
    );
}

/// `payments-br/offsite-pix`, "Payload example" and "Simplified payload
/// example".
#[test]
fn pix_examples() {
    check_both(
        "unique-reference-id",
        &pix().into(),
        &json!({
          "type": "pix_dynamic_code",
          "pix_dynamic_code": {
            "code": PIX_CODE,
            "merchant_name": "Account holder name",
            "key": "39580525000189",
            "key_type": "CNPJ"
          }
        }),
    );
}

/// `payments-br/offsite-pix`, "Select the payment experience": the
/// `payment_settings` of Pix with redirect.
#[test]
fn pix_with_redirect_example() {
    let msg = brl_simplified(
        "unique-reference-id",
        pix().flow_type(PixFlowType::AppSwitch),
    );
    let body = sent(msg);
    assert_eq!(
        body["interactive"]["action"]["parameters"]["payment_settings"],
        json!([
          {
            "type": "pix_dynamic_code",
            "pix_dynamic_code": {
              "code": PIX_CODE,
              "merchant_name": "Account holder name",
              "key": "39580525000189",
              "key_type": "CNPJ",
              "flow_type": "APPSWITCH"
            }
          }
        ])
    );
}

/// `payments-br/boleto`, "Payload example" and "Simplified payload
/// example".
#[test]
fn boleto_examples() {
    check_both(
        "unique-reference-id",
        &PaymentSetting::boleto(DIGITABLE_LINE),
        &json!({
          "type": "boleto",
          "boleto": {"digitable_line": DIGITABLE_LINE}
        }),
    );
}

/// `payments-br/one-click-payments`, "Payload example" and "Simplified
/// payload example" (`<UNIQUE_REFERENCE_ID>` filled in).
#[test]
fn one_click_examples() {
    check_both(
        "UNIQUE_REFERENCE_ID",
        &PaymentSetting::offsite_card_pay("5235", "1234567"),
        &json!({
          "type": "offsite_card_pay",
          "offsite_card_pay": {"last_four_digits": "5235", "credential_id": "1234567"}
        }),
    );
}

/// Every value of the Brazil tables, as written there.
#[test]
fn brazil_values_are_the_tables() {
    fn wire(v: impl serde::Serialize) -> Value {
        serde_json::to_value(v).unwrap()
    }
    assert_eq!(wire(Currency::Brl), json!("BRL"));
    assert_eq!(wire(&PaymentType::Br), json!("br"));
    for (kind, name) in [
        (PixKeyType::Cpf, "CPF"),
        (PixKeyType::Cnpj, "CNPJ"),
        (PixKeyType::Email, "EMAIL"),
        (PixKeyType::Phone, "PHONE"),
        (PixKeyType::Evp, "EVP"),
    ] {
        assert_eq!(wire(&kind), json!(name));
    }
    assert_eq!(wire(&PixFlowType::AppSwitch), json!("APPSWITCH"));
    assert_eq!(wire(&PixFlowType::CopyPaste), json!("COPYPASTE"));
    for (status, name) in [
        (PaymentUpdateStatus::Pending, "pending"),
        (PaymentUpdateStatus::Captured, "captured"),
        (PaymentUpdateStatus::Failed, "failed"),
    ] {
        assert_eq!(wire(&status), json!(name));
    }
    assert_eq!(Currency::Brl.offset(), 100);
    assert_eq!(Currency::Brl.reference_id_max(), 60);
    assert_eq!(Currency::Inr.reference_id_max(), 35);
    assert_eq!(Amount::brl(1234), Amount::new(1234, 100));
}

/// A BRL message is a message like any other: never replayed after a
/// timeout.
#[tokio::test]
async fn brl_order_details_is_never_replayed() {
    let t = ScriptedTransport::new();
    t.push_error(|| TransportError::Timeout);
    let msg = OutboundMessage::new(
        Recipient::phone(TO),
        brl_simplified("ref", PaymentSetting::boleto(DIGITABLE_LINE)),
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

// ─── order_details: Brazil's rules ───────────────────────────────────────

fn brl_valid() -> OrderDetails {
    brl_full("ref", pix()).footer("f")
}

/// Each edit of a valid BRL message is refused on `field`, before
/// sending.
#[track_caller]
fn check_brl_rules(cases: &[(Edit, &str)]) {
    assert_eq!(brl_valid().validate(), Ok(()));
    for (edit, field) in cases {
        let mut msg = brl_valid();
        edit(&mut msg);
        let got = refused(&msg);
        assert!(
            got == *field || got == format!("{P}.{field}"),
            "expected `{field}`, got `{got}`"
        );
    }
}

fn order_of(m: &mut OrderDetails) -> &mut Order {
    m.parameters.order.as_mut().unwrap()
}

#[test]
fn brl_message_rules_are_checked() {
    check_brl_rules(&[
        (|m| m.body = chars(1025), "interactive.body.text"),
        (|m| m.footer = Some(chars(61)), "interactive.footer.text"),
        (
            |m| m.parameters.reference_id = String::new(),
            "reference_id",
        ),
        (
            |m| m.parameters.reference_id = "x".repeat(61),
            "reference_id",
        ),
        // The alphabet is ASCII: a two-byte letter is refused, whatever
        // the length.
        (
            |m| m.parameters.reference_id = "pedidoé".into(),
            "reference_id",
        ),
        (|m| m.parameters.payment_type = None, "payment_type"),
        (
            |m| m.parameters.payment_type = Some(PaymentType::Upi),
            "payment_type",
        ),
        (
            |m| m.parameters.payment_type = Some(PaymentType::Other("BR".into())),
            "payment_type",
        ),
        (
            |m| m.parameters.total_amount = Amount::new(50000, 1000),
            "total_amount.offset",
        ),
        (
            |m| m.parameters.total_amount = Amount::brl(50001),
            "total_amount.value",
        ),
        (
            |m| {
                m.parameters.order = None;
                m.parameters.total_amount = Amount::brl(0);
            },
            "total_amount.value",
        ),
        // A simplified order takes no header.
        (
            |m| {
                m.parameters.order = None;
                m.header = Some(MediaSource::link("https://x/i.png"));
            },
            "interactive.header",
        ),
    ]);
}

/// India's fields are refused in a BRL order: no Brazil page documents
/// them.
#[test]
fn india_fields_are_refused_in_a_brl_order() {
    check_brl_rules(&[
        (
            |m| m.parameters.payment_configuration = Some("pc".into()),
            "payment_configuration",
        ),
        (
            |m| {
                m.parameters.beneficiaries = vec![Beneficiary::new("n", "l", "c", "s", "560001")];
            },
            "beneficiaries",
        ),
        (
            |m| m.parameters.payment_settings = vec![PaymentSetting::upi_intent_link("upi://pay")],
            "payment_settings[0].type",
        ),
        (
            |m| {
                m.parameters.payment_settings = vec![
                    PaymentGateway::new(GatewayProvider::Payu(PayuFields::default()), "pg").into(),
                ];
            },
            "payment_settings[0].type",
        ),
        (
            |m| {
                m.parameters.payment_settings = vec![
                    PaymentLink::new("https://x")
                        .success_url("https://x/ok")
                        .into(),
                ];
            },
            "payment_settings[0].payment_link.success_url",
        ),
        (
            |m| {
                m.parameters.payment_settings = vec![
                    PaymentLink::new("https://x")
                        .cancel_url("https://x/no")
                        .into(),
                ];
            },
            "payment_settings[0].payment_link.cancel_url",
        ),
        (|m| order_of(m).quick_pay = true, "order.type"),
        (
            |m| {
                let item = &mut order_of(m).items[0];
                item.image = Some(ItemImage {
                    link: "https://x/i.png".into(),
                });
            },
            "order.items[0].image",
        ),
        (
            |m| {
                let item = &mut order_of(m).items[0];
                *item = item
                    .clone()
                    .import("Brasil", "Importadora", importer_address());
            },
            "order.items[0].country_of_origin",
        ),
        (
            |m| order_of(m).items[0].importer_name = Some("Importadora".into()),
            "order.items[0].importer_name",
        ),
        (
            |m| order_of(m).items[0].importer_address = Some(importer_address()),
            "order.items[0].importer_address",
        ),
        // payments-br/orders, the image object: `link` "Required"; a
        // header by media id is India's (`payments-in/payment-links`).
        (
            |m| m.header = Some(MediaId::new(MEDIA).unwrap().into()),
            "interactive.header.image.id",
        ),
    ]);
}

/// The simplified order's header refusal is Brazil's: an INR message
/// without an order is refused for the order, header or not; a BRL one by
/// link with an order passes.
#[test]
fn the_header_rules_are_brazils_only() {
    let mut inr = valid().header_image(MediaSource::link("https://x/i.png"));
    inr.parameters.order = None;
    assert_eq!(refused(&inr), format!("{P}.order"));
    let inr = valid().header_image(MediaId::new(MEDIA).unwrap());
    assert_eq!(inr.validate(), Ok(()));
    let brl = brl_valid().header_image(MediaSource::link("https://x/i.png"));
    assert_eq!(brl.validate(), Ok(()));
}

#[test]
fn brl_order_rules_are_checked() {
    check_brl_rules(&[
        (|m| order_of(m).items.clear(), "order.items"),
        // payments-br/orders, the item table: `retailer_id` "Required".
        (
            |m| order_of(m).items[0].retailer_id = None,
            "order.items[0].retailer_id",
        ),
        (
            |m| order_of(m).items[0].retailer_id = Some(String::new()),
            "order.items[0].retailer_id",
        ),
        (
            |m| order_of(m).items[0].name = chars(61),
            "order.items[0].name",
        ),
        (
            |m| order_of(m).items[0].amount = Amount::new(50000, 10),
            "order.items[0].amount.offset",
        ),
        (
            |m| order_of(m).items[0].sale_amount = Some(Amount::new(40000, 10)),
            "order.items[0].sale_amount.offset",
        ),
        (
            |m| order_of(m).subtotal = Amount::new(50000, 1),
            "order.subtotal.offset",
        ),
        // The subtotal is the items' sum: here 50000 × 1, not 49999.
        (
            |m| {
                order_of(m).subtotal = Amount::brl(49999);
                m.parameters.total_amount = Amount::brl(49999);
            },
            "order.subtotal.value",
        ),
        // A sale price counts instead of the price.
        (
            |m| order_of(m).items[0].sale_amount = Some(Amount::brl(40000)),
            "order.subtotal.value",
        ),
        (
            |m| order_of(m).tax = DescribedAmount::new(Amount::brl(0)).description(chars(61)),
            "order.tax.description",
        ),
        (
            |m| {
                order_of(m).shipping =
                    Some(DescribedAmount::new(Amount::brl(0)).description(chars(61)));
            },
            "order.shipping.description",
        ),
        (
            |m| order_of(m).discount = Some(Discount::new(Amount::brl(0)).description(chars(61))),
            "order.discount.description",
        ),
        (
            |m| order_of(m).discount = Some(Discount::new(Amount::brl(0)).program_name(chars(61))),
            "order.discount.discount_program_name",
        ),
        (
            |m| order_of(m).expiration = Some(OrderExpiration::new(EXPIRES, chars(121))),
            "order.expiration.description",
        ),
    ]);
}

#[test]
fn brl_payment_setting_rules_are_checked() {
    fn set(m: &mut OrderDetails, s: impl Into<PaymentSetting>) {
        m.parameters.payment_settings = vec![s.into()];
    }
    check_brl_rules(&[
        (
            |m| {
                set(
                    m,
                    PixDynamicCode {
                        code: String::new(),
                        ..pix()
                    },
                );
            },
            "payment_settings[0].pix_dynamic_code.code",
        ),
        (
            |m| {
                set(
                    m,
                    PixDynamicCode {
                        merchant_name: String::new(),
                        ..pix()
                    },
                );
            },
            "payment_settings[0].pix_dynamic_code.merchant_name",
        ),
        (
            |m| {
                set(
                    m,
                    PixDynamicCode {
                        key: String::new(),
                        ..pix()
                    },
                );
            },
            "payment_settings[0].pix_dynamic_code.key",
        ),
        (
            |m| {
                set(
                    m,
                    PixDynamicCode {
                        key_type: PixKeyType::Other(String::new()),
                        ..pix()
                    },
                );
            },
            "payment_settings[0].pix_dynamic_code.key_type",
        ),
        (
            |m| set(m, pix().flow_type(PixFlowType::Other(String::new()))),
            "payment_settings[0].pix_dynamic_code.flow_type",
        ),
        (
            |m| set(m, PaymentSetting::payment_link("")),
            "payment_settings[0].payment_link.uri",
        ),
        (
            |m| set(m, PaymentSetting::boleto("")),
            "payment_settings[0].boleto.digitable_line",
        ),
        (
            |m| set(m, PaymentSetting::offsite_card_pay("5235", "")),
            "payment_settings[0].offsite_card_pay.credential_id",
        ),
        (
            |m| set(m, PaymentSetting::offsite_card_pay("523", "c")),
            "payment_settings[0].offsite_card_pay.last_four_digits",
        ),
        (
            |m| set(m, PaymentSetting::offsite_card_pay("52355", "c")),
            "payment_settings[0].offsite_card_pay.last_four_digits",
        ),
        (
            |m| set(m, PaymentSetting::offsite_card_pay("523a", "c")),
            "payment_settings[0].offsite_card_pay.last_four_digits",
        ),
        // Four characters, but full-width digits (three bytes each).
        (
            |m| set(m, PaymentSetting::offsite_card_pay("５２３５", "c")),
            "payment_settings[0].offsite_card_pay.last_four_digits",
        ),
    ]);
}

/// Every Brazil limit is inclusive and counted in characters (the tests
/// above refuse one more).
#[test]
fn every_brl_limit_is_inclusive_and_counts_characters() {
    let item = OrderItem::new(chars(60), Amount::brl(10000), 2)
        .retailer_id("SKU-1")
        .sale_amount(Amount::brl(9000));
    let order = Order::new(
        [item],
        Amount::brl(18000),
        DescribedAmount::new(Amount::brl(1000)).description(chars(60)),
    )
    .shipping(DescribedAmount::new(Amount::brl(500)).description(chars(60)))
    .discount(
        Discount::new(Amount::brl(1500))
            .description(chars(60))
            .program_name(chars(60)),
    )
    .expiration(OrderExpiration::new(EXPIRES, chars(120)));
    assert_eq!(order.items_sum(), Some(18000));
    let params = OrderDetailsParameters::brl("x".repeat(60), GoodsType::Physical, pix(), order)
        .payment_setting(PaymentSetting::payment_link("https://x"))
        .payment_setting(PaymentSetting::boleto(DIGITABLE_LINE))
        .payment_setting(PaymentSetting::offsite_card_pay("0000", "c"));
    assert_eq!(params.total_amount, Amount::brl(18000));
    let msg = OrderDetails::new(chars(1024), params)
        .footer(chars(60))
        .header_image(MediaSource::link("https://x/i.png"));
    assert_eq!(msg.validate(), Ok(()));
    // Every alphabet character, at 60.
    let mut msg = brl_valid();
    msg.parameters.reference_id = "Az09_-.".repeat(9)[..60].to_owned();
    assert_eq!(msg.validate(), Ok(()));
}

/// The payment settings are optional in Brazil (every page's table).
#[test]
fn a_brl_order_needs_no_payment_setting() {
    let mut msg = brl_valid();
    msg.parameters.payment_settings.clear();
    assert_eq!(msg.validate(), Ok(()));
    let body = serde_json::to_value(&msg).unwrap();
    assert!(
        body["action"]["parameters"]
            .get("payment_settings")
            .is_none()
    );
}

/// The subtotal sums in `u128`: lines beyond `u64::MAX` are `None`, not a
/// wrapped value.
#[test]
fn the_items_sum_never_overflows() {
    let max = Amount::brl(u64::MAX);
    let one = OrderItem::new("Ouro", max, 1).retailer_id("G");
    let order = Order::new([one.clone()], max, DescribedAmount::new(Amount::brl(0)));
    assert_eq!(order.items_sum(), Some(u64::MAX));
    let order = Order::new(
        [one.clone(), one],
        max,
        DescribedAmount::new(Amount::brl(0)),
    );
    assert_eq!(order.items_sum(), None);
    let mut msg = brl_valid();
    msg.parameters.order = Some(order);
    msg.parameters.total_amount = max;
    assert_eq!(refused(&msg), format!("{P}.order.subtotal.value"));
}

// ─── One currency, one set of rules ──────────────────────────────────────

/// The decisive pair: the same values pass by Brazil's rules and are
/// refused by India's, and the other way round.
#[test]
fn a_brl_order_is_checked_by_brazils_rules_and_an_inr_one_by_indias() {
    // A 60-character reference: Brazil's limit, beyond India's 35.
    let mut brl = brl_valid();
    brl.parameters.reference_id = "x".repeat(60);
    assert_eq!(brl.validate(), Ok(()));
    let mut inr = valid();
    inr.parameters.reference_id = "x".repeat(60);
    assert_eq!(refused(&inr), format!("{P}.reference_id"));
    inr.parameters.reference_id = "x".repeat(35);
    assert_eq!(inr.validate(), Ok(()));

    // No `order`: a simplified BRL message, an incomplete INR one.
    let mut brl = brl_valid();
    brl.parameters.order = None;
    assert_eq!(brl.validate(), Ok(()));
    let mut inr = valid();
    inr.parameters.order = None;
    assert_eq!(refused(&inr), format!("{P}.order"));

    // An item without origin or importer, and no catalog: India needs
    // them; Brazil's item table has neither.
    let brl = brl_valid();
    assert!(brl.parameters.order.as_ref().unwrap().catalog_id.is_none());
    assert_eq!(brl.validate(), Ok(()));

    // A subtotal that is not the items' sum: Brazil checks it, India
    // does not (the examples break it there).
    let mut inr = valid();
    let o = inr.parameters.order.as_mut().unwrap();
    assert_ne!(o.items_sum(), Some(o.subtotal.value));
    assert_eq!(inr.validate(), Ok(()));

    // Brazil's settings and `payment_type` in an INR order.
    for setting in [
        PaymentSetting::from(pix()),
        PaymentSetting::boleto(DIGITABLE_LINE),
        PaymentSetting::offsite_card_pay("5235", "1234567"),
    ] {
        let mut inr = valid();
        inr.parameters.payment_settings = vec![setting];
        assert_eq!(refused(&inr), format!("{P}.payment_settings[0].type"));
    }
    let mut inr = valid();
    inr.parameters.payment_type = Some(PaymentType::Br);
    assert_eq!(refused(&inr), format!("{P}.payment_type"));
    // A payment link in INR keeps its success and cancel URLs.
    let mut inr = valid();
    inr.parameters.payment_settings = vec![
        PaymentLink::new("https://x")
            .success_url("https://x/ok")
            .cancel_url("https://x/no")
            .into(),
    ];
    inr.parameters.payment_type = Some(PaymentType::Upi);
    assert_eq!(inr.validate(), Ok(()));
}

// ─── order_status ────────────────────────────────────────────────────────

fn status_json(parameters: &Value) -> Value {
    envelope(&json!({
      "type": "order_status",
      "body": {"text": "your-mandatory-text-body-content"},
      "footer": {"text": "your-optional-text-footer-content"},
      "action": {"name": "review_order", "parameters": parameters}
    }))
}

fn brl_status() -> OrderStatus {
    OrderStatus::brl("your-mandatory-text-body-content", "unique-reference-id")
        .footer("your-optional-text-footer-content")
}

/// `payments-br/orders`, "Order status example": the order's status and
/// the payment's.
#[test]
fn brl_order_status_example() {
    let msg = brl_status()
        .order(OrderUpdate::new(OrderStatusValue::Processing))
        .payment(PaymentUpdate::new(PaymentUpdateStatus::Captured).timestamp(1_722_445_231));
    assert_eq!(
        sent(msg),
        status_json(&json!({
          "reference_id": "unique-reference-id",
          "order": {"status": "processing"},
          "payment": {"status": "captured", "timestamp": 1_722_445_231}
        }))
    );
}

/// `payments-br/orders`, "Simplified order status example": the payment
/// alone.
#[test]
fn brl_simplified_order_status_example() {
    let msg = brl_status()
        .payment(PaymentUpdate::new(PaymentUpdateStatus::Captured).timestamp(1_722_445_231));
    assert_eq!(
        sent(msg),
        status_json(&json!({
          "reference_id": "unique-reference-id",
          "payment": {"status": "captured", "timestamp": 1_722_445_231}
        }))
    );
}

/// The decisive pair for `order_status`: `pending`, a `payment` update,
/// no order update and a 60-character reference pass in Brazil and are
/// refused in India.
#[test]
fn an_order_status_is_checked_by_its_currencys_rules() {
    let pending = OrderUpdate::new(OrderStatusValue::Pending);
    let payment = PaymentUpdate::new(PaymentUpdateStatus::Pending);
    let ref_60 = "x".repeat(60);

    let brl = OrderStatus::brl("b", ref_60.clone())
        .order(pending.clone())
        .payment(payment.clone());
    assert_eq!(brl.validate(), Ok(()));
    assert_eq!(OrderStatus::brl("b", "ref").validate(), Ok(()));
    assert_eq!(
        OrderStatus::brl("b", "x".repeat(61))
            .validate()
            .unwrap_err()
            .field,
        format!("{P}.reference_id")
    );

    let field = |m: OrderStatus| m.validate().unwrap_err().field;
    assert_eq!(
        field(OrderStatus::new("b", "ref", pending)),
        format!("{P}.order.status")
    );
    assert_eq!(
        field(
            OrderStatus::new("b", "ref", OrderUpdate::new(OrderStatusValue::Processing))
                .payment(payment)
        ),
        format!("{P}.payment")
    );
    let mut inr = OrderStatus::new("b", ref_60, OrderUpdate::new(OrderStatusValue::Processing));
    assert_eq!(field(inr.clone()), format!("{P}.reference_id"));
    inr.reference_id = "x".repeat(35);
    assert_eq!(inr.validate(), Ok(()));
    inr.order = None;
    assert_eq!(field(inr), format!("{P}.order"));
}

#[test]
fn brl_order_status_rules_are_checked() {
    type StatusEdit = fn(&mut OrderStatus);
    let ok = || brl_status().order(OrderUpdate::new(OrderStatusValue::Pending));
    assert_eq!(ok().validate(), Ok(()));
    let cases: &[(StatusEdit, &str)] = &[
        (|m| m.body = chars(1025), "interactive.body.text"),
        (|m| m.footer = Some(chars(61)), "interactive.footer.text"),
        (|m| m.reference_id = "x".repeat(61), "reference_id"),
        (|m| m.reference_id = "pedido é".into(), "reference_id"),
        (
            |m| m.order = Some(OrderUpdate::new(OrderStatusValue::Other(String::new()))),
            "order.status",
        ),
        (
            |m| {
                m.order =
                    Some(OrderUpdate::new(OrderStatusValue::Completed).description(chars(121)));
            },
            "order.description",
        ),
        (
            |m| {
                m.payment = Some(PaymentUpdate::new(
                    PaymentUpdateStatus::Other(String::new()),
                ));
            },
            "payment.status",
        ),
    ];
    for (edit, field) in cases {
        let mut msg = ok();
        edit(&mut msg);
        let message = OutboundMessage::new(Recipient::phone(TO), msg);
        let got = message.validate().unwrap_err().field;
        assert!(
            got == *field || got == format!("{P}.{field}"),
            "expected `{field}`, got `{got}`"
        );
    }
    // At the limits, in characters; every status of both tables.
    for status in [
        OrderStatusValue::Pending,
        OrderStatusValue::Processing,
        OrderStatusValue::PartiallyShipped,
        OrderStatusValue::Shipped,
        OrderStatusValue::Completed,
        OrderStatusValue::Canceled,
    ] {
        for payment in [
            PaymentUpdateStatus::Pending,
            PaymentUpdateStatus::Captured,
            PaymentUpdateStatus::Failed,
        ] {
            let msg = OrderStatus::brl(chars(1024), "B.m_3-4".repeat(9)[..60].to_owned())
                .footer(chars(60))
                .order(OrderUpdate::new(status.clone()).description(chars(120)))
                .payment(PaymentUpdate::new(payment));
            assert_eq!(msg.validate(), Ok(()));
        }
    }
}
