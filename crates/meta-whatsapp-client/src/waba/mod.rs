//! WhatsApp Business Account: details and update, phone numbers (list,
//! create), webhook subscriptions (`subscribed_apps`, WABA-level callback
//! override), assigned users; plus the business portfolio accessor
//! ([`Client::business`]) for client/owned WABA lists and messaging customer
//! bases.
//!
//! Docs: `whatsapp-business-accounts`,
//! `reference/whatsapp-business-account/{whatsapp-business-account-api,
//! phone-number-management-api, subscribed-apps-api,
//! assigned-users-management-api}`, `reference/business/{business-account-api,
//! client-whatsapp-business-accounts-api, owned-whatsapp-business-accounts}`,
//! `solution-providers/{manage-accounts, manage-webhooks, manage-phone-numbers,
//! manage-system-users, registering-phone-numbers}`, `webhooks/override`,
//! `in-app-signup` (messaging customer bases).
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! Every path is built from segments ([`Client::get_at`] and friends), so a
//! WABA or business id containing `/` or `..` cannot address another object
//! with the token.
//!
//! # Subscriptions and the callback override
//!
//! `subscribe_app` is idempotent in the sense that matters for onboarding:
//! subscribing an already-subscribed app succeeds and changes nothing —
//! **except** for the WABA-level override. Meta documents that a subscribe
//! call *without* a body removes an existing override. So
//! `subscribe_app(None)` means "subscribed, no override", not "subscribed,
//! leave the override alone"; repeat it with the same argument you used
//! the first time.
//!
//! # Where Meta's pages disagree
//!
//! - Assigned users: the reference describes `user`/`tasks` as a form body,
//!   the `manage-system-users` guide passes them in the query string. This
//!   client follows the guide's example (Graph accepts either).
//! - `GET assigned_users` needs `business`: the reference defines it as the
//!   business that owns or has access to the WABA, the guide's example passes
//!   the WABA id. It is a required argument here; pass what applies to you.
//! - Sort values differ per edge: `creation_time_ascending` for WABA lists
//!   (`manage-accounts`), `creation_time.asc` or
//!   `last_onboarded_time_ascending` for phone numbers; phone number sorting
//!   is therefore a raw string.

mod business;
mod types;

pub use business::{Business, CreatedMessagingCustomerBase, WabaListQuery};
pub use types::{
    AccountReviewStatus, AssignedUser, AssignedUserType, BusinessInfo, BusinessRef,
    BusinessVerificationStatus, CallbackOverride, Filter, MAX_CALLBACK_URI_CHARS,
    MessagingCustomerBase, NewPhoneNumber, OwnershipType, SubscribedApp, SubscribedAppData,
    WabaInfo, WabaSort, WabaTask, WabaUpdate,
};

pub(crate) use types::lenient_string;

use futures::Stream;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{BusinessId, WabaId};
use meta_whatsapp_core::paging::Page;
use serde::Serialize;

use crate::phone_numbers::{CreatedPhoneNumber, PhoneNumberInfo, fields_param};
use crate::request::{paginate_or_error, reject_cursors};
use crate::{Client, GraphRequest};

/// Entry point, see [`Client::waba`].
#[derive(Debug, Clone)]
pub struct Waba {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Waba`] API for `waba_id`.
    pub fn waba(&self, waba_id: impl Into<WabaId>) -> Waba {
        Waba {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

/// Options for listing a WABA's phone numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PhoneNumbersQuery {
    /// Fields to return (empty = Meta's defaults).
    pub fields: Vec<String>,
    /// `filtering` conditions (e.g. [`Filter::account_mode`]).
    pub filtering: Vec<Filter>,
    /// Raw `sort` value, see the module docs.
    pub sort: Option<String>,
    /// Page size, 1–100.
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`
    /// ([`Page::next_cursor`]); for [`Waba::phone_numbers`] only, the
    /// stream manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl PhoneNumbersQuery {
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
    pub fn sort(mut self, sort: impl Into<String>) -> Self {
        self.sort = Some(sort.into());
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

/// Options for listing a WABA's assigned users
/// (`reference/whatsapp-business-account/assigned-users-management-api`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListAssignedUsers {
    /// The business portfolio the assignments are read for (`business`,
    /// required).
    pub business: BusinessId,
    /// Page size, 1–100 (Meta's default: 25).
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`; for
    /// [`Waba::assigned_users`] only, the stream manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl ListAssignedUsers {
    /// The users assigned for `business`, Meta's page size.
    pub fn new(business: impl Into<BusinessId>) -> Self {
        Self {
            business: business.into(),
            limit: None,
            after: None,
            before: None,
        }
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

#[derive(Serialize)]
struct SubscribeBody<'a> {
    override_callback_uri: &'a str,
    verify_token: &'a str,
}

impl Waba {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `GET /{WABA_ID}` with `fields` (empty = Meta's defaults, id and
    /// name). E.g. `account_review_status`, `business_verification_status`,
    /// `currency`, `timezone_id`, `message_template_namespace`.
    pub async fn get(&self, fields: &[&str]) -> Result<WabaInfo> {
        self.client
            .get_at(&[self.waba_id.as_str()])
            .query_opt("fields", fields_param(fields))
            .context("WhatsApp Business Account")
            .send()
            .await
    }

    /// `GET /{WABA_ID}?fields=owner_business_info`, decoded without quoting
    /// the body in a decode error (it carries the business's name).
    pub(crate) async fn owner_business(&self) -> Result<Option<BusinessRef>> {
        let info: WabaInfo = self
            .client
            .get_at(&[self.waba_id.as_str()])
            .query("fields", "owner_business_info")
            .context("WhatsApp Business Account owner")
            .send_private()
            .await?;
        Ok(info.owner_business_info)
    }

    /// `POST /{WABA_ID}`: rename or change the time zone.
    pub async fn update(&self, update: &WabaUpdate) -> Result<()> {
        if update.name.is_none() && update.timezone_id.is_none() {
            return Err(ValidationError::new("update", "set `name` or `timezone_id`").into());
        }
        self.client
            .post_at(&[self.waba_id.as_str()])
            .json(update)
            .idempotent(true)
            .context("WABA update response")
            .send_success()
            .await
    }

    fn phone_numbers_request(&self, query: &PhoneNumbersQuery) -> Result<GraphRequest> {
        if let Some(limit) = query.limit
            && !(1..=100).contains(&limit)
        {
            return Err(ValidationError::new("limit", "must be 1-100").into());
        }
        let mut req = self
            .client
            .get_at(&[self.waba_id.as_str(), "phone_numbers"])
            .query_opt(
                "fields",
                (!query.fields.is_empty()).then(|| query.fields.join(",")),
            )
            .query_opt("sort", query.sort.as_deref())
            .query_opt("limit", query.limit)
            .context("WABA phone numbers");
        if !query.filtering.is_empty() {
            req = req.query_json("filtering", &query.filtering);
        }
        Ok(req)
    }

    /// `GET /{WABA_ID}/phone_numbers`, one page. Meta sorts by Embedded
    /// Signup completion, most recent first, unless `sort` says otherwise.
    /// The next page: the same query with `after` set to this page's
    /// [`Page::next_cursor`].
    pub async fn phone_numbers(&self, query: &PhoneNumbersQuery) -> Result<Page<PhoneNumberInfo>> {
        self.phone_numbers_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// All phone numbers, following cursors. The stream manages them
    /// itself: a query with `after` or `before` set is refused (the
    /// stream's single item is that validation error).
    pub fn phone_numbers_stream(
        &self,
        query: &PhoneNumbersQuery,
    ) -> impl Stream<Item = Result<PhoneNumberInfo>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.phone_numbers_request(query)),
        )
    }

    /// `POST /{WABA_ID}/phone_numbers`: add a number to the WABA (only
    /// needed when Embedded Signup did not, e.g. the bypass flow). Then
    /// `request_code` → `verify_code` → `register` on the returned id.
    pub async fn create_phone_number(&self, number: &NewPhoneNumber) -> Result<CreatedPhoneNumber> {
        number.validate()?;
        self.client
            .post_at(&[self.waba_id.as_str(), "phone_numbers"])
            .json(number)
            .context("create phone number response")
            .send()
            .await
    }

    /// `GET /{WABA_ID}/subscribed_apps`.
    ///
    /// Unlike the other lists, it takes no cursor: the reference documents
    /// neither `after`/`before` nor a `paging` object (a WABA has a handful
    /// of subscribed apps). Should Meta page it anyway,
    /// [`Self::subscribed_apps_stream`] follows the cursors.
    pub async fn subscribed_apps(&self) -> Result<Page<SubscribedApp>> {
        self.subscribed_apps_request().send().await
    }

    /// All subscribed apps, following cursors.
    pub fn subscribed_apps_stream(
        &self,
    ) -> impl Stream<Item = Result<SubscribedApp>> + Send + 'static + use<> {
        self.subscribed_apps_request().paginate()
    }

    fn subscribed_apps_request(&self) -> GraphRequest {
        self.client
            .get_at(&[self.waba_id.as_str(), "subscribed_apps"])
            .context("subscribed apps")
    }

    /// `POST /{WABA_ID}/subscribed_apps`: subscribe the calling app to this
    /// WABA's webhooks, optionally sending the WABA's supported webhooks to
    /// an alternate callback.
    ///
    /// Safe to repeat with the same argument (see the module docs: `None`
    /// removes an existing override). Replayed on transient errors.
    pub async fn subscribe_app(&self, callback_override: Option<&CallbackOverride>) -> Result<()> {
        let mut req = self
            .client
            .post_at(&[self.waba_id.as_str(), "subscribed_apps"])
            .idempotent(true)
            .context("subscribe app response");
        if let Some(o) = callback_override {
            o.validate()?;
            req = req.json(&SubscribeBody {
                override_callback_uri: &o.override_callback_uri,
                verify_token: o.verify_token.expose_secret(),
            });
        }
        req.send_success().await
    }

    /// `DELETE /{WABA_ID}/subscribed_apps`: stop this WABA's webhooks for the
    /// calling app, immediately.
    pub async fn unsubscribe_app(&self) -> Result<()> {
        self.client
            .delete_at(&[self.waba_id.as_str(), "subscribed_apps"])
            .context("unsubscribe app response")
            .send_success()
            .await
    }

    fn assigned_users_request(&self, query: &ListAssignedUsers) -> Result<GraphRequest> {
        if let Some(limit) = query.limit
            && !(1..=100).contains(&limit)
        {
            return Err(ValidationError::new("limit", "must be 1-100").into());
        }
        Ok(self
            .client
            .get_at(&[self.waba_id.as_str(), "assigned_users"])
            .query("business", &query.business)
            .query_opt("limit", query.limit)
            .context("assigned users"))
    }

    /// `GET /{WABA_ID}/assigned_users?business=…`, one page. The next page:
    /// the same query with `after` set to this page's
    /// [`Page::next_cursor`].
    pub async fn assigned_users(&self, query: &ListAssignedUsers) -> Result<Page<AssignedUser>> {
        self.assigned_users_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// All assigned users, following cursors. The stream manages them
    /// itself: a query with `after` or `before` set is refused (the
    /// stream's single item is that validation error).
    pub fn assigned_users_stream(
        &self,
        query: &ListAssignedUsers,
    ) -> impl Stream<Item = Result<AssignedUser>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.assigned_users_request(query)),
        )
    }

    /// `POST /{WABA_ID}/assigned_users?user=…&tasks=[…]`: grant a user (e.g.
    /// your system user, for credit line sharing) tasks on the WABA. Needs an
    /// admin system user token.
    pub async fn assign_user(&self, user_id: &str, tasks: &[WabaTask]) -> Result<()> {
        if user_id.trim().is_empty() {
            return Err(ValidationError::new("user", "required").into());
        }
        if tasks.is_empty() {
            return Err(ValidationError::new("tasks", "at least one task is required").into());
        }
        self.client
            .post_at(&[self.waba_id.as_str(), "assigned_users"])
            .query("user", user_id)
            .query_json("tasks", &tasks)
            // Granting the same tasks again leaves the same state.
            .idempotent(true)
            .context("assign user response")
            .send_success()
            .await
    }

    /// `DELETE /{WABA_ID}/assigned_users?user=…`: revoke all of a user's
    /// access to the WABA.
    pub async fn remove_user(&self, user_id: &str) -> Result<()> {
        if user_id.trim().is_empty() {
            return Err(ValidationError::new("user", "required").into());
        }
        self.client
            .delete_at(&[self.waba_id.as_str(), "assigned_users"])
            .query("user", user_id)
            .context("remove user response")
            .send_success()
            .await
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use meta_whatsapp_core::ErrorKind;
    use meta_whatsapp_core::testing::{RecordedBody, ScriptedTransport};
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

    #[tokio::test]
    async fn get_with_fields_parses_review_status_example() {
        // solution-providers/manage-accounts, "Retrieve WABA review status".
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"account_review_status": "APPROVED", "id": "1111111111111"}),
        );
        let w = client(&t)
            .waba("106526625562206")
            .get(&["account_review_status"])
            .await
            .unwrap();
        assert_eq!(w.account_review_status, Some(AccountReviewStatus::Approved));
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/106526625562206");
        assert_eq!(
            req.query("fields").as_deref(),
            Some("account_review_status")
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_requires_a_field_and_posts_json() {
        let t = ScriptedTransport::new();
        let w = client(&t).waba("1");
        assert!(w.update(&WabaUpdate::new()).await.is_err());
        assert!(t.requests().is_empty());
        t.push_json(200, json!({"success": true}));
        w.update(&WabaUpdate::new().name("Lucky Shrub"))
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/1");
        assert_eq!(req.json(), Some(json!({"name": "Lucky Shrub"})));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn phone_numbers_page_with_filter_and_docs_example() {
        // solution-providers/manage-phone-numbers, "Filter phone numbers by account mode".
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
              "data": [{
                "id": "1972385232742141",
                "display_phone_number": "+1 631-555-1111",
                "verified_name": "John's Cake Shop",
                "quality_rating": "UNKNOWN"
              }],
              "paging": {"cursors": {"before": "abcdefghij", "after": "klmnopqr"}}
            }),
        );
        let page = client(&t)
            .waba("102290129340398")
            .phone_numbers(
                &PhoneNumbersQuery::new()
                    .filter(Filter::account_mode("SANDBOX"))
                    .fields(["id", "display_phone_number"])
                    .limit(10),
            )
            .await
            .unwrap();
        assert_eq!(
            page.data[0].verified_name.as_deref(),
            Some("John's Cake Shop")
        );
        assert_eq!(page.next_cursor(), None);
        let req = t.last_request().unwrap();
        assert_eq!(req.path(), "/v25.0/102290129340398/phone_numbers");
        assert_eq!(
            req.query("filtering").as_deref(),
            Some(r#"[{"field":"account_mode","operator":"EQUAL","value":"SANDBOX"}]"#)
        );
        assert_eq!(
            req.query("fields").as_deref(),
            Some("id,display_phone_number")
        );
        assert_eq!(req.query("limit").as_deref(), Some("10"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn phone_numbers_stream_follows_cursors_and_validates_limit() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "1"}], "paging": {"cursors": {"after": "c1"}, "next": "https://graph.facebook.com/next"}}),
        );
        t.push_json(200, json!({"data": [{"id": "2"}]}));
        let ids: Vec<String> = client(&t)
            .waba("W")
            .phone_numbers_stream(&PhoneNumbersQuery::new())
            .map(|r| r.unwrap().id.into_inner())
            .collect()
            .await;
        assert_eq!(ids, vec!["1", "2"]);
        assert_eq!(t.requests()[1].query("after").as_deref(), Some("c1"));
        assert_eq!(t.remaining(), 0);

        let errs: Vec<_> = client(&t)
            .waba("W")
            .phone_numbers_stream(&PhoneNumbersQuery::new().limit(101))
            .collect()
            .await;
        assert_eq!(errs.len(), 1);
        assert!(matches!(
            errs[0],
            Err(meta_whatsapp_core::Error::Validation(_))
        ));
        assert_eq!(t.requests().len(), 2);
    }

    /// Every list takes its cursor in its query: the one-page method sends
    /// it, the stream refuses it (it manages the cursors) before any request.
    #[tokio::test]
    async fn phone_numbers_and_assigned_users_take_cursors_in_their_query() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"data": []}));
        t.push_json(200, json!({"data": []}));
        t.push_json(200, json!({"data": []}));
        let w = client(&t).waba("W");
        w.phone_numbers(&PhoneNumbersQuery::new().after("QVFIU"))
            .await
            .unwrap();
        w.phone_numbers(&PhoneNumbersQuery::new().before("QVFIB"))
            .await
            .unwrap();
        w.assigned_users(&ListAssignedUsers::new("B").limit(100).after("MjQZD"))
            .await
            .unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].query("after").as_deref(), Some("QVFIU"));
        assert_eq!(reqs[0].query("before"), None);
        assert_eq!(reqs[1].query("before").as_deref(), Some("QVFIB"));
        assert_eq!(reqs[2].path(), "/v25.0/W/assigned_users");
        assert_eq!(reqs[2].query("business").as_deref(), Some("B"));
        assert_eq!(reqs[2].query("limit").as_deref(), Some("100"));
        assert_eq!(reqs[2].query("after").as_deref(), Some("MjQZD"));
        assert_eq!(t.remaining(), 0);

        let refused: Vec<_> = w
            .phone_numbers_stream(&PhoneNumbersQuery::new().after("x"))
            .collect()
            .await;
        assert!(
            matches!(&refused[..], [Err(meta_whatsapp_core::Error::Validation(v))] if v.field == "after"),
            "{refused:?}"
        );
        let refused: Vec<_> = w
            .assigned_users_stream(&ListAssignedUsers::new("B").before("x"))
            .collect()
            .await;
        assert!(
            matches!(&refused[..], [Err(meta_whatsapp_core::Error::Validation(v))] if v.field == "before"),
            "{refused:?}"
        );
        for limit in [0, 101] {
            let err = w
                .assigned_users(&ListAssignedUsers::new("B").limit(limit))
                .await
                .unwrap_err();
            assert!(
                matches!(&err, meta_whatsapp_core::Error::Validation(v) if v.field == "limit"),
                "{limit}: {err}"
            );
        }
        assert_eq!(t.requests().len(), 3, "refused before any request");
    }

    #[tokio::test]
    async fn assigned_users_stream_follows_cursors() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "1", "name": "Anna Flex", "tasks": ["MANAGE"]}],
                "paging": {"cursors": {"before": "MAZDZD", "after": "MjQZD"},
                    "next": "https://graph.facebook.com/v25.0/W/assigned_users?after=MjQZD"}}),
        );
        t.push_json(
            200,
            json!({"data": [{"id": "2", "name": "Jasper Brown", "tasks": ["DEVELOP"]}]}),
        );
        let users: Vec<String> = client(&t)
            .waba("W")
            .assigned_users_stream(&ListAssignedUsers::new("B"))
            .map(|u| u.unwrap().id)
            .collect()
            .await;
        assert_eq!(users, ["1", "2"]);
        let second = &t.requests()[1];
        assert_eq!(second.query("after").as_deref(), Some("MjQZD"));
        assert_eq!(second.query("business").as_deref(), Some("B"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn create_phone_number_posts_guide_example() {
        // solution-providers/registering-phone-numbers, step 1.
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"id": "110200345501442"}));
        let created = client(&t)
            .waba("102290129340398")
            .create_phone_number(&NewPhoneNumber::new("1", "14195551518", "Lucky Shrub"))
            .await
            .unwrap();
        assert_eq!(created.id.as_str(), "110200345501442");
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/102290129340398/phone_numbers");
        assert_eq!(
            req.json(),
            Some(json!({"cc": "1", "phone_number": "14195551518", "verified_name": "Lucky Shrub"}))
        );
        assert_eq!(t.remaining(), 0);
        assert!(
            client(&t)
                .waba("1")
                .create_phone_number(&NewPhoneNumber::new("1", " ", "x"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn subscribe_without_and_with_override() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        t.push_json(200, json!({"success": true}));
        let w = client(&t).waba("102290129340398");
        w.subscribe_app(None).await.unwrap();
        // webhooks/override, "Set WABA alternate callback".
        w.subscribe_app(Some(&CallbackOverride::new(
            "https://my-waba-alternate-callback.com/webhook",
            "myvoiceismypassport?",
        )))
        .await
        .unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::POST);
        assert_eq!(reqs[0].path(), "/v25.0/102290129340398/subscribed_apps");
        assert_eq!(reqs[0].body, RecordedBody::Empty, "no body: no override");
        assert_eq!(
            reqs[1].json(),
            Some(json!({
              "override_callback_uri": "https://my-waba-alternate-callback.com/webhook",
              "verify_token": "myvoiceismypassport?"
            }))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn subscribe_is_replayed_on_transient_errors() {
        let t = ScriptedTransport::new();
        t.push_json(
            500,
            json!({"error": {"message": "x", "code": 2, "is_transient": true}}),
        );
        t.push_json(200, json!({"success": true}));
        let c = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy {
                max_retries: 1,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            })
            .build()
            .unwrap();
        c.waba("1").subscribe_app(None).await.unwrap();
        assert_eq!(t.requests().len(), 2);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn subscribed_apps_parse_override_example() {
        // webhooks/override, "Get WABA alternate callback".
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
              "data" : [{
                "whatsapp_business_api_data" : {
                  "id" : "670843887433847",
                  "link" : "https://www.facebook.com/games/?app_id=67084...",
                  "name" : "Lucky Shrub"
                },
                "override_callback_uri" : "https://my-waba-alternate-callback.com/webhook"
              }]
            }),
        );
        let page = client(&t).waba("W").subscribed_apps().await.unwrap();
        let app = &page.data[0];
        assert_eq!(app.whatsapp_business_api_data.id, "670843887433847");
        assert_eq!(
            app.override_callback_uri.as_deref(),
            Some("https://my-waba-alternate-callback.com/webhook")
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/W/subscribed_apps");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn unsubscribe_is_a_delete() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .waba("102289599326934")
            .unsubscribe_app()
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::DELETE);
        assert_eq!(req.path(), "/v25.0/102289599326934/subscribed_apps");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn subscribe_permission_error_is_classified() {
        let t = ScriptedTransport::new();
        t.push_json(
            403,
            json!({"error": {"message": "Your app doesn't have permission", "type": "OAuthException", "code": 200, "error_subcode": 1349174}}),
        );
        let err = client(&t).waba("1").subscribe_app(None).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn assigned_users_list_assign_remove() {
        // solution-providers/manage-system-users examples.
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [
              {"id": "1972385232742142", "name": "Anna Flex", "tasks": ["MANAGE"]},
              {"id": "1972385232752545", "name": "Jasper Brown", "tasks": ["DEVELOP"]}
            ]}),
        );
        t.push_json(200, json!({"success": true}));
        t.push_json(200, json!({"success": true}));
        let w = client(&t).waba("W");
        let users = w
            .assigned_users(&ListAssignedUsers::new("B"))
            .await
            .unwrap();
        assert_eq!(users.data[0].tasks, vec![WabaTask::Manage]);
        w.assign_user("1972555232742222", &[WabaTask::Manage])
            .await
            .unwrap();
        w.remove_user("1972555232742222").await.unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].path(), "/v25.0/W/assigned_users");
        assert_eq!(reqs[0].query("business").as_deref(), Some("B"));
        assert_eq!(reqs[0].query("after"), None);
        assert_eq!(reqs[1].method, Method::POST);
        assert_eq!(reqs[1].query("user").as_deref(), Some("1972555232742222"));
        assert_eq!(reqs[1].query("tasks").as_deref(), Some(r#"["MANAGE"]"#));
        assert_eq!(reqs[2].method, Method::DELETE);
        assert_eq!(reqs[2].query("user").as_deref(), Some("1972555232742222"));
        assert_eq!(t.remaining(), 0);

        assert!(w.assign_user("1", &[]).await.is_err());
        assert!(w.remove_user("").await.is_err());
        assert_eq!(t.requests().len(), 3);
    }

    /// A WABA or business id taken from data must not be able to address a
    /// different Graph object with the tenant's token.
    #[tokio::test]
    async fn ids_cannot_escape_their_path_segment() {
        let t = ScriptedTransport::new();
        for _ in 0..4 {
            t.push_json(200, json!({"success": true}));
        }
        t.push_json(200, json!({"data": []}));
        let c = client(&t);
        c.waba("OTHER/subscribed_apps")
            .subscribe_app(None)
            .await
            .unwrap();
        c.waba("123?fields=x").unsubscribe_app().await.unwrap();
        c.waba("W/assigned_users").remove_user("1").await.unwrap();
        c.waba("W/../OTHER")
            .update(&WabaUpdate::new().name("n"))
            .await
            .unwrap();
        c.business("B/owned_whatsapp_business_accounts")
            .client_whatsapp_business_accounts(&WabaListQuery::new())
            .await
            .unwrap();
        let paths: Vec<String> = t.requests().iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(
            paths,
            vec![
                "/v25.0/OTHER%2Fsubscribed_apps/subscribed_apps",
                "/v25.0/123%3Ffields=x/subscribed_apps",
                "/v25.0/W%2Fassigned_users/assigned_users",
                "/v25.0/W%2F..%2FOTHER",
                "/v25.0/B%2Fowned_whatsapp_business_accounts/client_whatsapp_business_accounts",
            ]
        );
        for id in ["..", "."] {
            assert!(matches!(
                c.waba(id).subscribe_app(None).await,
                Err(meta_whatsapp_core::Error::Validation(_))
            ));
            assert!(c.waba(id).get(&[]).await.is_err());
            let streamed: Vec<_> = c
                .waba(id)
                .phone_numbers_stream(&PhoneNumbersQuery::new())
                .take(3)
                .collect()
                .await;
            assert_eq!(streamed.len(), 1);
            assert!(matches!(
                streamed[0],
                Err(meta_whatsapp_core::Error::Validation(_))
            ));
            assert!(c.business(id).get(&[]).await.is_err());
        }
        assert_eq!(t.requests().len(), 5, "invalid ids never reach the wire");
        assert_eq!(t.remaining(), 0);
    }
}
