//! Business portfolio accessor: the WABAs shared with (client) or owned by
//! a business, and its messaging customer bases.

use futures::Stream;
use serde::{Deserialize, Serialize};
use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::BusinessId;
use wa_core::paging::Page;

use super::types::{BusinessInfo, Filter, MessagingCustomerBase, WabaInfo, WabaSort};
use crate::phone_numbers::fields_param;
use crate::{Client, GraphRequest};

/// Entry point, see [`Client::business`].
#[derive(Debug, Clone)]
pub struct Business {
    client: Client,
    business_id: BusinessId,
}

impl Client {
    /// [`Business`] API for a business portfolio (yours, as a partner, to
    /// list client WABAs; or a customer's).
    pub fn business(&self, business_id: impl Into<BusinessId>) -> Business {
        Business {
            client: self.clone(),
            business_id: business_id.into(),
        }
    }
}

/// Options for the client/owned WABA lists (`solution-providers/manage-accounts`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct WabaListQuery {
    /// Fields to return (empty = Meta's defaults).
    pub fields: Vec<String>,
    /// `filtering` conditions, e.g. [`Filter::created_after`].
    pub filtering: Vec<Filter>,
    /// Sort by creation time.
    pub sort: Option<WabaSort>,
}

impl WabaListQuery {
    /// Meta's defaults.
    pub fn new() -> Self {
        Self::default()
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

    /// Add a filter.
    #[must_use]
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filtering.push(filter);
        self
    }

    /// Sort order.
    #[must_use]
    pub fn sort(mut self, sort: WabaSort) -> Self {
        self.sort = Some(sort);
        self
    }
}

#[derive(Serialize)]
struct CustomerBaseBody<'a> {
    messaging_customer_base_name: &'a str,
}

/// `{"messaging_customer_base_id": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreatedMessagingCustomerBase {
    /// The new messaging customer base id.
    pub messaging_customer_base_id: String,
}

#[derive(Deserialize)]
struct CustomerBases {
    #[serde(default)]
    messaging_customer_bases: Vec<MessagingCustomerBase>,
}

impl Business {
    /// The id this API is scoped to.
    pub fn id(&self) -> &BusinessId {
        &self.business_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `GET /{BUSINESS_ID}` with `fields` (e.g. `id,name,timezone_id`).
    pub async fn get(&self, fields: &[&str]) -> Result<BusinessInfo> {
        self.client
            .get_at(&[self.business_id.as_str()])
            .query_opt("fields", fields_param(fields))
            .context("business portfolio")
            .send()
            .await
    }

    fn waba_list(&self, edge: &str, query: &WabaListQuery) -> GraphRequest {
        let mut req = self
            .client
            .get_at(&[self.business_id.as_str(), edge])
            .query_opt(
                "fields",
                (!query.fields.is_empty()).then(|| query.fields.join(",")),
            )
            .context("WhatsApp Business Accounts");
        if let Some(sort) = query.sort {
            req = req.query_struct(&SortParam { sort });
        }
        if !query.filtering.is_empty() {
            req = req.query_json("filtering", &query.filtering);
        }
        req
    }

    /// `GET /{BUSINESS_ID}/client_whatsapp_business_accounts`: WABAs shared
    /// with this (partner) business, e.g. by Embedded Signup. Useful as a
    /// periodic reconciliation next to `debug_token`.
    pub async fn client_whatsapp_business_accounts(
        &self,
        query: &WabaListQuery,
    ) -> Result<Page<WabaInfo>> {
        self.waba_list("client_whatsapp_business_accounts", query)
            .send()
            .await
    }

    /// All client WABAs, following cursors.
    pub fn client_whatsapp_business_accounts_stream(
        &self,
        query: &WabaListQuery,
    ) -> impl Stream<Item = Result<WabaInfo>> + Send + 'static + use<> {
        self.waba_list("client_whatsapp_business_accounts", query)
            .paginate()
    }

    /// `GET /{BUSINESS_ID}/owned_whatsapp_business_accounts`: WABAs this
    /// business owns.
    pub async fn owned_whatsapp_business_accounts(
        &self,
        query: &WabaListQuery,
    ) -> Result<Page<WabaInfo>> {
        self.waba_list("owned_whatsapp_business_accounts", query)
            .send()
            .await
    }

    /// All owned WABAs, following cursors.
    pub fn owned_whatsapp_business_accounts_stream(
        &self,
        query: &WabaListQuery,
    ) -> impl Stream<Item = Result<WabaInfo>> + Send + 'static + use<> {
        self.waba_list("owned_whatsapp_business_accounts", query)
            .paginate()
    }

    /// `POST /{BUSINESS_ID}/messaging_customer_base` (`in-app-signup`):
    /// create a messaging customer base for In-App Signup subscribers.
    pub async fn create_messaging_customer_base(
        &self,
        name: &str,
    ) -> Result<CreatedMessagingCustomerBase> {
        if name.trim().is_empty() {
            return Err(ValidationError::new("messaging_customer_base_name", "required").into());
        }
        self.client
            .post_at(&[self.business_id.as_str(), "messaging_customer_base"])
            .json(&CustomerBaseBody {
                messaging_customer_base_name: name,
            })
            .context("create messaging customer base response")
            .send()
            .await
    }

    /// `GET /{BUSINESS_ID}/messaging_customer_base`.
    pub async fn messaging_customer_bases(&self) -> Result<Vec<MessagingCustomerBase>> {
        let bases: CustomerBases = self
            .client
            .get_at(&[self.business_id.as_str(), "messaging_customer_base"])
            .context("messaging customer bases")
            .send()
            .await?;
        Ok(bases.messaging_customer_bases)
    }
}

#[derive(Serialize)]
struct SortParam {
    sort: WabaSort,
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wa_core::testing::ScriptedTransport;

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

    fn manage_accounts_example() -> serde_json::Value {
        // solution-providers/manage-accounts, "Get list of shared WABAs".
        json!({
          "data": [
            {"id": "1906385232743451", "name": "My WhatsApp Business Account", "currency": "USD", "timezone_id": "1", "message_template_namespace": "abcdefghijk_12lmnop"},
            {"id": "1972385232742141", "name": "My Regional Account", "currency": "INR", "timezone_id": "5", "message_template_namespace": "12abcdefghijk_34lmnop"}
          ],
          "paging": {"cursors": {"before": "abcdefghij", "after": "klmnopqr"}}
        })
    }

    #[tokio::test]
    async fn client_wabas_parse_docs_example() {
        let t = ScriptedTransport::new();
        t.push_json(200, manage_accounts_example());
        let page = client(&t)
            .business("805021500648488")
            .client_whatsapp_business_accounts(&WabaListQuery::new())
            .await
            .unwrap();
        assert_eq!(page.data.len(), 2);
        assert_eq!(page.data[1].currency.as_deref(), Some("INR"));
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(
            req.path(),
            "/v25.0/805021500648488/client_whatsapp_business_accounts"
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn owned_wabas_filter_and_sort_encode_like_the_docs() {
        let t = ScriptedTransport::new();
        t.push_json(200, manage_accounts_example());
        client(&t)
            .business("805021500648488")
            .owned_whatsapp_business_accounts(
                &WabaListQuery::new()
                    .filter(Filter::created_after(1604962813))
                    .sort(WabaSort::CreationTimeAscending),
            )
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(
            req.path(),
            "/v25.0/805021500648488/owned_whatsapp_business_accounts"
        );
        assert_eq!(
            req.query("filtering").as_deref(),
            Some(r#"[{"field":"creation_time","operator":"GREATER_THAN","value":"1604962813"}]"#)
        );
        assert_eq!(
            req.query("sort").as_deref(),
            Some("creation_time_ascending")
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn owned_wabas_stream_paginates() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "1"}], "paging": {"cursors": {"after": "a"}, "next": "https://graph.facebook.com/x"}}),
        );
        t.push_json(200, json!({"data": [{"id": "2"}]}));
        let ids: Vec<String> = client(&t)
            .business("B")
            .owned_whatsapp_business_accounts_stream(&WabaListQuery::new())
            .map(|w| w.unwrap().id.into_inner())
            .collect()
            .await;
        assert_eq!(ids, vec!["1", "2"]);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn messaging_customer_bases() {
        // in-app-signup, "Create/List messaging customer base".
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"messaging_customer_base_id": "456789012345678"}),
        );
        t.push_json(
            200,
            json!({"messaging_customer_bases": [{"id": "456789012345678", "name": "Summer 2026 Subscribers"}]}),
        );
        let b = client(&t).business("109876543210");
        let created = b
            .create_messaging_customer_base("Summer 2026 Subscribers")
            .await
            .unwrap();
        assert_eq!(created.messaging_customer_base_id, "456789012345678");
        let bases = b.messaging_customer_bases().await.unwrap();
        assert_eq!(bases[0].name.as_deref(), Some("Summer 2026 Subscribers"));
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::POST);
        assert_eq!(
            reqs[0].path(),
            "/v25.0/109876543210/messaging_customer_base"
        );
        assert_eq!(
            reqs[0].json(),
            Some(json!({"messaging_customer_base_name": "Summer 2026 Subscribers"}))
        );
        assert_eq!(reqs[1].method, Method::GET);
        assert_eq!(t.remaining(), 0);
        assert!(b.create_messaging_customer_base(" ").await.is_err());
        assert_eq!(t.requests().len(), 2);
    }

    #[tokio::test]
    async fn get_business_portfolio() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"id": "2729063490586005", "name": "Wind & Wool", "timezone_id": 1}),
        );
        let info = client(&t)
            .business("2729063490586005")
            .get(&["id", "name", "timezone_id"])
            .await
            .unwrap();
        assert_eq!(info.timezone_id.as_deref(), Some("1"));
        assert_eq!(
            t.last_request().unwrap().query("fields").as_deref(),
            Some("id,name,timezone_id")
        );
        assert_eq!(t.remaining(), 0);
    }
}
