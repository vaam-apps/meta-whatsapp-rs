//! Reference code for the `meta-whatsapp-rs-errors` skill: deciding what a failed call
//! means from `ErrorKind`, never from the message text, and never resending
//! what may already have gone out.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use meta_whatsapp_rs::client::messages::Messages;
use meta_whatsapp_rs::prelude::*;

/// What the order service does next.
#[derive(Debug)]
pub enum Next {
    Accepted(MessageId), // wait for status webhooks
    FixInput(String),    // refused locally, nothing was sent
    FixTemplate,         // not approved in that language, or wrong parameters
    StopMarketing,       // opted out (131050) or per-user limit (131049): never retry
    Rejected(Error),     // Meta refused it: nothing went out
    Reconcile,           // may have been sent: check status webhooks first
}

/// Send an order update and classify the outcome.
pub async fn notify_shipped(messages: &Messages, to: Recipient, order_no: &str) -> Next {
    let template = TemplateMessage::new("order_shipped", "en_US").body([Parameter::text(order_no)]);
    let msg = OutboundMessage::template(to, template).callback_data(format!("order:{order_no}")); // echoed on status webhooks
    match messages.send(&msg).await {
        Ok(sent) => sent
            .message_id()
            .cloned()
            .map_or(Next::Reconcile, Next::Accepted),
        Err(Error::Validation(v)) => Next::FixInput(v.field),
        Err(e) => match e.kind() {
            ErrorKind::TemplateNotFound | ErrorKind::TemplateParameterMismatch => Next::FixTemplate,
            ErrorKind::MarketingOptedOut | ErrorKind::EcosystemEngagementLimit => {
                Next::StopMarketing
            }
            _ if !e.may_have_been_sent() => Next::Rejected(e),
            _ => Next::Reconcile,
        },
    }
}

/// A job queue's decision for a send that failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Resend {
    Never,          // nothing will change: fix the cause instead
    Later,          // Meta refused it for now (throttling, template sync)
    ReconcileFirst, // it may have been delivered: look for its status webhook
}

/// Resend only what Meta provably refused and may accept later.
pub fn after_failed_send(e: &Error) -> Resend {
    if matches!(e, Error::Validation(_)) {
        return Resend::Never; // refused locally: nothing was sent
    }
    if e.may_have_been_sent() {
        return Resend::ReconcileFirst;
    }
    if e.is_retryable() {
        Resend::Later
    } else {
        Resend::Never // includes 131049, 131050 and 131048: never auto-retry
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use meta_whatsapp_rs::core::testing::ScriptedTransport;

    use super::*;

    fn graph_error(code: i64) -> Value {
        json!({"error": {"message": format!("(#{code}) test"), "type": "OAuthException", "code": code}})
    }

    fn messages(transport: &ScriptedTransport) -> Messages {
        let client = Client::builder()
            .transport(transport.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy {
                max_retries: 3,
                base_delay: Duration::ZERO,
                max_delay: Duration::ZERO,
            })
            .build()
            .unwrap();
        client.messages("106540352242922")
    }

    fn to() -> Recipient {
        Recipient::phone("+16505551234")
    }

    #[tokio::test]
    async fn an_opt_out_stops_marketing_and_is_not_resent() {
        let transport = ScriptedTransport::new();
        transport.push_json(400, graph_error(131050));
        let next = notify_shipped(&messages(&transport), to(), "860198").await;
        assert!(matches!(next, Next::StopMarketing), "{next:?}");
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn a_5xx_on_a_send_is_returned_not_replayed() {
        let transport = ScriptedTransport::new();
        transport.push_json(500, graph_error(131000));
        let next = notify_shipped(&messages(&transport), to(), "860198").await;
        assert!(matches!(next, Next::Reconcile), "{next:?}");
        // One request although the policy allows three retries: the
        // message may already be on its way.
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(transport.remaining(), 0);
    }

    #[tokio::test]
    async fn throttling_is_replayed_by_the_client() {
        let transport = ScriptedTransport::new();
        transport.push_json(400, graph_error(130429));
        transport.push_json(200, json!({"messages": [{"id": "wamid.2"}]}));
        let next = notify_shipped(&messages(&transport), to(), "860198").await;
        assert!(
            matches!(next, Next::Accepted(ref id) if id.as_str() == "wamid.2"),
            "{next:?}"
        );
        assert_eq!(transport.requests().len(), 2);
        assert_eq!(transport.remaining(), 0);
    }

    #[test]
    fn the_queue_rule() {
        let refused = |code: i64, status: u16| {
            let mut e = meta_whatsapp_rs::GraphApiError::new(code, "test");
            e.http_status = Some(status);
            Error::from(e)
        };
        assert_eq!(after_failed_send(&refused(130429, 400)), Resend::Later);
        assert_eq!(after_failed_send(&refused(131049, 400)), Resend::Never);
        assert_eq!(after_failed_send(&refused(131050, 400)), Resend::Never);
        assert_eq!(
            after_failed_send(&refused(131000, 500)),
            Resend::ReconcileFirst
        );
        let timeout = Error::from(meta_whatsapp_rs::core::error::TransportError::Timeout);
        assert_eq!(after_failed_send(&timeout), Resend::ReconcileFirst);
        let local = Error::from(meta_whatsapp_rs::core::error::ValidationError::new("to", "empty"));
        assert_eq!(after_failed_send(&local), Resend::Never);
    }
}
