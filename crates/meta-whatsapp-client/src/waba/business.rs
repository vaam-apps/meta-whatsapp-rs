//! Business portfolio accessor: the WABAs shared with (client) or owned by
//! a business, its messaging customer bases, and the parent BSUID account
//! it is enrolled in.

use futures::Stream;
use http::Method;
use meta_whatsapp_core::error::{TransportError, ValidationError};
use meta_whatsapp_core::ids::BusinessId;
use meta_whatsapp_core::paging::Page;
use meta_whatsapp_core::{Error, Result};
use serde::{Deserialize, Serialize};
use url::Url;

use super::types::{BusinessInfo, Filter, MessagingCustomerBase, WabaInfo, WabaSort};
use crate::phone_numbers::fields_param;
use crate::request::{
    PARENT_BSUID_ACCOUNTS_EDGE, PARENT_BSUID_ACCOUNTS_HOST, paginate_or_error, reject_cursors,
};
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
    /// Cursor from a previous page's `paging.cursors.after`
    /// ([`Page::next_cursor`]); for the one-page methods only, the streams
    /// manage their own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
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

/// The parent BSUID account a business portfolio is enrolled in, from
/// [`Business::parent_bsuid_account`] (`business-scoped-user-ids`,
/// § Get parent BSUID account).
///
/// Every portfolio enrolled in one parent BSUID account sees a WhatsApp
/// user under the same parent BSUID (`US.ENT.…`, a webhook's
/// `parent_user_id`), and any of their business phone numbers can message
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ParentBsuidAccount {
    /// The id of the parent BSUID account the enrolled portfolios share.
    pub parent_bsuid_account_id: String,
    /// The business portfolios enrolled in it: any business phone number
    /// in them can use the account's parent BSUIDs.
    #[serde(default)]
    pub enrolled_business_portfolios: Vec<BusinessId>,
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
        self.waba_page("client_whatsapp_business_accounts", query)
            .send()
            .await
    }

    /// All client WABAs, following cursors. The stream manages them
    /// itself: a query with `after` or `before` set is refused (the
    /// stream's single item is that validation error).
    pub fn client_whatsapp_business_accounts_stream(
        &self,
        query: &WabaListQuery,
    ) -> impl Stream<Item = Result<WabaInfo>> + Send + 'static + use<> {
        self.waba_stream("client_whatsapp_business_accounts", query)
    }

    /// `GET /{BUSINESS_ID}/owned_whatsapp_business_accounts`: WABAs this
    /// business owns.
    pub async fn owned_whatsapp_business_accounts(
        &self,
        query: &WabaListQuery,
    ) -> Result<Page<WabaInfo>> {
        self.waba_page("owned_whatsapp_business_accounts", query)
            .send()
            .await
    }

    /// All owned WABAs, following cursors. The stream manages them itself:
    /// a query with `after` or `before` set is refused (the stream's single
    /// item is that validation error).
    pub fn owned_whatsapp_business_accounts_stream(
        &self,
        query: &WabaListQuery,
    ) -> impl Stream<Item = Result<WabaInfo>> + Send + 'static + use<> {
        self.waba_stream("owned_whatsapp_business_accounts", query)
    }

    /// One page of `edge`, at the query's cursor.
    fn waba_page(&self, edge: &str, query: &WabaListQuery) -> GraphRequest {
        self.waba_list(edge, query)
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
    }

    /// Every page of `edge`; the stream owns the cursors.
    fn waba_stream(
        &self,
        edge: &str,
        query: &WabaListQuery,
    ) -> impl Stream<Item = Result<WabaInfo>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .map(|()| self.waba_list(edge, query)),
        )
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

    /// `GET https://api.facebook.com/{BUSINESS_ID}/parent-bsuid-accounts`
    /// (`business-scoped-user-ids`, § Get parent BSUID account): the parent
    /// BSUID account this portfolio is enrolled in, and every portfolio
    /// enrolled in it.
    ///
    /// Meta serves this API from `api.facebook.com`, without an API
    /// version, not from Graph (the page's changelog, May 28, 2026,
    /// corrected the host). It goes there with the client's token, whatever
    /// [`crate::ClientBuilder::endpoint`] names: behind a Graph proxy too.
    /// That URL is the only one on that host the client sends a token to
    /// (`GraphRequest`'s credential rules); an egress proxy that must see
    /// every request belongs in the [`HttpTransport`](meta_whatsapp_core::transport::HttpTransport).
    ///
    /// The credential rule takes a business id of ASCII digits only (the
    /// shape of every business id Meta's pages show), so anything else is
    /// refused here, before any request ([`ValidationError`] on
    /// `business_id`). Replayed on transient errors, as every `GET` is
    /// ([`crate::RetryPolicy`]).
    /// What Meta answers for a portfolio that is not enrolled is not
    /// documented: an answer without `parent_bsuid_account_id` is a
    /// decode error.
    pub async fn parent_bsuid_account(&self) -> Result<ParentBsuidAccount> {
        let url = parent_bsuid_accounts_url(&self.business_id)?;
        self.client
            .request_url(Method::GET, url)
            .context("parent BSUID account")
            .send()
            .await
    }
}

/// `https://api.facebook.com/{business_id}/parent-bsuid-accounts`, for a
/// business id of digits only.
fn parent_bsuid_accounts_url(business_id: &BusinessId) -> Result<Url> {
    let id = business_id.as_str();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ValidationError::new(
            "business_id",
            "must be a business portfolio id (digits only)",
        )
        .into());
    }
    let build = |why: &str| {
        Error::Transport(TransportError::Build(format!(
            "parent BSUID accounts URL: {why}"
        )))
    };
    let mut url = Url::parse(&format!("https://{PARENT_BSUID_ACCOUNTS_HOST}/"))
        .map_err(|e| build(&e.to_string()))?;
    url.path_segments_mut()
        .map_err(|()| build("not a hierarchical URL"))?
        .pop_if_empty()
        .push(id)
        .push(PARENT_BSUID_ACCOUNTS_EDGE);
    Ok(url)
}

#[derive(Serialize)]
struct SortParam {
    sort: WabaSort,
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use meta_whatsapp_core::testing::ScriptedTransport;
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
        assert_eq!(t.requests()[1].query("after").as_deref(), Some("a"));
        assert_eq!(t.remaining(), 0);
    }

    /// The docs example's cursors, fed back: the one-page methods send
    /// them, the streams refuse them before any request.
    #[tokio::test]
    async fn client_and_owned_wabas_take_cursors_in_their_query() {
        let t = ScriptedTransport::new();
        t.push_json(200, manage_accounts_example());
        t.push_json(200, manage_accounts_example());
        let b = client(&t).business("805021500648488");
        let page = b
            .client_whatsapp_business_accounts(&WabaListQuery::new())
            .await
            .unwrap();
        let after = page.paging.unwrap().cursors.unwrap().after.unwrap();
        b.owned_whatsapp_business_accounts(&WabaListQuery::new().after(after).before("abcdefghij"))
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.query("after").as_deref(), Some("klmnopqr"));
        assert_eq!(req.query("before").as_deref(), Some("abcdefghij"));
        assert_eq!(t.requests()[0].query("after"), None);
        assert_eq!(t.remaining(), 0);
        for query in [
            WabaListQuery::new().after("klmnopqr"),
            WabaListQuery::new().before("abcdefghij"),
        ] {
            let client_wabas: Vec<_> = b
                .client_whatsapp_business_accounts_stream(&query)
                .collect()
                .await;
            let owned: Vec<_> = b
                .owned_whatsapp_business_accounts_stream(&query)
                .collect()
                .await;
            for refused in [client_wabas, owned] {
                assert!(
                    matches!(
                        &refused[..],
                        [Err(meta_whatsapp_core::Error::Validation(_))]
                    ),
                    "{refused:?}"
                );
            }
        }
        assert_eq!(t.requests().len(), 2);
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

    /// `business-scoped-user-ids`, § Get parent BSUID account: the response
    /// syntax as printed (its placeholders are strings).
    fn parent_bsuid_account_example() -> serde_json::Value {
        json!({
          "parent_bsuid_account_id": "<PARENT_BSUID_ACCOUNT_ID>",
          "enrolled_business_portfolios": [
            "<BUSINESS_PORTFOLIO_ID>",
            "<BUSINESS_PORTFOLIO_ID>"
          ]
        })
    }

    /// The request goes to `https://api.facebook.com/{id}/parent-bsuid-accounts`
    /// (no version, no query, no body) with the client's token, and the
    /// page's example parses.
    #[tokio::test]
    async fn parent_bsuid_account_reaches_api_facebook_com_with_the_token() {
        let t = ScriptedTransport::new();
        t.push_json(200, parent_bsuid_account_example());
        let account = client(&t)
            .business("805021500648488")
            .parent_bsuid_account()
            .await
            .unwrap();
        assert_eq!(account.parent_bsuid_account_id, "<PARENT_BSUID_ACCOUNT_ID>");
        assert_eq!(
            account.enrolled_business_portfolios,
            [
                BusinessId::new("<BUSINESS_PORTFOLIO_ID>"),
                BusinessId::new("<BUSINESS_PORTFOLIO_ID>")
            ]
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(
            req.url.as_str(),
            "https://api.facebook.com/805021500648488/parent-bsuid-accounts"
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(req.url.query(), None);
        assert_eq!(req.body, meta_whatsapp_core::testing::RecordedBody::Empty);
        assert_eq!(t.remaining(), 0);

        // A merchant's client sends the merchant's token there.
        t.push_json(
            200,
            json!({
                "parent_bsuid_account_id": "1122334455",
                "enrolled_business_portfolios": ["805021500648488", "109876543210"],
                "a_field_meta_adds_later": true
            }),
        );
        let account = client(&t)
            .with_token("MERCHANT".into())
            .business(BusinessId::new("109876543210"))
            .parent_bsuid_account()
            .await
            .unwrap();
        assert_eq!(account.parent_bsuid_account_id, "1122334455");
        assert_eq!(
            account.enrolled_business_portfolios[1].as_str(),
            "109876543210"
        );
        let req = t.last_request().unwrap();
        assert_eq!(
            req.url.as_str(),
            "https://api.facebook.com/109876543210/parent-bsuid-accounts"
        );
        assert_eq!(req.bearer(), Some("MERCHANT"));
        assert_eq!(t.remaining(), 0);
    }

    /// Behind a Graph proxy the call still goes to `api.facebook.com`: the
    /// proxy stands in for Graph, and Meta serves this API elsewhere.
    #[tokio::test]
    async fn parent_bsuid_account_ignores_a_graph_proxy() {
        let t = ScriptedTransport::new();
        t.push_json(200, parent_bsuid_account_example());
        let proxied = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .endpoint(
                meta_whatsapp_core::config::GraphEndpoint::custom(
                    "https://graph-proxy.internal/",
                    meta_whatsapp_core::config::ApiVersion::DEFAULT,
                )
                .unwrap(),
            )
            .build()
            .unwrap();
        proxied
            .business("805021500648488")
            .parent_bsuid_account()
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(
            req.url.as_str(),
            "https://api.facebook.com/805021500648488/parent-bsuid-accounts"
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    /// An id that is not digits is refused before any request, naming the
    /// business id (the credential rule would refuse it too, on `url`).
    #[tokio::test]
    async fn parent_bsuid_account_refuses_an_id_that_is_not_digits() {
        let t = ScriptedTransport::new();
        let c = client(&t);
        for id in [
            "",
            "abc",
            "12a4",
            "123/x",
            "123/parent-bsuid-accounts",
            "..",
            ".",
            "v25.0",
            "1 2",
            " 123",
            "+123",
            "-1",
            "1.5",
            "123?x=1",
            "123#x",
            "%31",
            "\u{661}\u{662}\u{663}", // Arabic-Indic digits
            "\u{ff11}\u{ff12}",      // fullwidth digits
        ] {
            let err = c.business(id).parent_bsuid_account().await.unwrap_err();
            assert!(
                matches!(&err, meta_whatsapp_core::Error::Validation(v) if v.field == "business_id"),
                "{id:?}: {err}"
            );
        }
        assert!(t.requests().is_empty());
    }

    /// A Graph error keeps its kind; a transient one is replayed, as for
    /// every `GET`; so is a timeout.
    #[tokio::test]
    async fn parent_bsuid_account_errors_and_retries() {
        let t = ScriptedTransport::new();
        t.push_json(
            400,
            json!({"error": {"message": "Invalid OAuth access token.", "type": "OAuthException", "code": 190}}),
        );
        let err = client(&t)
            .business("805021500648488")
            .parent_bsuid_account()
            .await
            .unwrap_err();
        assert_eq!(err.kind(), meta_whatsapp_core::ErrorKind::Authentication);
        assert_eq!(t.requests().len(), 1);

        let t = ScriptedTransport::new();
        t.push_json(
            500,
            json!({"error": {"message": "x", "code": 2, "is_transient": true}}),
        );
        t.push_error(|| meta_whatsapp_core::error::TransportError::Timeout);
        t.push_json(200, parent_bsuid_account_example());
        let retrying = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy {
                max_retries: 2,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            })
            .build()
            .unwrap();
        let account = retrying
            .business("805021500648488")
            .parent_bsuid_account()
            .await
            .unwrap();
        assert_eq!(account.parent_bsuid_account_id, "<PARENT_BSUID_ACCOUNT_ID>");
        let reqs = t.requests();
        assert_eq!(reqs.len(), 3);
        for req in &reqs {
            assert_eq!(req.method, Method::GET);
            assert_eq!(
                req.url.as_str(),
                "https://api.facebook.com/805021500648488/parent-bsuid-accounts"
            );
            assert_eq!(req.bearer(), Some("TOKEN"));
        }
        assert_eq!(t.remaining(), 0);
    }

    /// The page documents both fields; a missing list is empty, a missing
    /// account id a decode error (what Meta answers for a portfolio that is
    /// not enrolled is not documented).
    #[test]
    fn parent_bsuid_account_parsing() {
        let parsed: ParentBsuidAccount =
            serde_json::from_value(json!({"parent_bsuid_account_id": "1"})).unwrap();
        assert!(parsed.enrolled_business_portfolios.is_empty());
        assert!(
            serde_json::from_value::<ParentBsuidAccount>(
                json!({"enrolled_business_portfolios": ["1"]})
            )
            .is_err()
        );
        let parsed: ParentBsuidAccount =
            serde_json::from_value(parent_bsuid_account_example()).unwrap();
        assert_eq!(
            serde_json::to_value(&parsed).unwrap(),
            parent_bsuid_account_example()
        );
    }
}
