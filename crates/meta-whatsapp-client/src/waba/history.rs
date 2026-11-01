//! The delivery events of a message history entry
//! (`reference/message-history/whatsapp-business-message-history-events-api`).

use futures::Stream;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::paging::Page;
use serde::{Deserialize, Serialize};

use super::activity::ApplicationRef;
use super::{check_limit, fields_query};
use crate::request::{paginate_or_error, reject_cursors};
use crate::templates::macros::string_enum;
use crate::{Client, GraphRequest};

string_enum! {
    /// A message's delivery status (`WhatsAppMessageDeliveryStatus`).
    pub enum DeliveryStatus {
        /// Accepted.
        Accepted => "ACCEPTED",
        /// Delivered.
        Delivered => "DELIVERED",
        /// Failed.
        Error => "ERROR",
        /// Read.
        Read => "READ",
        /// Sent.
        Sent => "SENT",
    }
}

string_enum! {
    /// Whether the webhook for a delivery event went through
    /// (`webhook_update_state`).
    pub enum WebhookUpdateState {
        /// It failed.
        Error => "ERROR",
        /// It went through.
        Success => "SUCCESS",
    }
}

/// One delivery status occurrence
/// (`WhatsAppBusinessMessageDeliveryStatusOccurrence`). The reference
/// requires `id`, `delivery_status` and `occurrence_timestamp` (Meta's
/// defaults); the rest come with `fields`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct DeliveryStatusOccurrence {
    /// The occurrence's id.
    pub id: String,
    /// The status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<DeliveryStatus>,
    /// The error, when the delivery failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_description: Option<String>,
    /// When the status occurred, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence_timestamp: Option<i64>,
    /// When the status was recorded, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_timestamp: Option<i64>,
    /// The app that processed the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<ApplicationRef>,
    /// Whether its webhook went through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_update_state: Option<WebhookUpdateState>,
    /// The webhook that received the update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_uri: Option<String>,
}

/// One item of a message history's events
/// (`WhatsAppMessageHistoryEventsEdge`): the occurrence and its cursor.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct MessageHistoryEvent {
    /// This item's cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// The occurrence.
    pub node: DeliveryStatusOccurrence,
}

/// Options for [`MessageHistory::events`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListMessageHistoryEvents {
    /// Only events of this status (`delivery_status`).
    pub delivery_status: Option<DeliveryStatus>,
    /// Fields to return, as Meta writes them (empty = Meta's defaults,
    /// `cursor` and `node{id,delivery_status,occurrence_timestamp}`), e.g.
    /// `node{id,delivery_status,error_description,webhook_update_state}`.
    pub fields: Vec<String>,
    /// Page size, 1–100 (Meta's default: 25).
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`; for the
    /// one-page method only, the stream manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl ListMessageHistoryEvents {
    /// Meta's defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Only events of this status.
    #[must_use]
    pub fn delivery_status(mut self, status: DeliveryStatus) -> Self {
        self.delivery_status = Some(status);
        self
    }

    /// Fields to return.
    #[must_use]
    pub fn fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Page size (1–100).
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Continue after this cursor.
    #[must_use]
    pub fn after(mut self, cursor: impl Into<String>) -> Self {
        self.after = Some(cursor.into());
        self
    }

    /// Go back before this cursor.
    #[must_use]
    pub fn before(mut self, cursor: impl Into<String>) -> Self {
        self.before = Some(cursor.into());
        self
    }
}

/// Entry point, see [`Client::message_history`].
#[derive(Debug, Clone)]
pub struct MessageHistory {
    client: Client,
    history_id: String,
}

impl Client {
    /// [`MessageHistory`] API for a message history entry's id (see the
    /// [module docs](crate::waba#message-history-ids-and-campaign-schedules) on where it comes from).
    pub fn message_history(&self, history_id: impl Into<String>) -> MessageHistory {
        MessageHistory {
            client: self.clone(),
            history_id: history_id.into(),
        }
    }
}

impl MessageHistory {
    /// The id this API is scoped to.
    pub fn id(&self) -> &str {
        &self.history_id
    }

    fn events_request(&self, query: &ListMessageHistoryEvents) -> Result<GraphRequest> {
        if self.history_id.trim().is_empty() {
            return Err(ValidationError::new("message_history_id", "required").into());
        }
        check_limit(query.limit)?;
        Ok(self
            .client
            .get_at(&[self.history_id.as_str(), "events"])
            .query_opt(
                "delivery_status",
                query.delivery_status.as_ref().map(DeliveryStatus::as_str),
            )
            .query_opt("fields", fields_query(&query.fields))
            .query_opt("limit", query.limit)
            .context("message history events"))
    }

    /// `GET /{MESSAGE_HISTORY_ID}/events`, one page: the delivery events of
    /// this message history entry, optionally of one status. The next
    /// page: the same query with `after` set to this page's
    /// [`Page::next_cursor`].
    pub async fn events(
        &self,
        query: &ListMessageHistoryEvents,
    ) -> Result<Page<MessageHistoryEvent>> {
        self.events_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// All events, following cursors. The stream manages them itself: a
    /// query with `after` or `before` set is refused (the stream's single
    /// item is that validation error).
    pub fn events_stream(
        &self,
        query: &ListMessageHistoryEvents,
    ) -> impl Stream<Item = Result<MessageHistoryEvent>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.events_request(query)),
        )
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use meta_whatsapp_core::{Error, ErrorKind};
    use pretty_assertions::assert_eq;
    use serde_json::json;

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

    /// `MessageHistoryEventsResponse` with every documented field (the page
    /// prints no example; values ours).
    fn events_example() -> serde_json::Value {
        json!({
            "data": [
                {
                    "cursor": "MAZDZD",
                    "node": {
                        "id": "1029384756",
                        "delivery_status": "ERROR",
                        "error_description": "Message undeliverable",
                        "occurrence_timestamp": 1_767_225_600,
                        "status_timestamp": 1_767_225_601,
                        "application": {"id": "670843887433847", "name": "JaspersMarket"},
                        "webhook_update_state": "SUCCESS",
                        "webhook_uri": "https://example.com/webhooks"
                    }
                },
                {"node": {"id": "1029384757", "delivery_status": "READ", "occurrence_timestamp": 1_767_225_700}}
            ],
            "paging": {"cursors": {"before": "MAZDZD", "after": "MQZDZD"}}
        })
    }

    #[tokio::test]
    async fn events_send_the_documented_query_and_parse_every_field() {
        let t = ScriptedTransport::new();
        t.push_json(200, events_example());
        let page = client(&t)
            .message_history("5544332211")
            .events(
                &ListMessageHistoryEvents::new()
                    .delivery_status(DeliveryStatus::Error)
                    .fields(["cursor", "node{id,delivery_status,error_description,occurrence_timestamp,status_timestamp,application,webhook_update_state,webhook_uri}"])
                    .limit(25)
                    .after("MQZDZD"),
            )
            .await
            .unwrap();
        let first = &page.data[0];
        assert_eq!(first.cursor.as_deref(), Some("MAZDZD"));
        assert_eq!(first.node.id, "1029384756");
        assert_eq!(first.node.delivery_status, Some(DeliveryStatus::Error));
        assert_eq!(
            first.node.error_description.as_deref(),
            Some("Message undeliverable")
        );
        assert_eq!(first.node.status_timestamp, Some(1_767_225_601));
        assert_eq!(
            first.node.webhook_update_state,
            Some(WebhookUpdateState::Success)
        );
        assert_eq!(
            page.data[1].node.delivery_status,
            Some(DeliveryStatus::Read)
        );
        assert_eq!(
            serde_json::to_value(&page.data).unwrap(),
            events_example()["data"]
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/5544332211/events");
        assert_eq!(req.query("delivery_status").as_deref(), Some("ERROR"));
        assert_eq!(
            req.query("fields").as_deref(),
            Some(
                "cursor,node{id,delivery_status,error_description,occurrence_timestamp,status_timestamp,application,webhook_update_state,webhook_uri}"
            )
        );
        assert_eq!(req.query("limit").as_deref(), Some("25"));
        assert_eq!(req.query("after").as_deref(), Some("MQZDZD"));
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn events_check_their_input_and_stream() {
        let t = ScriptedTransport::new();
        let c = client(&t);
        assert!(matches!(
            c.message_history(" ").events(&ListMessageHistoryEvents::new()).await,
            Err(Error::Validation(v)) if v.field == "message_history_id"
        ));
        for limit in [0, 101] {
            assert!(matches!(
                c.message_history("1").events(&ListMessageHistoryEvents::new().limit(limit)).await,
                Err(Error::Validation(v)) if v.field == "limit"
            ));
        }
        let refused: Vec<_> = c
            .message_history("1")
            .events_stream(&ListMessageHistoryEvents::new().before("x"))
            .collect()
            .await;
        assert!(
            matches!(&refused[..], [Err(Error::Validation(_))]),
            "{refused:?}"
        );
        assert!(t.requests().is_empty());

        t.push_json(
            200,
            json!({"data": [{"node": {"id": "1"}}], "paging": {"cursors": {"after": "a"}, "next": "https://graph.facebook.com/x"}}),
        );
        t.push_json(200, json!({"data": [{"node": {"id": "2"}}]}));
        let ids: Vec<String> = c
            .message_history("a/b")
            .events_stream(&ListMessageHistoryEvents::new().delivery_status(DeliveryStatus::Sent))
            .map(|e| e.unwrap().node.id)
            .collect()
            .await;
        assert_eq!(ids, ["1", "2"]);
        let reqs = t.requests();
        assert_eq!(reqs[0].path(), "/v25.0/a%2Fb/events");
        assert_eq!(reqs[1].query("after").as_deref(), Some("a"));
        assert_eq!(reqs[1].query("delivery_status").as_deref(), Some("SENT"));
        assert_eq!(t.remaining(), 0);

        // The page's 404 example.
        t.push_json(
            404,
            json!({"error": {"message": "WhatsApp Business Message History not found", "type": "GraphMethodException", "code": 803, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
        );
        let err = c
            .message_history("1")
            .events(&ListMessageHistoryEvents::new())
            .await
            .unwrap_err();
        assert_eq!(err.graph().map(|g| g.code), Some(803));
        assert_ne!(err.kind(), ErrorKind::Authentication);
        assert_eq!(t.remaining(), 0);
    }
}
