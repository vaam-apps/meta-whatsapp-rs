//! WABA analytics: messaging, conversation, pricing, template, call and group analytics.
//!
//! Docs: `analytics`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use serde::Deserialize;
use wa_core::Result;
use wa_core::ids::WabaId;

use crate::Client;

/// Entry point, see [`Client::analytics`].
#[derive(Debug, Clone)]
pub struct Analytics {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Analytics`] API for `waba_id`.
    pub fn analytics(&self, waba_id: impl Into<WabaId>) -> Analytics {
        Analytics {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

impl Analytics {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Get messaging analytics for this WABA.
    ///
    /// Returns the number and type of messages sent and delivered.
    pub async fn messaging(
        &self,
        start: i64,
        end: i64,
        granularity: Granularity,
    ) -> Result<MessagingAnalyticsResponse> {
        self.get_analytics(&format!(
            "analytics.start({}).end({}).granularity({})",
            start,
            end,
            granularity.as_str()
        ))
        .await
    }

    /// Get conversation analytics for this WABA.
    ///
    /// Returns cost and conversation information.
    pub async fn conversation(
        &self,
        start: i64,
        end: i64,
        granularity: &str,
    ) -> Result<ConversationAnalyticsResponse> {
        self.get_analytics(&format!(
            "conversation_analytics.start({}).end({}).granularity({})",
            start, end, granularity
        ))
        .await
    }

    /// Get template analytics for this WABA.
    pub async fn template(
        &self,
        start: i64,
        end: i64,
        granularity: &str,
    ) -> Result<TemplateAnalyticsResponse> {
        self.get_analytics(&format!(
            "template_analytics.start({}).end({}).granularity({})",
            start, end, granularity
        ))
        .await
    }

    /// Get call analytics for this WABA.
    pub async fn calls(
        &self,
        start: i64,
        end: i64,
        granularity: &str,
    ) -> Result<CallAnalyticsResponse> {
        self.get_analytics(&format!(
            "call_analytics.start({}).end({}).granularity({})",
            start, end, granularity
        ))
        .await
    }

    /// Get group analytics for this WABA.
    pub async fn groups(
        &self,
        start: i64,
        end: i64,
        granularity: &str,
    ) -> Result<GroupAnalyticsResponse> {
        self.get_analytics(&format!(
            "group_analytics.start({}).end({}).granularity({})",
            start, end, granularity
        ))
        .await
    }

    async fn get_analytics<T: serde::de::DeserializeOwned>(&self, fields: &str) -> Result<T> {
        self.client
            .get(&format!("{}", self.waba_id))
            .query("fields", fields)
            .context("analytics response")
            .send::<T>()
            .await
    }
}

/// Granularity option for messaging analytics.
#[derive(Debug, Clone, Copy)]
pub enum Granularity {
    /// Half-hour granularity.
    HalfHour,
    /// Daily granularity.
    Day,
    /// Monthly granularity.
    Month,
}

impl Granularity {
    fn as_str(&self) -> &'static str {
        match self {
            Self::HalfHour => "HALF_HOUR",
            Self::Day => "DAY",
            Self::Month => "MONTH",
        }
    }
}

/// Messaging analytics response.
#[derive(Debug, Clone, Deserialize)]
pub struct MessagingAnalyticsResponse {
    /// Analytics data.
    pub analytics: MessagingAnalytics,
}

/// Messaging analytics data.
#[derive(Debug, Clone, Deserialize)]
pub struct MessagingAnalytics {
    /// Phone numbers included.
    pub phone_numbers: Vec<String>,
    /// Country codes included.
    pub country_codes: Vec<String>,
    /// Granularity of data.
    pub granularity: String,
    /// Data points.
    pub data_points: Vec<MessagingDataPoint>,
}

/// Messaging data point.
#[derive(Debug, Clone, Deserialize)]
pub struct MessagingDataPoint {
    /// Start timestamp.
    pub start: i64,
    /// End timestamp.
    pub end: i64,
    /// Number of messages sent.
    pub sent: i64,
    /// Number of messages delivered.
    pub delivered: i64,
}

/// Conversation analytics response.
#[derive(Debug, Clone, Deserialize)]
pub struct ConversationAnalyticsResponse {
    /// Conversation analytics data.
    pub conversation_analytics: ConversationAnalytics,
}

/// Conversation analytics data.
#[derive(Debug, Clone, Deserialize)]
pub struct ConversationAnalytics {
    /// Data points.
    pub data: Vec<ConversationDataPoint>,
}

/// Conversation data point.
#[derive(Debug, Clone, Deserialize)]
pub struct ConversationDataPoint {
    /// Start timestamp.
    pub start: i64,
    /// End timestamp.
    pub end: i64,
    /// Number of conversations.
    #[serde(default)]
    pub conversation: i64,
    /// Cost of conversations.
    #[serde(default)]
    pub cost: f64,
}

/// Template analytics response.
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateAnalyticsResponse {
    /// Template analytics data.
    pub template_analytics: TemplateAnalytics,
}

/// Template analytics data.
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateAnalytics {
    /// Data points.
    pub data: Vec<TemplateDataPoint>,
}

/// Template analytics data point.
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateDataPoint {
    /// Start timestamp.
    pub start: i64,
    /// End timestamp.
    pub end: i64,
}

/// Call analytics response.
#[derive(Debug, Clone, Deserialize)]
pub struct CallAnalyticsResponse {
    /// Call analytics data.
    pub call_analytics: CallAnalytics,
}

/// Call analytics data.
#[derive(Debug, Clone, Deserialize)]
pub struct CallAnalytics {
    /// Data points.
    pub data: Vec<CallDataPoint>,
}

/// Call analytics data point.
#[derive(Debug, Clone, Deserialize)]
pub struct CallDataPoint {
    /// Start timestamp.
    pub start: i64,
    /// End timestamp.
    pub end: i64,
}

/// Group analytics response.
#[derive(Debug, Clone, Deserialize)]
pub struct GroupAnalyticsResponse {
    /// Group analytics data.
    pub group_analytics: GroupAnalytics,
}

/// Group analytics data.
#[derive(Debug, Clone, Deserialize)]
pub struct GroupAnalytics {
    /// Data points.
    pub data: Vec<GroupDataPoint>,
}

/// Group analytics data point.
#[derive(Debug, Clone, Deserialize)]
pub struct GroupDataPoint {
    /// Start timestamp.
    pub start: i64,
    /// End timestamp.
    pub end: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Client, RetryPolicy};
    use http::Method;
    use serde_json::json;
    use wa_core::testing::ScriptedTransport;

    #[tokio::test]
    async fn messaging_analytics() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "analytics": {
                    "phone_numbers": ["16505550111"],
                    "country_codes": ["US"],
                    "granularity": "DAY",
                    "data_points": [
                        {
                            "start": 1543543200,
                            "end": 1543629600,
                            "sent": 196093,
                            "delivered": 179715
                        }
                    ]
                },
                "id": "102290129340398"
            }),
        );
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let response = client
            .analytics("102290129340398")
            .messaging(1543543200, 1544148000, Granularity::Day)
            .await
            .unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/102290129340398");
        assert!(
            req.query("fields")
                .unwrap()
                .contains("analytics.start(1543543200).end(1544148000).granularity(DAY)")
        );

        assert_eq!(response.analytics.phone_numbers.len(), 1);
        assert_eq!(response.analytics.data_points.len(), 1);
        assert_eq!(t.remaining(), 0);
    }
}
