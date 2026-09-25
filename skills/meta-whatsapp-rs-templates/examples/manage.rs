//! Reference code for the `meta-whatsapp-rs-templates` skill: template definitions
//! (what a template *is*), creating, listing, editing and deleting them,
//! and following their review with webhooks.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use futures::StreamExt;
use meta_whatsapp_rs::client::templates::{
    Button, ParameterFormat, TemplateCategory, TemplateComponent, TemplateCreated,
    TemplateDefinition, TemplateEdit, TemplateInfo, TemplateListQuery, TemplateStatus, Templates,
};
use meta_whatsapp_rs::core::ids::{TemplateId, UploadHandle};
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::fields::TemplateStatusEvent;

/// A utility template with positional placeholders and a URL button.
pub async fn create_order_update(templates: &Templates) -> meta_whatsapp_rs::Result<TemplateCreated> {
    let definition = TemplateDefinition::new("order_update", "en_US", TemplateCategory::Utility)
        .component(TemplateComponent::body_positional(
            "Hi {{1}}, order {{2}} has shipped.",
            ["Pablo", "860198"], // one example per placeholder, in order
        ))
        .component(TemplateComponent::buttons([Button::url_with_example(
            "Track",
            "https://shop.example/track/{{1}}",
            "860198",
        )]));
    templates.create(&definition).await // checked locally first; usually PENDING
}

/// A marketing template with named placeholders and an image header.
pub fn autumn_sale(header: &UploadHandle) -> TemplateDefinition {
    TemplateDefinition::new("autumn_sale", "en_US", TemplateCategory::Marketing)
        .parameter_format(ParameterFormat::Named) // required for {{first_name}}
        .component(TemplateComponent::header_image(header.as_str())) // a Resumable Upload handle
        .component(TemplateComponent::body_named(
            "Hi {{first_name}}, 20% off until Sunday.",
            [("first_name", "Alex")],
        ))
        .component(TemplateComponent::buttons([Button::quick_reply(
            "Stop promotions",
        )]))
}

/// Every approved template, following the cursors.
pub async fn approved(templates: &Templates) -> meta_whatsapp_rs::Result<Vec<TemplateInfo>> {
    let query = TemplateListQuery::new().status(TemplateStatus::Approved);
    let mut stream = std::pin::pin!(templates.list_stream(&query));
    let mut found = Vec::new();
    while let Some(template) = stream.next().await {
        found.push(template?);
    }
    Ok(found)
}

/// Replace the components: an edit sends ALL of them.
pub async fn reword(templates: &Templates, id: &TemplateId) -> meta_whatsapp_rs::Result<()> {
    let edit = TemplateEdit::components(vec![TemplateComponent::body_positional(
        "Hi {{1}}, order {{2}} is on its way.",
        ["Pablo", "860198"],
    )]);
    templates.edit(id, &edit).await // approved templates: 10 edits in 30 days
}

/// Approval is asynchronous: react to the webhook instead of polling.
pub fn on_review(event: &WebhookEvent) -> Option<(TemplateId, bool)> {
    let WebhookEvent::TemplateStatusUpdated { update, .. } = event else {
        return None;
    };
    match update.event {
        TemplateStatusEvent::Approved => Some((update.message_template_id.clone(), true)),
        TemplateStatusEvent::Rejected | TemplateStatusEvent::Disabled => {
            Some((update.message_template_id.clone(), false)) // `update.reason` says why
        }
        _ => None, // Paused, Pending, and values Meta adds later
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use meta_whatsapp_rs::core::testing::ScriptedTransport;

    use super::*;

    fn templates(transport: &ScriptedTransport) -> Templates {
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        client.templates("102290129340398")
    }

    #[tokio::test]
    async fn create_posts_the_definition() {
        let transport = ScriptedTransport::new();
        transport.push_json(
            200,
            json!({"id": "594425479261596", "status": "PENDING", "category": "UTILITY"}),
        );
        let created = create_order_update(&templates(&transport)).await.unwrap();
        assert_eq!(created.status, Some(TemplateStatus::Pending));
        let request = transport.last_request().unwrap();
        assert_eq!(request.path(), "/v25.0/102290129340398/message_templates");
        let body = request.json().unwrap();
        assert_eq!(body["category"], "UTILITY");
        assert_eq!(
            body["components"][0]["example"]["body_text"],
            json!([["Pablo", "860198"]])
        );
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn named_placeholders_need_the_named_format() {
        let handle = UploadHandle::new("4::aW1hZ2UvcG5n:ARb");
        assert!(autumn_sale(&handle).validate().is_ok());
        let undeclared =
            TemplateDefinition::new("autumn_sale", "en_US", TemplateCategory::Marketing).component(
                TemplateComponent::body_named("Hi {{first_name}}", [("first_name", "Alex")]),
            );
        assert!(undeclared.validate().is_err()); // positional is the default
    }

    #[tokio::test]
    async fn list_stream_follows_cursors() {
        let transport = ScriptedTransport::new();
        transport.push_json(
            200,
            json!({
                "data": [{"id": "1", "name": "a", "status": "APPROVED"}],
                "paging": {"cursors": {"after": "C1"}, "next": "https://graph.facebook.com/next"}
            }),
        );
        transport.push_json(
            200,
            json!({"data": [{"id": "2", "name": "b", "status": "APPROVED"}]}),
        );
        let found = approved(&templates(&transport)).await.unwrap();
        assert_eq!(found.len(), 2);
        let requests = transport.requests();
        assert_eq!(requests[0].query("status").as_deref(), Some("APPROVED"));
        assert_eq!(requests[1].query("after").as_deref(), Some("C1"));
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn review_webhooks() {
        // Meta's documented `message_template_status_update` delivery.
        let body = json!({"object": "whatsapp_business_account", "entry": [{
            "id": "102290129340398", "time": 1751247548,
            "changes": [{"field": "message_template_status_update", "value": {
                "event": "APPROVED", "message_template_id": 1_689_556_908_129_832_u64,
                "message_template_name": "order_update", "message_template_language": "en-US",
                "reason": "NONE", "message_template_category": "UTILITY"
            }}]
        }]});
        let payload = meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(body.to_string().as_bytes());
        let events = payload.unwrap().into_events();
        assert_eq!(
            on_review(&events[0]),
            Some(("1689556908129832".into(), true))
        );
    }
}
