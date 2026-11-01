//! WhatsApp Business Account: details and update, health status, phone
//! numbers (list, create), webhook subscriptions (`subscribed_apps`,
//! WABA-level callback override), assigned users; plus the business
//! portfolio accessor ([`Client::business`]) for client/owned WABA lists,
//! messaging customer bases and the parent BSUID account
//! ([`Business::parent_bsuid_account`], served from `api.facebook.com`,
//! not Graph). The catalog connected to a WABA is read by
//! [`Waba::product_catalogs`], defined with the other catalog reads in
//! [`crate::commerce`].
//!
//! Administration (roadmap L10b):
//!
//! - creating a WABA under a portfolio
//!   ([`Business::create_whatsapp_business_account`]);
//! - a client business's system user token, with the `appsecret_proof` of
//!   yours ([`Business::system_user_access_token`]); a portfolio's system
//!   users ([`Business::system_users`]); the WABAs assigned to a user
//!   ([`BusinessUser::assigned_whatsapp_business_accounts`]);
//! - a WABA's activity history ([`Waba::activities`]) and its Multi-Partner
//!   Solutions ([`Waba::solutions`]);
//! - the billing currency or payment method migration
//!   ([`Waba::set_payment_method_migration_intent`], then
//!   [`PaymentMethodMigration`]), two explicit calls nothing else makes;
//! - the delivery events of a message history entry
//!   ([`MessageHistory::events`]);
//! - Meta's campaign schedules ([`Waba::schedules`],
//!   [`Waba::create_schedule`]).
//!
//! Pre-verified phone numbers (roadmap L11c): a partner portfolio's pool
//! ([`Business::add_pre_verified_number`],
//! [`Business::pre_verified_numbers`],
//! [`Business::share_pre_verified_number`]) and each number's code,
//! partners and deletion ([`Client::pre_verified_phone_number`],
//! [`PreVerifiedPhoneNumber`]); see [below](#pre-verified-phone-numbers).
//!
//! Every creation here (a WABA, a migration intent, a schedule, a resumed
//! migration, a generated token) is a `POST` that is never replayed after a
//! timeout ([`crate::RetryPolicy`]); fetching a client's existing token
//! (`fetch_only`, read-only by Meta's page) is replayed as a read is.
//!
//! Docs: `whatsapp-business-accounts`, `support/health-status`,
//! `reference/whatsapp-business-account/{whatsapp-business-account-api,
//! phone-number-management-api, subscribed-apps-api,
//! assigned-users-management-api}`, `reference/business/{business-account-api,
//! client-whatsapp-business-accounts-api, owned-whatsapp-business-accounts}`,
//! `solution-providers/{manage-accounts, manage-webhooks, manage-phone-numbers,
//! manage-system-users, registering-phone-numbers}`, `webhooks/override`,
//! `in-app-signup` (messaging customer bases), `business-scoped-user-ids`
//! (§ Get parent BSUID account); for L10b
//! `reference/business/whatsapp-business-accounts-api` (`POST`),
//! `solution-providers/partner-initiated-waba-creation`,
//! `reference/whatsapp-business-account/{whatsapp-business-account-activities-api,
//! whatsapp-business-account-solutions-list-api, schedules-api}`,
//! `reference/user/assigned-whatsapp-business-accounts-api`,
//! `pricing/change-billing-currency`,
//! `reference/message-history/whatsapp-business-message-history-events-api`,
//! `embedded-signup/hosted-es` (step 5); for L11c
//! `embedded-signup/pre-verified-numbers`,
//! `reference/business/{add-phone-numbers-api,
//! whatsapp-business-pre-verified-phone-numbers-api,
//! whatsapp-business-pre-verified-phone-number-sharing-api}` and
//! `reference/whatsapp-business-pre-verified-phone-number/*`; and, outside that tree,
//! `facebook-login/facebook-login-for-business` (§ Get tokens) and the
//! Marketing API's `ads-commerce/marketing-api/reference/business/system_user_access_tokens`.
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
//! # Partner-initiated WABA creation
//!
//! A Solution Partner requests a WABA for a client in Meta Business Suite
//! (`solution-providers/partner-initiated-waba-creation`): the page
//! documents no API call for the request itself. What follows the
//! client's acceptance is the API's: a `PARTNER_ADDED` `account_update`
//! webhook, then a number ([`Waba::create_phone_number`]) and the credit
//! line ([`crate::credit_lines`]), with the partner's system user token.
//! [`Business::create_whatsapp_business_account`] creates a WABA under a
//! portfolio you manage.
//!
//! # The billing migration: two explicit calls
//!
//! [`Waba::set_payment_method_migration_intent`] clones the WABA into a new
//! one in the target currency and migrates its templates (and the Flows
//! they use); [`PaymentMethodMigration::resume_migration`] makes the clone
//! the billing account of the numbers, after which the old WABA can no
//! longer send (`pricing/change-billing-currency`). The currency, with the
//! legal entity's headquarters address, decides which Meta billing entity
//! invoices the business. No other call of the library makes either
//! request: deciding to is the integrator's. A credit card cannot be
//! attached through the API; for a card, add it to the clone in Business
//! Manager between the two calls (a Line of Credit is attached by passing
//! its id to the first).
//!
//! # Message history ids and campaign schedules
//!
//! The message history reference says its id comes from "message history
//! APIs", and no page of Meta's WhatsApp docs documents one: where a
//! [`MessageHistory`] id comes from is the caller's to know. Likewise the
//! schedules reference is all Meta documents about them: no guide says
//! where an `audience_id` comes from, nor what `waba_cs_id` holds beyond
//! "WABA customer status ID (phone number)" ([`NewSchedule`]). A schedule
//! there is Meta's, kept by Meta.
//!
//! # Pre-verified phone numbers
//!
//! A pool of numbers a Solution Partner or Tech Provider adds to its own
//! business portfolio and verifies ahead of time, so that Embedded Signup
//! can offer them to a business customer without a one-time password
//! (`embedded-signup/pre-verified-numbers`).
//!
//! | Step | Call | Meta path |
//! | --- | --- | --- |
//! | add a number to the pool | [`Business::add_pre_verified_number`] | `POST /{Business-ID}/add_phone_numbers` |
//! | list the pool | [`Business::pre_verified_numbers`] (and `_stream`) | `GET /{Business-ID}/preverified_numbers` |
//! | share one with a Multi-Partner Solution partner | [`Business::share_pre_verified_number`] | `POST /{Business-ID}/share_preverified_numbers` |
//! | read one | [`PreVerifiedPhoneNumber::get`] | `GET /{Pre-Verified-Phone-Number-ID}` |
//! | have a code sent | [`PreVerifiedPhoneNumber::request_code`] | `POST …/request_code` |
//! | verify it | [`PreVerifiedPhoneNumber::verify_code`] | `POST …/verify_code` |
//! | the partners it is shared with | [`PreVerifiedPhoneNumber::partners`] (and `_stream`) | `GET …/partners` |
//! | delete it (irreversible) | [`PreVerifiedPhoneNumber::delete`] | `DELETE /{Pre-Verified-Phone-Number-ID}` |
//!
//! Then offer the verified ids in Embedded Signup
//! ([`crate::embedded_signup::LaunchOptions::pre_verified_phone_ids`]), or,
//! with the phone number screen bypassed, add one to an onboarded WABA
//! ([`NewPhoneNumber::preverified_id`]). Once a business claims a
//! number, Meta replaces this temporary object with a business phone
//! number of **another** id ([`meta_whatsapp_core::ids::PhoneNumberId`]),
//! found in the signup's session event or in [`Waba::phone_numbers`] by
//! its `display_phone_number`; register that one within 90 days of
//! **your** verification, not of the claim.
//!
//! Meta's time limits, which nothing here enforces: a number added but not
//! verified within 30 days leaves the pool; a verified number nobody claims
//! within 90 days becomes `EXPIRED` and leaves it too; an unclaimed number
//! can be re-verified only from 45 days before its
//! `verification_expiry_time`. Keeping track of who claimed which number
//! is yours.
//!
//! Tokens: a user or system user token of the business that owns the pool,
//! with `business_management` (the guide's requirements). Take it from the
//! [`Client`] you build.
//!
//! ## Ids and codes
//!
//! A pre-verified number's id is a
//! [`PreVerifiedPhoneNumberId`](meta_whatsapp_core::ids::PreVerifiedPhoneNumberId),
//! numeric (Meta's own error examples: "must be a valid numeric string"):
//! its checked constructor refuses anything else, and every call here
//! checks it again as digits before any request, so an id built unchecked
//! that holds `/` or `..` never reaches a path or a body. The verification
//! code is a [`VerificationCode`](crate::phone_numbers::VerificationCode),
//! the same secret type a business phone number's `verify_code` takes:
//! never in `Debug`, an error, or a URL.
//!
//! ## Where Meta's pages disagree (decided here)
//!
//! - Request bodies: the guide's examples pass `phone_number`,
//!   `code_method`, `language`, `code`, `partner_business_id` and
//!   `preverified_id` as query parameters; the references define a JSON
//!   request body for each, with no example. This client sends the JSON
//!   bodies, as [`crate::phone_numbers::PhoneNumber::verify_code`] does:
//!   it keeps the code (and the phone number) out of URLs, which end up in
//!   proxy and transport logs.
//! - `code_method`: the reference's enum also lists `IVR`, which the guide
//!   never shows; [`CodeMethod`](crate::phone_numbers::CodeMethod) offers
//!   `SMS` and `VOICE`, both documented by both pages.
//! - Unsharing (`DELETE …/share_preverified_numbers`) is documented as
//!   removed ("it returns an error"): not offered.
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
//! - System users: `manage-system-users` prints
//!   `GET /{WABA_ID}/system_users`, while the line after it tells you to
//!   look up a *business* id (each of that guide's other requests is
//!   followed by how to find the id in its own path). The Marketing API's
//!   `reference/business/system_users` documents the edge on the Business
//!   node, `GET /{business-id}/system_users`, and the WABA reference lists
//!   no such edge; system users belong to a portfolio, which creates them
//!   (`POST /{business_id}/system_users`). This client reads them from the
//!   business ([`Business::system_users`]).
//! - The system user token: `hosted-es` sends `appsecret_proof` and
//!   `fetch_only` in a form body with the token as bearer,
//!   `onboard-business-customers` in the query string with the token. This
//!   client sends the form body, which keeps the proof out of URLs.
//! - The billing migration: the page asks for the Line of Credit id "during
//!   the Complete migration step", but its `resume_migration` request has
//!   no body, and its initiate request takes `extended_credit_id`. This
//!   client sends it with the intent, as the requests show.

mod accounts;
mod activity;
mod billing;
mod business;
mod history;
mod pre_verified;
mod schedules;
mod system_users;
mod types;

pub use accounts::{CreatedWaba, NewWaba, WabaBusinessType};
pub use activity::{
    ActivityCategory, ActivityEventType, ApplicationRef, ListWabaRecords, WabaActivity,
    WabaSolution,
};
pub use billing::{
    MigrationStarted, MigrationState, PaymentMethodMigration, PaymentMethodMigrationIntent,
    PaymentMigrationStatus,
};
pub use business::{Business, CreatedMessagingCustomerBase, ParentBsuidAccount, WabaListQuery};
pub use history::{
    DeliveryStatus, DeliveryStatusOccurrence, ListMessageHistoryEvents, MessageHistory,
    MessageHistoryEvent, WebhookUpdateState,
};
pub use pre_verified::{
    CreatedPreVerifiedNumber, ListPreVerifiedNumberPartners, ListPreVerifiedNumbers,
    PreVerifiedNumber, PreVerifiedPhoneNumber,
};
pub use schedules::{CampaignSchedule, CreatedSchedule, NewSchedule, ScheduleStatus};
pub use system_users::{
    BusinessUser, ListAssignedWabas, SystemUser, SystemUserAccessToken, SystemUserRole,
    SystemUserTokenRequest,
};
pub use types::{
    AccountReviewStatus, AssignedUser, AssignedUserType, BusinessInfo, BusinessRef,
    BusinessVerificationStatus, CallbackOverride, Filter, MAX_CALLBACK_URI_CHARS,
    MessagingCustomerBase, NewPhoneNumber, OwnershipType, SubscribedApp, SubscribedAppData,
    WabaInfo, WabaSort, WabaTask, WabaUpdate,
};

pub(crate) use types::lenient_string;

/// Messaging health of a WABA: the same types phone numbers and templates
/// use (see [`crate::common`]).
pub use crate::common::{HealthEntity, HealthEntityType, HealthError, HealthState, HealthStatus};

use futures::Stream;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{BusinessId, WabaId};
use meta_whatsapp_core::paging::Page;
use serde::{Deserialize, Serialize};

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

/// Refuse a page size outside 1–100, the range every paged read of this
/// module documents.
fn check_limit(limit: Option<u32>) -> Result<()> {
    match limit {
        Some(limit) if !(1..=100).contains(&limit) => {
            Err(ValidationError::new("limit", "must be 1-100").into())
        }
        _ => Ok(()),
    }
}

/// The `fields` parameter: the fields joined with commas, `None` for Meta's
/// defaults.
fn fields_query(fields: &[String]) -> Option<String> {
    (!fields.is_empty()).then(|| fields.join(","))
}

#[derive(Serialize)]
struct SubscribeBody<'a> {
    override_callback_uri: &'a str,
    verify_token: &'a str,
}

#[derive(Deserialize)]
struct HealthEnvelope {
    health_status: HealthStatus,
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

    /// `GET /{WABA_ID}?fields=health_status`: whether messages can be sent
    /// through this WABA, with the status of the WABA, its business
    /// portfolio and the app (`support/health-status`). Meta documents no
    /// `health_status` on the business node itself: its status is the
    /// [`HealthEntityType::Business`]
    /// entry here ([`HealthStatus::entity`]).
    pub async fn health_status(&self) -> Result<HealthStatus> {
        let env: HealthEnvelope = self
            .client
            .get_at(&[self.waba_id.as_str()])
            .query("fields", "health_status")
            .context("WABA health status")
            .send()
            .await?;
        Ok(env.health_status)
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
            .waba(WabaId::new("106526625562206").unwrap())
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
    async fn health_status_reads_the_field() {
        // support/health-status, "Example response", as a WABA answers it:
        // the phone number entity is "only included if targeting a
        // business phone number".
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
              "health_status": {
                "can_send_message": "AVAILABLE",
                "entities": [
                  {"entity_type": "WABA", "id": "102290129340398", "can_send_message": "AVAILABLE"},
                  {"entity_type": "BUSINESS", "id": "506914307656634", "can_send_message": "AVAILABLE"},
                  {"entity_type": "APP", "id": "634974688087057", "can_send_message": "AVAILABLE", "can_receive_call_sip": "AVAILABLE"}
                ]
              },
              "id": "102290129340398"
            }),
        );
        let health = client(&t)
            .waba(WabaId::new("102290129340398").unwrap())
            .health_status()
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/102290129340398");
        assert_eq!(req.query("fields").as_deref(), Some("health_status"));
        assert_eq!(req.url.query_pairs().count(), 1);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(health.can_send_message, Some(HealthState::Available));
        let business = health.entity(&HealthEntityType::Business).unwrap();
        assert_eq!(business.id, "506914307656634");
        assert_eq!(t.remaining(), 0);

        // Through `get` too, as a field of the WABA.
        t.push_json(
            200,
            json!({"health_status": {"can_send_message": "BLOCKED"}, "id": "1"}),
        );
        let info = client(&t)
            .waba(WabaId::new("1").unwrap())
            .get(&["health_status"])
            .await
            .unwrap();
        assert_eq!(
            info.health_status.and_then(|h| h.can_send_message),
            Some(HealthState::Blocked)
        );
        // No `health_status` in the answer: a decode error, not a status.
        t.push_json(200, json!({"id": "1"}));
        assert!(matches!(
            client(&t)
                .waba(WabaId::new("1").unwrap())
                .health_status()
                .await,
            Err(meta_whatsapp_core::Error::Decode { .. })
        ));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_requires_a_field_and_posts_json() {
        let t = ScriptedTransport::new();
        let w = client(&t).waba(WabaId::new("1").unwrap());
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
            .waba(WabaId::new("102290129340398").unwrap())
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
            .waba(WabaId::new("102290129340398").unwrap())
            .phone_numbers_stream(&PhoneNumbersQuery::new())
            .map(|r| r.unwrap().id.into_inner())
            .collect()
            .await;
        assert_eq!(ids, vec!["1", "2"]);
        assert_eq!(t.requests()[1].query("after").as_deref(), Some("c1"));
        assert_eq!(t.remaining(), 0);

        let errs: Vec<_> = client(&t)
            .waba(WabaId::new("102290129340398").unwrap())
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
        let w = client(&t).waba(WabaId::new("102290129340398").unwrap());
        w.phone_numbers(&PhoneNumbersQuery::new().after("QVFIU"))
            .await
            .unwrap();
        w.phone_numbers(&PhoneNumbersQuery::new().before("QVFIB"))
            .await
            .unwrap();
        w.assigned_users(
            &ListAssignedUsers::new(BusinessId::new("2729063490586005").unwrap())
                .limit(100)
                .after("MjQZD"),
        )
        .await
        .unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].query("after").as_deref(), Some("QVFIU"));
        assert_eq!(reqs[0].query("before"), None);
        assert_eq!(reqs[1].query("before").as_deref(), Some("QVFIB"));
        assert_eq!(reqs[2].path(), "/v25.0/102290129340398/assigned_users");
        assert_eq!(
            reqs[2].query("business").as_deref(),
            Some("2729063490586005")
        );
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
            .assigned_users_stream(
                &ListAssignedUsers::new(BusinessId::new("2729063490586005").unwrap()).before("x"),
            )
            .collect()
            .await;
        assert!(
            matches!(&refused[..], [Err(meta_whatsapp_core::Error::Validation(v))] if v.field == "before"),
            "{refused:?}"
        );
        for limit in [0, 101] {
            let err = w
                .assigned_users(
                    &ListAssignedUsers::new(BusinessId::new("2729063490586005").unwrap())
                        .limit(limit),
                )
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
                    "next": "https://graph.facebook.com/v25.0/102290129340398/assigned_users?after=MjQZD"}}),
        );
        t.push_json(
            200,
            json!({"data": [{"id": "2", "name": "Jasper Brown", "tasks": ["DEVELOP"]}]}),
        );
        let users: Vec<String> = client(&t)
            .waba(WabaId::new("102290129340398").unwrap())
            .assigned_users_stream(&ListAssignedUsers::new(
                BusinessId::new("2729063490586005").unwrap(),
            ))
            .map(|u| u.unwrap().id)
            .collect()
            .await;
        assert_eq!(users, ["1", "2"]);
        let second = &t.requests()[1];
        assert_eq!(second.query("after").as_deref(), Some("MjQZD"));
        assert_eq!(
            second.query("business").as_deref(),
            Some("2729063490586005")
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn create_phone_number_posts_guide_example() {
        // solution-providers/registering-phone-numbers, step 1.
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"id": "110200345501442"}));
        let created = client(&t)
            .waba(WabaId::new("102290129340398").unwrap())
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
                .waba(WabaId::new("1").unwrap())
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
        let w = client(&t).waba(WabaId::new("102290129340398").unwrap());
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
        c.waba(WabaId::new("1").unwrap())
            .subscribe_app(None)
            .await
            .unwrap();
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
        let page = client(&t)
            .waba(WabaId::new("102290129340398").unwrap())
            .subscribed_apps()
            .await
            .unwrap();
        let app = &page.data[0];
        assert_eq!(app.whatsapp_business_api_data.id, "670843887433847");
        assert_eq!(
            app.override_callback_uri.as_deref(),
            Some("https://my-waba-alternate-callback.com/webhook")
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/102290129340398/subscribed_apps");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn unsubscribe_is_a_delete() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .waba(WabaId::new("102289599326934").unwrap())
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
        let err = client(&t)
            .waba(WabaId::new("1").unwrap())
            .subscribe_app(None)
            .await
            .unwrap_err();
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
        let w = client(&t).waba(WabaId::new("102290129340398").unwrap());
        let users = w
            .assigned_users(&ListAssignedUsers::new(
                BusinessId::new("2729063490586005").unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(users.data[0].tasks, vec![WabaTask::Manage]);
        w.assign_user("1972555232742222", &[WabaTask::Manage])
            .await
            .unwrap();
        w.remove_user("1972555232742222").await.unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].path(), "/v25.0/102290129340398/assigned_users");
        assert_eq!(
            reqs[0].query("business").as_deref(),
            Some("2729063490586005")
        );
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
        c.waba(WabaId::new_unchecked("OTHER/subscribed_apps"))
            .subscribe_app(None)
            .await
            .unwrap();
        c.waba(WabaId::new_unchecked("123?fields=x"))
            .unsubscribe_app()
            .await
            .unwrap();
        c.waba(WabaId::new_unchecked("102290129340398/assigned_users"))
            .remove_user("1")
            .await
            .unwrap();
        c.waba(WabaId::new_unchecked("102290129340398/../OTHER"))
            .update(&WabaUpdate::new().name("n"))
            .await
            .unwrap();
        c.business(BusinessId::new_unchecked(
            "2729063490586005/owned_whatsapp_business_accounts",
        ))
        .client_whatsapp_business_accounts(&WabaListQuery::new())
        .await
        .unwrap();
        let paths: Vec<String> = t.requests().iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(
            paths,
            vec![
                "/v25.0/OTHER%2Fsubscribed_apps/subscribed_apps",
                "/v25.0/123%3Ffields=x/subscribed_apps",
                "/v25.0/102290129340398%2Fassigned_users/assigned_users",
                "/v25.0/102290129340398%2F..%2FOTHER",
                "/v25.0/2729063490586005%2Fowned_whatsapp_business_accounts/client_whatsapp_business_accounts",
            ]
        );
        for id in ["..", "."] {
            assert!(matches!(
                c.waba(WabaId::new_unchecked(id)).subscribe_app(None).await,
                Err(meta_whatsapp_core::Error::Validation(_))
            ));
            assert!(c.waba(WabaId::new_unchecked(id)).get(&[]).await.is_err());
            let streamed: Vec<_> = c
                .waba(WabaId::new_unchecked(id))
                .phone_numbers_stream(&PhoneNumbersQuery::new())
                .take(3)
                .collect()
                .await;
            assert_eq!(streamed.len(), 1);
            assert!(matches!(
                streamed[0],
                Err(meta_whatsapp_core::Error::Validation(_))
            ));
            assert!(
                c.business(BusinessId::new_unchecked(id))
                    .get(&[])
                    .await
                    .is_err()
            );
        }
        assert_eq!(t.requests().len(), 5, "invalid ids never reach the wire");
        assert_eq!(t.remaining(), 0);
    }
}
