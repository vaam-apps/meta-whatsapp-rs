//! Reference code for the `meta-whatsapp-rs-marketing` skill: opt-ins (In-App Signup
//! links, QR codes), sending marketing templates on the Cloud API or the
//! Marketing Messages API, honouring opt-outs and per-user limits, and
//! reading analytics.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::analytics::{MessagingAnalyticsQuery, MessagingGranularity};
use meta_whatsapp_rs::client::marketing::{MarketingOptions, OnboardingStatus};
use meta_whatsapp_rs::client::qr_codes::{CreateQrCode, QrImageFormat};
use meta_whatsapp_rs::client::signups::{NewSignup, SignupPolicy, deep_link};
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::fields::PreferenceValue;
use time::OffsetDateTime;

/// An opt-in link that subscribes the user and sends a promo code.
pub async fn opt_in_link(
    client: &Client,
    waba_id: WabaId,
    business_number: &str, // E.164 with `+`
    first_signup_of_this_business: bool,
) -> meta_whatsapp_rs::Result<String> {
    let mut signup = NewSignup::new(
        "Get our deals on WhatsApp",
        "You're in! Code {{promo_code}}",
        "https://shop.example/privacy",
    )
    .promo_code("WELCOME10");
    if first_signup_of_this_business {
        // Accepts Meta's marketing terms for the business: only after it agreed.
        signup = signup.policy(SignupPolicy::accept_terms());
    }
    let created = client.signups(waba_id).create(&signup).await?;
    deep_link(business_number, &created.id) // wa.me/<digits>/signup/<id>; add https://
}

/// A QR code for print: it opens a chat with a prefilled message.
pub async fn shop_window_qr(
    client: &Client,
    phone_number_id: PhoneNumberId,
) -> meta_whatsapp_rs::Result<String> {
    let request =
        CreateQrCode::new("Hi! Send me the autumn catalog").with_image(QrImageFormat::Svg);
    let qr = client.qr_codes(phone_number_id).create(&request).await?;
    Ok(qr.deep_link_url) // https://wa.me/message/<code>; qr.qr_image_url is the image
}

/// A marketing template through the Marketing Messages API when the WABA
/// is onboarded, else the Cloud API.
pub async fn send_offer(
    client: &Client,
    waba_id: WabaId,
    phone_number_id: PhoneNumberId,
    to: Recipient,
    offer: TemplateMessage, // an APPROVED marketing template
) -> meta_whatsapp_rs::Result<SendResponse> {
    let status = client
        .marketing_account(waba_id)
        .onboarding_status()
        .await?;
    if status == Some(OnboardingStatus::Onboarded) {
        let options = MarketingOptions::new(); // .strict(): no Cloud API fallback
        client
            .marketing(phone_number_id)
            .send(&to, &offer, &options)
            .await
    } else {
        let message = OutboundMessage::template(to, offer);
        client.messages(phone_number_id).send(&message).await
    }
}

/// What to record about a customer's marketing consent.
#[derive(Debug, PartialEq, Eq)]
pub enum Consent {
    OptedOut(Option<UserId>),       // stop until they resume
    OptedIn(Option<UserId>),        // `Resume`
    HoldFor24Hours(Option<UserId>), // 131049: not before 24 h, never automatically
}

/// Opt-outs arrive as preferences, or as errors on failed statuses.
pub fn consent_changes(event: &WebhookEvent) -> Vec<Consent> {
    match event {
        WebhookEvent::UserPreferenceChanged { preference, .. } => match preference.value {
            PreferenceValue::Stop => vec![Consent::OptedOut(preference.user_id.clone())],
            PreferenceValue::Resume => vec![Consent::OptedIn(preference.user_id.clone())],
            _ => vec![],
        },
        WebhookEvent::StatusUpdated { status, .. } => status
            .errors
            .iter()
            .filter_map(|error| match error.kind() {
                ErrorKind::MarketingOptedOut => {
                    Some(Consent::OptedOut(status.recipient_user_id.clone()))
                }
                ErrorKind::EcosystemEngagementLimit => {
                    Some(Consent::HoldFor24Hours(status.recipient_user_id.clone()))
                }
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// Sent and delivered per day.
pub async fn last_week(
    client: &Client,
    waba_id: WabaId,
    now: OffsetDateTime,
) -> meta_whatsapp_rs::Result<u64> {
    let query = MessagingAnalyticsQuery::new(
        now - time::Duration::days(7),
        now,
        MessagingGranularity::Day,
    );
    let analytics = client.analytics(waba_id).messaging(&query).await?;
    let delivered = analytics
        .iter()
        .flat_map(|a| &a.data_points)
        .filter_map(|p| p.delivered)
        .sum();
    Ok(delivered)
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use serde_json::json;

    use super::*;

    fn client(transport: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn terms_only_on_the_first_signup() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"id": "1234567890"}));
        let link = opt_in_link(
            &client(&transport),
            "102290129340398".into(),
            "+15551234567",
            false,
        )
        .await
        .unwrap();
        assert_eq!(link, "wa.me/15551234567/signup/1234567890");
        let body = transport.last_request().unwrap().json().unwrap();
        assert!(body.get("policy").is_none());
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn onboarded_wabas_use_the_marketing_messages_api() {
        let transport = ScriptedTransport::new();
        transport.push_json(
            200,
            json!({"id": "102290129340398",
            "marketing_messages_onboarding_status": "ONBOARDED"}),
        );
        transport.push_json(200, json!({"messages": [{"id": "wamid.MM"}]}));
        let offer = TemplateMessage::new("autumn_sale", "en_US");
        send_offer(
            &client(&transport),
            "102290129340398".into(),
            "106540352242922".into(),
            Recipient::phone("+16505551234"),
            offer,
        )
        .await
        .unwrap();
        assert_eq!(
            transport.last_request().unwrap().path(),
            "/v25.0/106540352242922/marketing_messages"
        );
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn a_failed_status_can_carry_an_engagement_limit() {
        // Meta's documented `failed` status with 131049.
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": "messages", "value": {"messaging_product": "whatsapp",
                "metadata": {"display_phone_number": "15550783881", "phone_number_id": "106540352242922"},
                "statuses": [{"id": "wamid.X", "status": "failed", "timestamp": "1751142888",
                    "recipient_id": "16505551234", "recipient_user_id": "US.1",
                    "errors": [{"code": 131049, "title": "healthy ecosystem engagement"}]}]}}]}]});
        let events =
            meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(body.to_string().as_bytes())
                .unwrap()
                .into_events();
        assert_eq!(
            consent_changes(&events[0]),
            [Consent::HoldFor24Hours(Some("US.1".into()))]
        );
    }
}
