//! Reference code for the Multi-Partner Solutions part of the
//! `meta-whatsapp-rs-embedded-signup` skill: check a solution before
//! accepting it (an accepted solution cannot be declined), read an
//! onboarded client's business token through the solution, and wait for
//! a client to accept a migration intent.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::partners::solutions::PermissionTask;
use meta_whatsapp_rs::core::ids::{AppId, BusinessId, MigrationIntentId, SolutionId};
use meta_whatsapp_rs::prelude::*;

/// Accept `solution` only if it invites `our_app` with `MESSAGING`, as
/// agreed with the other partner. `false`: not accepted (decline it with
/// `reject`, or ask your partner for a new one). Your system user token.
pub async fn accept_if_agreed(
    client: &Client,
    system_token: AccessToken,
    solution: &SolutionId,
    our_app: &AppId,
) -> meta_whatsapp_rs::Result<bool> {
    let system = client.with_token(system_token);
    let api = system.partner_solutions();
    let fields = [
        "name",
        "owner_permissions",
        "partners{partner_permissions,partner_app}",
    ];
    let info = api.get(solution, &fields).await?;
    let agreed = info.partners.iter().flat_map(|p| &p.data).any(|partner| {
        let ours = partner.partner_app.as_ref().and_then(|a| a.id.as_ref()) == Some(our_app);
        let messaging = partner
            .partner_permissions
            .as_ref()
            .is_some_and(|tasks| tasks.contains(&PermissionTask::Messaging));
        ours && messaging
    });
    if !agreed {
        return Ok(false);
    }
    // Cannot be undone, and never retried.
    api.accept(solution).await?;
    Ok(true)
}

/// A client onboarded through `solution` (their business portfolio id is
/// in the `account_update` webhook of their onboarding), as a client that
/// authenticates with their business token. Keep the token as you keep
/// Embedded Signup's (the token vault).
pub async fn client_business(
    client: &Client,
    system_token: AccessToken,
    solution: &SolutionId,
    customer: &BusinessId,
) -> meta_whatsapp_rs::Result<Client> {
    let system = client.with_token(system_token);
    let token = system
        .partner_solutions()
        .access_token(solution, customer)
        .await?;
    Ok(client.with_token(token.access_token))
}

/// Whether the client accepted the migration intent a Tech Provider sent
/// you, read with the client's business token: go on (subscribe to their
/// WABA's webhooks, share your credit line) only when it did.
pub async fn migration_accepted(
    business: &Client,
    intent: &MigrationIntentId,
) -> meta_whatsapp_rs::Result<bool> {
    let info = business.migration_intents().get(intent, &[]).await?;
    Ok(info.is_accepted())
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use serde_json::json;

    use super::*;

    // The guide's example ids.
    const SOLUTION: &str = "795033096057724";
    const OUR_APP: &str = "21202248997039";

    fn client(transport: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(transport.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn solution_answer(partner_permissions: &[&str]) -> serde_json::Value {
        json!({
            "name": "Social OVD with Lucky Shrub",
            "owner_permissions": ["MANAGE", "DEVELOP"],
            "partners": {"data": [{
                "partner_permissions": partner_permissions,
                "partner_app": {"name": "Lucky Shrub", "id": OUR_APP},
                "id": "795033099391057"
            }]},
            "id": SOLUTION
        })
    }

    #[tokio::test]
    async fn accepts_only_what_was_agreed() {
        let t = ScriptedTransport::new();
        t.push_json(200, solution_answer(&["MESSAGING", "MANAGE"]));
        t.push_json(200, json!({"success": true}));
        let solution = SolutionId::new(SOLUTION).unwrap();
        let ours = AppId::new(OUR_APP).unwrap();
        let accepted = accept_if_agreed(&client(&t), "SYSTEM".into(), &solution, &ours)
            .await
            .unwrap();
        assert!(accepted);
        let requests = t.requests();
        assert_eq!(
            requests[1].path(),
            format!("/v25.0/{SOLUTION}/accept").as_str()
        );
        assert_eq!(requests[1].bearer(), Some("SYSTEM"));
        assert_eq!(t.remaining(), 0);

        // Without MESSAGING for us: read, not accepted.
        let t = ScriptedTransport::new();
        t.push_json(200, solution_answer(&["MANAGE"]));
        let accepted = accept_if_agreed(&client(&t), "SYSTEM".into(), &solution, &ours)
            .await
            .unwrap();
        assert!(!accepted);
        assert_eq!(t.requests().len(), 1);
    }

    #[tokio::test]
    async fn reads_the_business_token_and_the_migration_intent() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"data": [{"access_token": "EAAB-CLIENT"}]}));
        t.push_json(200, json!({"id": "1", "status": "ACCEPTED"}));
        let business = client_business(
            &client(&t),
            "SYSTEM".into(),
            &SolutionId::new("303610109049230").unwrap(),
            &BusinessId::new("2729063490586005").unwrap(),
        )
        .await
        .unwrap();
        assert!(
            migration_accepted(&business, &MigrationIntentId::new("1"))
                .await
                .unwrap()
        );
        let requests = t.requests();
        assert_eq!(
            requests[0].query("business_id").as_deref(),
            Some("2729063490586005")
        );
        assert_eq!(requests[1].bearer(), Some("EAAB-CLIENT"));
        assert_eq!(t.remaining(), 0);
    }
}
