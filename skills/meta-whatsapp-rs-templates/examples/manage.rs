//! Reference code for the `meta-whatsapp-rs-templates` skill: template definitions
//! (what a template *is*), creating, listing, editing and deleting them,
//! archiving them and grouping them, and following their review with
//! webhooks.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use futures::StreamExt;
use meta_whatsapp_rs::client::templates::{
    Button, ParameterFormat, TemplateCategory, TemplateComponent, TemplateCreated,
    TemplateDefinition, TemplateEdit, TemplateGroupCreate, TemplateInfo, TemplateListQuery,
    TemplateStatus, Templates,
};
use meta_whatsapp_rs::core::ids::{TemplateGroupId, TemplateId, UploadHandle};
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::fields::TemplateStatusEvent;

/// A utility template with positional placeholders and a URL button.
pub async fn create_order_update(
    templates: &Templates,
) -> meta_whatsapp_rs::Result<TemplateCreated> {
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

/// Archive in bulk; returns the ids Meta refused, with its reason.
pub async fn archive_unused(
    templates: &Templates,
    ids: &[TemplateId],
) -> meta_whatsapp_rs::Result<Vec<(TemplateId, String)>> {
    let result = templates.archive(ids).await?; // 1 to 100 ids; never replayed
    Ok(result.failed_templates.into_iter().collect()) // a partial failure is no error
}

/// Group templates to read their analytics as a set.
pub async fn black_friday_group(
    templates: &Templates,
    ids: &[TemplateId],
) -> meta_whatsapp_rs::Result<TemplateGroupId> {
    let group = TemplateGroupCreate::new("Black Friday 2024", ids.iter().cloned())
        .description("US-based Black Friday sale 2024 templates.");
    Ok(templates.create_group(&group).await?.id) // keep it: there is no list call
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
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use serde_json::json;

    use super::*;

    fn templates(transport: &ScriptedTransport) -> Templates {
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        client.templates(WabaId::new("102290129340398").unwrap())
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

    #[tokio::test]
    async fn archive_reports_what_failed() {
        let transport = ScriptedTransport::new();
        transport.push_json(
            200,
            json!({
                "archived_templates": ["1387372356726668"],
                "failed_templates": {
                    "1304694804498707": "Only approved or rejected templates can be archived"
                }
            }),
        );
        let ids = [
            TemplateId::new("1387372356726668").unwrap(),
            TemplateId::new("1304694804498707").unwrap(),
        ];
        let failed = archive_unused(&templates(&transport), &ids).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0.as_str(), "1304694804498707");
        let request = transport.last_request().unwrap();
        assert_eq!(
            request.path(),
            "/v25.0/102290129340398/message_templates/archive"
        );
        assert_eq!(
            request.json(),
            Some(json!({"hsm_ids": ["1387372356726668", "1304694804498707"]}))
        );
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn groups_write_template_ids_as_integers() {
        let transport = ScriptedTransport::new();
        transport.push_json(200, json!({"id": "9020555671393375"}));
        let ids = [
            TemplateId::new("278077987957091").unwrap(),
            TemplateId::new("1945418102598215").unwrap(),
        ];
        let id = black_friday_group(&templates(&transport), &ids)
            .await
            .unwrap();
        assert_eq!(id.as_str(), "9020555671393375");
        let body = transport.last_request().unwrap().json().unwrap();
        assert_eq!(
            body["whatsapp_business_templates"],
            json!([278_077_987_957_091_u64, 1_945_418_102_598_215_u64])
        );
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
        let payload =
            meta_whatsapp_rs::webhooks::WebhookPayload::from_slice(body.to_string().as_bytes());
        let events = payload.unwrap().into_events();
        assert_eq!(
            on_review(&events[0]),
            Some((TemplateId::new("1689556908129832").unwrap(), true))
        );
    }
}
