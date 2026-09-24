//! Reference code for the `wa-rs-groups-and-calling` skill: what a merchant
//! inbox needs beyond one-to-one text — blocking a customer, group chats,
//! and WhatsApp calls the CMS cannot take.
//!
//! wa-rs compiles this file and runs its tests in its own gate
//! (`crates/wa-rs/tests/skills.rs`).

use wa_rs::client::groups::{CreateGroup, JoinApprovalMode};
use wa_rs::core::ids::GroupId;
use wa_rs::prelude::*;
use wa_rs::webhooks::fields::{CallDirection, CallEventType, GroupUpdateType};

/// "Block this customer" in the merchant's inbox, as the merchant.
pub async fn block_customer(
    merchant: &Client, // `with_token` of the merchant who owns the number
    number: PhoneNumberId,
    customer: Recipient, // `inbox.recipient(&key)`: a BSUID or `+<wa_id>`
) -> wa_rs::Result<Vec<ErrorKind>> {
    let answer = merchant.block_users(number).block(&[customer]).await?;
    // A 2xx can still list failures (with a top-level 139100 error).
    let refused = answer
        .block_users
        .failed_users
        .iter()
        .flat_map(|user| user.errors.iter().map(wa_rs::GraphApiError::kind))
        .collect();
    Ok(refused) // empty: blocked. 131047: they have not written in the last 24 hours
}

/// A group chat the merchant opens: only a request id comes back.
pub async fn open_group(
    merchant: &Client,
    number: PhoneNumberId,
    subject: &str, // 1-128 characters
) -> wa_rs::Result<Option<String>> {
    let mut request = CreateGroup::new(subject);
    request.join_approval_mode = Some(JoinApprovalMode::ApprovalRequired);
    let created = merchant.groups(number).create(&request).await?; // not replayed after a timeout
    Ok(created.request_id) // match it in the `group_lifecycle_update` webhook
}

/// The webhook that completes `open_group`: request id, group id, invite link.
pub fn group_created(event: &WebhookEvent) -> Option<(&str, &GroupId, Option<&str>)> {
    let WebhookEvent::GroupUpdated { update, .. } = event else {
        return None;
    };
    if update.update_type != GroupUpdateType::GroupCreate || !update.errors.is_empty() {
        return None; // a failed creation carries `errors`
    }
    Some((
        update.request_id.as_deref()?,
        &update.group_id,
        update.invite_link.as_deref(), // or later: `client.group(id).invite_link()`
    ))
}

/// A customer calls, and nobody can take WhatsApp calls in the CMS (wa-rs
/// signals calls; it carries no audio): reject it, answer in the chat.
pub async fn decline_call(merchant: &Client, event: &WebhookEvent) -> wa_rs::Result<bool> {
    let WebhookEvent::CallUpdated {
        phone_number_id,
        call,
        ..
    } = event
    else {
        return Ok(false);
    };
    if call.event != CallEventType::Connect || call.direction != Some(CallDirection::UserInitiated)
    {
        return Ok(false);
    }
    merchant
        .calling(phone_number_id.clone())
        .reject(&call.id)
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use wa_rs::core::testing::ScriptedTransport;
    use wa_rs::webhooks::WebhookPayload;

    use super::*;

    const NUMBER: &str = "106540352242922";
    const CUSTOMER: &str = "US.13491208655302741918";

    fn merchant(transport: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(transport.clone())
            .access_token("MERCHANT-TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn events(field: &str, value: &Value) -> Vec<WebhookEvent> {
        let body = json!({"object": "whatsapp_business_account", "entry": [{"id": "102290129340398",
            "changes": [{"field": field, "value": value}]}]});
        WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
    }

    #[tokio::test]
    async fn blocking_by_bsuid_reports_who_could_not_be_blocked() {
        let transport = ScriptedTransport::new();
        // Meta's block-users mixed result, for a BSUID.
        transport.push_json(
            200,
            json!({"messaging_product": "whatsapp", "block_users": {
                "failed_users": [{"input": CUSTOMER, "user_id": CUSTOMER, "errors": [{
                    "message": "Re-engagement required", "code": 131047,
                    "error_data": {"details": "User has not messaged in the last 24 hours"}}]}]},
                "error": {"message": "(#139100) Failed to block/unblock users",
                    "type": "OAuthException", "code": 139100}}),
        );
        let refused = block_customer(
            &merchant(&transport),
            NUMBER.into(),
            Recipient::user(CUSTOMER),
        )
        .await
        .unwrap();
        assert_eq!(refused, [ErrorKind::CustomerServiceWindowClosed]);
        let request = transport.last_request().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(request.path(), "/v25.0/106540352242922/block_users");
        assert_eq!(request.bearer(), Some("MERCHANT-TOKEN"));
        assert_eq!(
            request.json().unwrap(),
            json!({"messaging_product": "whatsapp", "block_users": [{"user_id": CUSTOMER}]})
        );
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn a_group_is_created_by_webhook() {
        let transport = ScriptedTransport::new();
        transport.push_json(
            200,
            json!({"messaging_product": "whatsapp", "request_id": "6a1f2c9e5b7d4e0f"}),
        );
        let request_id = open_group(&merchant(&transport), NUMBER.into(), "Linen club")
            .await
            .unwrap();
        assert_eq!(request_id.as_deref(), Some("6a1f2c9e5b7d4e0f"));
        assert_eq!(
            transport.last_request().unwrap().json().unwrap(),
            json!({"messaging_product": "whatsapp", "subject": "Linen club",
                "join_approval_mode": "approval_required"})
        );
        assert_eq!(transport.remaining(), 0);

        // Meta's `group_lifecycle_update` example: one success, one failure.
        let updates = events(
            "group_lifecycle_update",
            &json!({"messaging_product": "whatsapp",
            "metadata": {"display_phone_number": "15550783881", "phone_number_id": NUMBER},
            "groups": [
                {"timestamp": "1750100000", "group_id": "Y2FwaV9ncm91cDox", "type": "group_create",
                 "request_id": "6a1f2c9e5b7d4e0f", "subject": "Linen club",
                 "invite_link": "https://chat.whatsapp.com/Ab12Cd34Ef56",
                 "join_approval_mode": "approval_required"},
                {"timestamp": "1750100100", "group_id": "Y2FwaV9ncm91cDoy", "type": "group_create",
                 "request_id": "7b2e3dae6c8e5f1a", "subject": "Linen club",
                 "errors": [{"code": 131000, "message": "Something went wrong"}]}
            ]}),
        );
        let created: Vec<_> = updates.iter().filter_map(group_created).collect();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, "6a1f2c9e5b7d4e0f");
        assert_eq!(created[0].1.as_str(), "Y2FwaV9ncm91cDox");
        assert_eq!(created[0].2, Some("https://chat.whatsapp.com/Ab12Cd34Ef56"));
    }

    #[tokio::test]
    async fn an_incoming_call_is_rejected_and_nothing_else() {
        let call = |direction: &str, event: &str| {
            events(
                "calls",
                &json!({"messaging_product": "whatsapp",
                    "metadata": {"display_phone_number": "16315553601", "phone_number_id": NUMBER},
                    "contacts": [{"profile": {"name": "Pablo Morales"}, "user_id": CUSTOMER}],
                    "calls": [{"id": "wacid.ABGGFjFVU2AfAgo6V-Hc5eCgK5Gh", "from_user_id": CUSTOMER,
                        "event": event, "timestamp": "1671644824", "direction": direction,
                        "session": {"sdp_type": "offer", "sdp": "v=0\r\n"}}]}),
            )
            .remove(0)
        };
        let transport = ScriptedTransport::new();
        transport.push_json(
            200,
            json!({"messaging_product": "whatsapp", "success": true}),
        );
        let merchant = merchant(&transport);

        assert!(
            !decline_call(&merchant, &call("BUSINESS_INITIATED", "connect"))
                .await
                .unwrap()
        );
        assert!(
            !decline_call(&merchant, &call("USER_INITIATED", "terminate"))
                .await
                .unwrap()
        );
        assert!(
            decline_call(&merchant, &call("USER_INITIATED", "connect"))
                .await
                .unwrap()
        );
        let request = transport.last_request().unwrap();
        assert_eq!(request.path(), "/v25.0/106540352242922/calls");
        assert_eq!(
            request.json().unwrap(),
            json!({"messaging_product": "whatsapp",
                "call_id": "wacid.ABGGFjFVU2AfAgo6V-Hc5eCgK5Gh", "action": "reject"})
        );
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(transport.remaining(), 0);
    }
}
