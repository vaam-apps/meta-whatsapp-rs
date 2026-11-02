//! The checkout endpoint's four sample requests, each encrypted as the
//! WhatsApp client does (RSA-OAEP-SHA256 for the AES key, AES-128-GCM with
//! a 16-byte IV for the body: the Flows endpoint's protocol), decrypted by
//! [`FlowEndpointKey::decrypt_checkout_request`], answered with the page's
//! sample response built from the request, sealed by the returned
//! [`ResponseSealer`], and opened again as the WhatsApp client would (same
//! key, bit-flipped IV): what comes out is the page's response, exactly.

use aes_gcm::AesGcm;
use aes_gcm::aead::consts::U16;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::aes::Aes128;
use aws_lc_rs::rsa::{OAEP_SHA256_MGF1SHA256, OaepPublicEncryptingKey, PublicEncryptingKey};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use meta_whatsapp_core::error::CryptoError;
use pretty_assertions::assert_eq;

use super::*;
use crate::flows::endpoint::{EncryptedFlowRequest, EndpointAction, FlowEndpointKey, FlowResponse};
use crate::payments::checkout::{
    CheckoutRequest, CheckoutResponse, CheckoutResponseData, CheckoutSubAction, Coupon,
};

/// The Flows endpoint tests' 2048-bit key (`flows/endpoint/testdata`).
const PKCS8: &str = include_str!("../../../flows/endpoint/testdata/test_rsa2048_pkcs8.pem");
const PUBLIC: &str = include_str!("../../../flows/endpoint/testdata/test_rsa2048_public.pem");

type Cipher = AesGcm<Aes128, U16>;

/// What the WhatsApp client does with one request: a fresh AES key and IV.
struct WhatsAppClient {
    aes_key: [u8; 16],
    iv: [u8; 16],
}

impl WhatsAppClient {
    fn new() -> Self {
        let mut aes_key = [0u8; 16];
        let mut iv = [0u8; 16];
        getrandom::fill(&mut aes_key).unwrap();
        getrandom::fill(&mut iv).unwrap();
        Self { aes_key, iv }
    }

    fn encrypt(&self, payload: &Value) -> EncryptedFlowRequest {
        let der: String = PUBLIC.lines().filter(|l| !l.starts_with("-----")).collect();
        let public = PublicEncryptingKey::from_der(&STANDARD.decode(der).unwrap()).unwrap();
        let oaep = OaepPublicEncryptingKey::new(public).unwrap();
        let mut wrapped = vec![0u8; oaep.ciphertext_size()];
        let wrapped = oaep
            .encrypt(&OAEP_SHA256_MGF1SHA256, &self.aes_key, &mut wrapped, None)
            .unwrap()
            .to_vec();
        let mut data = serde_json::to_vec(payload).unwrap();
        let tag = Cipher::new(&self.aes_key.into())
            .encrypt_in_place_detached(&self.iv.into(), b"", &mut data)
            .unwrap();
        data.extend_from_slice(&tag);
        EncryptedFlowRequest {
            encrypted_flow_data: STANDARD.encode(data),
            encrypted_aes_key: STANDARD.encode(wrapped),
            initial_vector: STANDARD.encode(self.iv),
        }
    }

    /// Open the endpoint's sealed answer: the same key, the IV flipped.
    fn open(&self, sealed: &str) -> Value {
        let mut data = STANDARD.decode(sealed).unwrap();
        let tag = data.split_off(data.len() - 16);
        let flipped = self.iv.map(|b| b ^ 0xFF);
        Cipher::new(&self.aes_key.into())
            .decrypt_in_place_detached(&flipped.into(), b"", &mut data, tag.as_slice().into())
            .unwrap();
        serde_json::from_slice(&data).unwrap()
    }
}

fn key() -> FlowEndpointKey {
    FlowEndpointKey::from_pem(PKCS8).unwrap()
}

/// The order every request and answer of the page carries, with the
/// fields they differ in: the shipping value, the coupon, the selected
/// address (in the `apply_shipping` answer's `shipping_info`) and the
/// total. Everything else is copied from the page.
fn endpoint_order(
    shipping: u64,
    coupon: Option<Value>,
    selected_address: bool,
    total: u64,
) -> Value {
    let mut shipping_info = json!({"country": "IN", "addresses": [address_json()]});
    if selected_address {
        shipping_info["selected_address"] = address_json();
    }
    let mut order = json!({
        "reference_id": "abc.123_xyz-1",
        "type": "physical-goods",
        "currency": "INR",
        "shipping_info": shipping_info,
        "order": {
            "items": [
                {
                    "amount": {"offset": 100, "value": 200000},
                    "sale_amount": {"offset": 100, "value": 150000},
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
            "subtotal": {"offset": 100, "value": 150000},
            "shipping": {"offset": 100, "value": shipping},
            "tax": {"offset": 100, "value": 10000},
            "discount": {"offset": 100, "value": 15000, "description": "Additional 10% off"},
            "status": "pending",
            "expiration": {"timestamp": "1726627150", "description": "order expires in 5 min"}
        },
        "total_amount": {"offset": 100, "value": total}
    });
    if let Some(coupon) = coupon {
        order["coupon"] = coupon;
    }
    order
}

/// The applied coupon of the page's examples.
fn trynew10() -> Value {
    json!({"code": "TRYNEW10", "discount": {"value": 16500, "offset": 100}})
}

/// A sample request of the page: `order` and `input` under `data`.
fn request(sub_action: &str, order: Value, input: Value) -> Value {
    let mut request = json!({
        "data": {},
        "action": "data_exchange",
        "sub_action": sub_action,
        "version": "1.0"
    });
    request["data"]["order_details"] = order;
    request["data"]["input"] = input;
    request
}

/// A sample response of the page carrying `order`.
fn order_response(sub_action: &str, order: Value) -> Value {
    let mut response = json!({"sub_action": sub_action, "version": "1.0", "data": {}});
    response["data"]["order_details"] = order;
    response
}

/// Decrypt `doc` as the endpoint receives it, check it parses back to the
/// page's JSON, answer it with `answer`, check that against the page's
/// response, seal it, and check what the WhatsApp client opens.
#[track_caller]
fn exchange(
    doc_request: &Value,
    doc_response: &Value,
    answer: impl FnOnce(&CheckoutRequest) -> CheckoutResponse,
) -> CheckoutRequest {
    let whatsapp = WhatsAppClient::new();
    let (request, sealer) = key()
        .decrypt_checkout_request(&whatsapp.encrypt(doc_request))
        .unwrap();
    assert_eq!(request.action, EndpointAction::DataExchange);
    assert_eq!(request.version, "1.0");
    assert_eq!(serde_json::to_value(&request).unwrap(), *doc_request);

    let response = answer(&request);
    assert_eq!(serde_json::to_value(&response).unwrap(), *doc_response);
    if let Err(e) = response.validate() {
        panic!("the page's response fails validation: {e}");
    }
    // It parses back too.
    let parsed: CheckoutResponse = serde_json::from_value(doc_response.clone()).unwrap();
    assert_eq!(parsed, response);

    assert_eq!(
        whatsapp.open(&sealer.seal(&response).unwrap()),
        *doc_response
    );
    request
}

#[test]
fn get_coupons_sample_request_and_response() {
    let doc_request = request(
        "get_coupons",
        endpoint_order(20_000, None, false, 165_000),
        json!({"user_id": "919000090000"}),
    );
    let doc_response = json!({
        "version": "1.0",
        "sub_action": "get_coupons",
        "data": {
            "coupons": [
                {"description": "Save R20 on the order", "code": "TRYNEW20", "id": "try_new_20_id"},
                {"description": "Save R30 on the order", "code": "TRYNEW30", "id": "try_new_30_id"},
                {"description": "Save R50 on the order", "code": "TRYNEW50", "id": "try_new50_id"}
            ]
        }
    });
    let request = exchange(&doc_request, &doc_response, |r| {
        assert_eq!(r.sub_action, Some(CheckoutSubAction::GetCoupons));
        CheckoutResponse::coupons(
            &r.version,
            [
                Coupon::new("TRYNEW20", "try_new_20_id", "Save R20 on the order"),
                Coupon::new("TRYNEW30", "try_new_30_id", "Save R30 on the order"),
                Coupon::new("TRYNEW50", "try_new50_id", "Save R50 on the order"),
            ],
        )
    });
    let data = request.data.unwrap();
    assert_eq!(data.input.user_id, "919000090000");
    assert_eq!(data.input.coupon, None);
    // The order is the send's, its payment settings left out, its
    // expiration described.
    assert!(data.order_details.payment_settings.is_empty());
    assert_eq!(data.order_details.total_amount, Amount::inr(165_000));
    assert_eq!(
        data.order_details.shipping_info,
        Some(ShippingInfo::india([address()]))
    );
}

#[test]
fn apply_coupon_sample_request_and_response() {
    let doc_request = request(
        "apply_coupon",
        endpoint_order(20_000, None, false, 165_000),
        json!({"user_id": "919000090000", "coupon": {"code": "TRYNEW10"}}),
    );
    let doc_response = order_response(
        "apply_coupon",
        endpoint_order(20_000, Some(trynew10()), false, 148_500),
    );
    exchange(&doc_request, &doc_response, |r| {
        let data = r.data.as_ref().unwrap();
        let code = &data.input.coupon.as_ref().unwrap().code;
        assert_eq!(code, "TRYNEW10");
        // The coupon attached, the total follows: 165000 - 16500.
        let order = data
            .order_details
            .clone()
            .coupon(AppliedCoupon::new(code, Amount::inr(16_500)));
        assert_eq!(order.total_amount, Amount::inr(148_500));
        CheckoutResponse::order(&r.version, CheckoutSubAction::ApplyCoupon, order)
    });
}

#[test]
fn remove_coupon_sample_request_and_response() {
    let doc_request = request(
        "remove_coupon",
        endpoint_order(20_000, Some(trynew10()), false, 148_500),
        json!({"user_id": "919000090000"}),
    );
    let doc_response = order_response(
        "remove_coupon",
        endpoint_order(20_000, None, false, 165_000),
    );
    let request = exchange(&doc_request, &doc_response, |r| {
        let order = r
            .data
            .as_ref()
            .unwrap()
            .order_details
            .clone()
            .without_coupon();
        CheckoutResponse::order(&r.version, CheckoutSubAction::RemoveCoupon, order)
    });
    assert_eq!(
        request.data.unwrap().order_details.coupon,
        Some(AppliedCoupon::new("TRYNEW10", Amount::inr(16_500)))
    );
}

#[test]
fn apply_shipping_sample_request_and_response() {
    let doc_request = request(
        "apply_shipping",
        endpoint_order(20_000, Some(trynew10()), false, 148_500),
        json!({"user_id": "919000090000", "selected_address": address_json()}),
    );
    let doc_response = order_response(
        "apply_shipping",
        endpoint_order(40_000, Some(trynew10()), true, 168_500),
    );
    exchange(&doc_request, &doc_response, |r| {
        let data = r.data.as_ref().unwrap();
        let selected = data.input.selected_address.clone().unwrap();
        assert_eq!(selected, address());
        let mut order = data.order_details.clone();
        // Shipping to that address costs ₹400: 150000 + 40000 + 10000
        // - 15000 - 16500.
        if let Some(o) = order.order.as_mut() {
            o.shipping = Some(DescribedAmount::new(Amount::inr(40_000)));
        }
        order.shipping_info = Some(ShippingInfo::india([address()]).selected_address(selected));
        let order = order.with_expected_total();
        assert_eq!(order.total_amount, Amount::inr(168_500));
        CheckoutResponse::order(&r.version, CheckoutSubAction::ApplyShipping, order)
    });
}

/// The answer to `apply_coupon` of the page, typed.
fn applied() -> CheckoutResponse {
    let doc = order_response(
        "apply_coupon",
        endpoint_order(20_000, Some(trynew10()), false, 148_500),
    );
    serde_json::from_value(doc).unwrap()
}

fn order_of(response: &mut CheckoutResponse) -> &mut OrderDetailsParameters {
    match &mut response.data {
        CheckoutResponseData::Order { order_details } => order_details,
        CheckoutResponseData::Coupons { .. } => panic!("not an order"),
    }
}

#[test]
fn an_answer_is_checked_before_it_is_sealed() {
    applied().validate().unwrap();
    let refused = |f: &dyn Fn(&mut CheckoutResponse)| {
        let mut r = applied();
        f(&mut r);
        r.validate().unwrap_err()
    };
    // The coupon's discount comes off the total: the order's total without
    // it is refused.
    let e = refused(&|r| order_of(r).total_amount = Amount::inr(165_000));
    assert_eq!(e.field, "data.order_details.total_amount.value");
    assert!(e.reason.contains("coupon"), "{e:?}");
    // A coupon worth more than the order.
    let e = refused(&|r| {
        order_of(r).coupon = Some(AppliedCoupon::new("ALL", Amount::inr(1_000_000)));
    });
    assert_eq!(e.field, "data.order_details.total_amount.value");
    // `apply_coupon` attaches it; `remove_coupon` removes it.
    assert_eq!(
        refused(&|r| *order_of(r) = order_of(r).clone().without_coupon()).field,
        "data.order_details.coupon"
    );
    assert_eq!(
        refused(&|r| r.sub_action = CheckoutSubAction::RemoveCoupon).field,
        "data.order_details.coupon"
    );
    // No payment settings come back.
    let e = refused(&|r| {
        order_of(r).payment_settings = vec![
            PaymentGateway::new(GatewayProvider::Razorpay(RazorpayFields::default()), CONFIG)
                .into(),
        ];
    });
    assert_eq!(e.field, "data.order_details.payment_settings");
    // India only.
    let e = refused(&|r| order_of(r).currency = Currency::Brl);
    assert_eq!(e.field, "data.order_details.currency");
    // The coupon's amount is in paise.
    assert_eq!(
        refused(&|r| {
            if let Some(c) = order_of(r).coupon.as_mut() {
                c.discount.offset = 1;
            }
        })
        .field,
        "data.order_details.coupon.discount.offset"
    );
    // The data the sub-action takes.
    assert_eq!(
        refused(&|r| r.sub_action = CheckoutSubAction::GetCoupons).field,
        "data"
    );
    assert_eq!(
        refused(&|r| r.sub_action = CheckoutSubAction::from("apply_gift_card")).field,
        "sub_action"
    );
    let mut coupons = CheckoutResponse::coupons("1.0", []);
    coupons.sub_action = CheckoutSubAction::ApplyShipping;
    assert_eq!(coupons.validate().unwrap_err().field, "data");
    // India's item rules hold without payment settings: an item without a
    // catalog needs its importer.
    let e = refused(&|r| {
        if let Some(o) = order_of(r).order.as_mut() {
            o.items[0].importer_name = None;
        }
    });
    assert_eq!(e.field, "data.order_details.order.items[0].importer_name");
}

#[test]
fn what_does_not_decrypt_or_parse_is_one_error() {
    let whatsapp = WhatsAppClient::new();
    // Tampered: the tag no longer matches.
    let mut tampered = whatsapp.encrypt(&json!({"version": "1.0", "action": "ping"}));
    tampered.initial_vector = STANDARD.encode([7u8; 16]);
    assert_eq!(
        key().decrypt_checkout_request(&tampered).unwrap_err(),
        CryptoError::Decrypt
    );
    // Decrypts, but is not a checkout request (an order in USD).
    let mut usd = request(
        "get_coupons",
        endpoint_order(20_000, None, false, 165_000),
        json!({"user_id": "919000090000"}),
    );
    usd["data"]["order_details"]["currency"] = json!("USD");
    assert_eq!(
        key()
            .decrypt_checkout_request(&whatsapp.encrypt(&usd))
            .unwrap_err(),
        CryptoError::Decrypt
    );
    // A request with no sub-action (a health check, undocumented here)
    // parses, to be answered as a Flow endpoint's.
    let (ping, sealer) = key()
        .decrypt_checkout_request(&whatsapp.encrypt(&json!({"version": "3.0", "action": "ping"})))
        .unwrap();
    assert_eq!(ping.action, EndpointAction::Ping);
    assert_eq!((ping.sub_action, ping.data), (None, None));
    assert_eq!(
        whatsapp.open(&sealer.seal(&FlowResponse::health_check()).unwrap()),
        json!({"data": {"status": "active"}})
    );
}

/// Past each sub-action's own rules: an answer names no way to pay, keeps
/// India's checks of a send, and is whole (each guard here survived its
/// deletion before this test).
#[test]
fn a_checkout_answer_names_no_way_to_pay_and_is_whole() {
    let refused = |f: &dyn Fn(&mut CheckoutResponse)| {
        let mut r = applied();
        f(&mut r);
        r.validate().unwrap_err().field
    };
    // The older configuration is a way to pay too, and so is Brazil's
    // payment type.
    assert_eq!(
        refused(&|r| order_of(r).payment_configuration = Some(CONFIG.to_owned())),
        "data.order_details.payment_configuration"
    );
    assert_eq!(
        refused(&|r| order_of(r).payment_type = Some(PaymentType::Br)),
        "data.order_details.payment_type"
    );
    // India's beneficiaries, checked as in a send.
    assert_eq!(
        refused(&|r| {
            order_of(r).beneficiaries = vec![Beneficiary::new(
                "",
                "One BKC",
                "Mumbai",
                "Maharashtra",
                "400051",
            )];
        }),
        "data.order_details.beneficiaries[0].name"
    );
    // The coupon has a code, the shipping info its country, the answer
    // the request's version.
    assert_eq!(
        refused(&|r| {
            if let Some(c) = order_of(r).coupon.as_mut() {
                c.code.clear();
            }
        }),
        "data.order_details.coupon.code"
    );
    assert_eq!(
        refused(&|r| {
            if let Some(s) = order_of(r).shipping_info.as_mut() {
                s.country.clear();
            }
        }),
        "data.order_details.shipping_info.country"
    );
    assert_eq!(refused(&|r| r.version.clear()), "version");
    assert_eq!(
        CheckoutResponse::coupons("", [])
            .validate()
            .unwrap_err()
            .field,
        "version"
    );
}

/// The total a coupon leaves: `None` for a coupon worth more than the
/// order (never a free order, which `total_amount` above zero refuses
/// anyway), and a total of zero refused.
#[test]
fn a_coupon_never_takes_the_total_below_zero() {
    let mut order = order_of(&mut applied()).clone();
    // 150000 + 20000 + 10000 - 15000 = 165000 before the coupon.
    assert_eq!(order.expected_total(), Some(148_500));
    order.coupon = Some(AppliedCoupon::new("ALL", Amount::inr(165_001)));
    assert_eq!(order.expected_total(), None);
    assert_eq!(order.clone().with_expected_total().total_amount.value, 0);
    order.coupon = Some(AppliedCoupon::new("ALL", Amount::inr(165_000)));
    assert_eq!(order.expected_total(), Some(0));
    let free = CheckoutResponse::order(
        "1.0",
        CheckoutSubAction::ApplyCoupon,
        order.with_expected_total(),
    );
    assert_eq!(
        free.validate().unwrap_err().field,
        "data.order_details.total_amount.value"
    );
    // At `u64::MAX` the sum does not wrap.
    let mut huge = order_of(&mut applied()).clone();
    if let Some(o) = huge.order.as_mut() {
        o.subtotal = Amount::inr(u64::MAX);
    }
    huge.coupon = None;
    assert_eq!(huge.expected_total(), None);
    huge.total_amount = Amount::inr(u64::MAX);
    let r = CheckoutResponse::order("1.0", CheckoutSubAction::ApplyShipping, huge);
    assert_eq!(
        r.validate().unwrap_err().field,
        "data.order_details.total_amount.value"
    );
}
