//! A WABA's activity history
//! (`reference/whatsapp-business-account/whatsapp-business-account-activities-api`)
//! and the Multi-Partner Solutions it belongs to
//! (`reference/whatsapp-business-account/whatsapp-business-account-solutions-list-api`).

use futures::Stream;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::ids::{AppId, BusinessId};
use meta_whatsapp_core::paging::Page;
use serde::{Deserialize, Serialize};

use super::types::WabaTask;
use super::{Waba, check_limit, fields_query};
use crate::GraphRequest;
use crate::request::{paginate_or_error, reject_cursors};
use crate::templates::macros::string_enum;

/// Options for a paged WABA read that takes fields, a page size and
/// cursors: [`Waba::activities`] and [`Waba::solutions`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListWabaRecords {
    /// Fields to return (empty = Meta's defaults, which each method names).
    pub fields: Vec<String>,
    /// Page size, 1–100.
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`; for the
    /// one-page methods only, the streams manage their own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl ListWabaRecords {
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

string_enum! {
    /// The `category` of an activity (`WhatsAppBusinessAccountHistoryComponentType`).
    pub enum ActivityCategory {
        /// The account.
        Account => "ACCOUNT",
        /// An app.
        App => "APP",
        /// Billing.
        Billing => "BILLING",
        /// The business profile.
        BusinessProfile => "BUSINESS_PROFILE",
        /// A message template.
        MessageTemplate => "MESSAGE_TEMPLATE",
        /// The payment configuration.
        PaymentConfiguration => "PAYMENT_CONFIGURATION",
        /// A phone number.
        PhoneNumber => "PHONE_NUMBER",
        /// A user.
        User => "USER",
        /// The WhatsApp Business payment terms of service.
        WhatsappBusinessPaymentTos => "WHATSAPP_BUSINESS_PAYMENT_TOS",
    }
}

string_enum! {
    /// The `event_type` of an activity (`WhatsAppBusinessAccountHistoryEventType`,
    /// every value of the reference).
    pub enum ActivityEventType {
        /// Migrated from On-Behalf-Of to shared.
        AccountMigratedFromOboToShared => "ACCOUNT_MIGRATED_FROM_OBO_TO_SHARED",
        /// An intent to migrate from On-Behalf-Of to shared.
        AccountMigrationFromOboToSharedIntent => "ACCOUNT_MIGRATION_FROM_OBO_TO_SHARED_INTENT",
        /// The review status changed.
        AccountReviewStatusUpdated => "ACCOUNT_REVIEW_STATUS_UPDATED",
        /// Eligible for the sandbox.
        AccountSandboxEligible => "ACCOUNT_SANDBOX_ELIGIBLE",
        /// Verified.
        AccountVerified => "ACCOUNT_VERIFIED",
        /// An app was installed.
        AppInstalled => "APP_INSTALLED",
        /// An app was uninstalled.
        AppUninstalled => "APP_UNINSTALLED",
        /// Billing added.
        BillingAdded => "BILLING_ADDED",
        /// Billing deleted.
        BillingDeleted => "BILLING_DELETED",
        /// Billing updated.
        BillingUpdated => "BILLING_UPDATED",
        /// A bot was created.
        BotCreated => "BOT_CREATED",
        /// A bot was updated.
        BotUpdated => "BOT_UPDATED",
        /// A business capability changed.
        BusinessCapabilityUpdated => "BUSINESS_CAPABILITY_UPDATED",
        /// The business's primary location country changed.
        BusinessPrimaryLocationCountryUpdated => "BUSINESS_PRIMARY_LOCATION_COUNTRY_UPDATED",
        /// The profile picture changed.
        BusinessProfilePictureUpdated => "BUSINESS_PROFILE_PICTURE_UPDATED",
        /// The business profile changed.
        BusinessProfileUpdated => "BUSINESS_PROFILE_UPDATED",
        /// The calling configuration changed.
        CallingConfigChanged => "CALLING_CONFIG_CHANGED",
        /// The calling status changed.
        CallingStatusChanged => "CALLING_STATUS_CHANGED",
        /// The data localization setting changed.
        DataLocalizationSettingChanged => "DATA_LOCALIZATION_SETTING_CHANGED",
        /// Meta's `DEFAULT`.
        Default => "DEFAULT",
        /// A template was added.
        MessageTemplateAdded => "MESSAGE_TEMPLATE_ADDED",
        /// A template was deleted.
        MessageTemplateDeleted => "MESSAGE_TEMPLATE_DELETED",
        /// A template was edited.
        MessageTemplateEdited => "MESSAGE_TEMPLATE_EDITED",
        /// A display name reverification was requested (Meta spells the
        /// value `REVERFICATION`).
        NameReverificationRequested => "NAME_REVERFICATION_REQUESTED",
        /// An Official Business Account request.
        OfficialBusinessAccountRequest => "OFFICIAL_BUSINESS_ACCOUNT_REQUEST",
        /// A one-time password was requested.
        OtpRequested => "OTP_REQUESTED",
        /// A one-time password was verified.
        OtpVerified => "OTP_VERIFIED",
        /// A partner was added.
        PartnerAdded => "PARTNER_ADDED",
        /// A partner was removed.
        PartnerRemoved => "PARTNER_REMOVED",
        /// The payment configuration changed.
        PaymentConfigurationUpdated => "PAYMENT_CONFIGURATION_UPDATED",
        /// A phone number migration completed.
        PhoneMigrationCompleted => "PHONE_MIGRATION_COMPLETED",
        /// A phone number migration started.
        PhoneMigrationStarted => "PHONE_MIGRATION_STARTED",
        /// A phone number was added.
        PhoneNumberAdded => "PHONE_NUMBER_ADDED",
        /// A phone number was deleted.
        PhoneNumberDeleted => "PHONE_NUMBER_DELETED",
        /// A phone number was deregistered from Cloud API.
        PhoneNumberDeregisteredFromCloudApi => "PHONE_NUMBER_DEREGISTERED_FROM_CLOUD_API",
        /// A phone number's Official Business Account was approved.
        PhoneNumberObaApproved => "PHONE_NUMBER_OBA_APPROVED",
        /// A phone number was registered on Cloud API.
        PhoneNumberRegisteredOnCloudApi => "PHONE_NUMBER_REGISTERED_ON_CLOUD_API",
        /// Search visibility changed to not visible.
        SearchStatusChangedToNonVisible => "SEARCH_STATUS_CHANGED_TO_NON_VISIBLE",
        /// Search visibility changed to pending visible.
        SearchStatusChangedToPendingVisible => "SEARCH_STATUS_CHANGED_TO_PENDING_VISIBLE",
        /// Search visibility changed to visible.
        SearchStatusChangedToVisible => "SEARCH_STATUS_CHANGED_TO_VISIBLE",
        /// The two-step verification PIN changed.
        TwoStepVerificationPinChanged => "TWO_STEP_VERIFICATION_PIN_CHANGED",
        /// The two-step verification PIN was removed.
        TwoStepVerificationPinRemoved => "TWO_STEP_VERIFICATION_PIN_REMOVED",
        /// A two-step verification PIN reset was requested.
        TwoStepVerificationPinResetRequested => "TWO_STEP_VERIFICATION_PIN_RESET_REQUESTED",
        /// A user was added.
        UserAdded => "USER_ADDED",
        /// A user was deleted.
        UserDeleted => "USER_DELETED",
        /// A user permission was added.
        UserPermissionAdded => "USER_PERMISSION_ADDED",
        /// A user permission was updated.
        UserPermissionUpdated => "USER_PERMISSION_UPDATED",
        /// A welcome message sequence was added.
        WelcomeMessageSequenceAdded => "WELCOME_MESSAGE_SEQUENCE_ADDED",
        /// A welcome message sequence was deleted.
        WelcomeMessageSequenceDeleted => "WELCOME_MESSAGE_SEQUENCE_DELETED",
        /// A welcome message sequence was updated.
        WelcomeMessageSequenceUpdated => "WELCOME_MESSAGE_SEQUENCE_UPDATED",
        /// The WhatsApp Business payment terms of service were accepted.
        WhatsappBusinessPaymentTosAccepted => "WHATSAPP_BUSINESS_PAYMENT_TOS_ACCEPTED",
    }
}

/// One record of a WABA's activity history (`WhatsAppBusinessActivityHistory`).
/// Every field is optional in the reference; Meta's defaults are
/// `event_type` and `timestamp`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct WabaActivity {
    /// The record's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The business that performed the activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_business_id: Option<BusinessId>,
    /// The user or system that performed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    /// What it concerns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<ActivityCategory>,
    /// What happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<ActivityEventType>,
    /// When, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
}

string_enum! {
    /// A Multi-Partner Solution's effective `status`
    /// (`WhatsAppBusinessSolutionStatus`).
    pub enum SolutionStatus {
        /// A draft.
        Draft => "DRAFT",
        /// Initiated.
        Initiated => "INITIATED",
        /// Active.
        Active => "ACTIVE",
        /// Rejected.
        Rejected => "REJECTED",
        /// Deactivated.
        Deactivated => "DEACTIVATED",
    }
}

string_enum! {
    /// A Multi-Partner Solution's `status_for_pending_request`
    /// (`WhatsAppBusinessSolutionPendingStatus`).
    pub enum SolutionPendingStatus {
        /// Waiting to be activated.
        PendingActivation => "PENDING_ACTIVATION",
        /// Waiting to be deactivated.
        PendingDeactivation => "PENDING_DEACTIVATION",
        /// Nothing pending (`NONE`).
        NothingPending => "NONE",
    }
}

/// `{id, name}` of a Meta app (`ApplicationNode`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ApplicationRef {
    /// The app's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<AppId>,
    /// The app's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A Multi-Partner Solution a WABA belongs to (`WhatsAppBusinessSolution`).
///
/// Which fields are present depends on `fields`: Meta's defaults are
/// `name`, `status` and `status_for_pending_request`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct WabaSolution {
    /// The solution's id (absent unless requested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its effective status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SolutionStatus>,
    /// A status change waiting for approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_for_pending_request: Option<SolutionPendingStatus>,
    /// The app that owns the solution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_app: Option<ApplicationRef>,
    /// The WABA permissions granted to the solution's owner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owner_permissions: Vec<WabaTask>,
}

impl Waba {
    fn records_request(
        &self,
        edge: &'static str,
        context: &'static str,
        query: &ListWabaRecords,
    ) -> Result<GraphRequest> {
        check_limit(query.limit)?;
        Ok(self
            .client
            .get_at(&[self.waba_id.as_str(), edge])
            .query_opt("fields", fields_query(&query.fields))
            .query_opt("limit", query.limit)
            .context(context))
    }

    pub(super) fn records_page(
        &self,
        edge: &'static str,
        context: &'static str,
        query: &ListWabaRecords,
    ) -> Result<GraphRequest> {
        Ok(self
            .records_request(edge, context, query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref()))
    }

    pub(super) fn records_stream<T>(
        &self,
        edge: &'static str,
        context: &'static str,
        query: &ListWabaRecords,
    ) -> impl Stream<Item = Result<T>> + Send + 'static + use<T>
    where
        T: serde::de::DeserializeOwned + Send + 'static,
    {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.records_request(edge, context, query)),
        )
    }

    /// `GET /{WABA_ID}/activities`, one page: the WABA's activity history
    /// (default fields `event_type` and `timestamp`; the others: `id`,
    /// `actor_business_id`, `actor_id`, `category`). The next page: the
    /// same query with `after` set to this page's [`Page::next_cursor`].
    pub async fn activities(&self, query: &ListWabaRecords) -> Result<Page<WabaActivity>> {
        self.records_page("activities", "WABA activities", query)?
            .send()
            .await
    }

    /// All activities, following cursors. The stream manages them itself:
    /// a query with `after` or `before` set is refused (the stream's single
    /// item is that validation error).
    pub fn activities_stream(
        &self,
        query: &ListWabaRecords,
    ) -> impl Stream<Item = Result<WabaActivity>> + Send + 'static + use<> {
        self.records_stream("activities", "WABA activities", query)
    }

    /// `GET /{WABA_ID}/solutions`, one page: the Multi-Partner Solutions
    /// this WABA belongs to (default fields `name`, `status`,
    /// `status_for_pending_request`; the others: `id`, `owner_app`,
    /// `owner_permissions`). The next page: the same query with `after` set
    /// to this page's [`Page::next_cursor`].
    pub async fn solutions(&self, query: &ListWabaRecords) -> Result<Page<WabaSolution>> {
        self.records_page("solutions", "WABA solutions", query)?
            .send()
            .await
    }

    /// All solutions, following cursors. The stream manages them itself: a
    /// query with `after` or `before` set is refused (the stream's single
    /// item is that validation error).
    pub fn solutions_stream(
        &self,
        query: &ListWabaRecords,
    ) -> impl Stream<Item = Result<WabaSolution>> + Send + 'static + use<> {
        self.records_stream("solutions", "WABA solutions", query)
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use meta_whatsapp_core::ids::WabaId;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use meta_whatsapp_core::{Error, ErrorKind};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::{Client, RetryPolicy};

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn waba(t: &ScriptedTransport) -> Waba {
        client(t).waba(WabaId::new("102290129340398").unwrap())
    }

    /// The `ActivityList` schema with every field of
    /// `WhatsAppBusinessActivityHistory` (the page prints no example;
    /// values ours).
    fn activities_example() -> serde_json::Value {
        json!({
            "data": [
                {
                    "id": "1234567890",
                    "actor_business_id": "805021500648488",
                    "actor_id": "1972555232742222",
                    "category": "PHONE_NUMBER",
                    "event_type": "PHONE_NUMBER_REGISTERED_ON_CLOUD_API",
                    "timestamp": 1_767_225_600
                },
                {"event_type": "NAME_REVERFICATION_REQUESTED", "timestamp": 1_767_225_000},
                {"event_type": "SOMETHING_NEW", "category": "SOMEWHERE_NEW"}
            ],
            "paging": {"cursors": {"before": "QVFI", "after": "QVFJ"}}
        })
    }

    #[tokio::test]
    async fn activities_send_the_documented_query_and_parse_every_field() {
        let t = ScriptedTransport::new();
        t.push_json(200, activities_example());
        let page = waba(&t)
            .activities(
                &ListWabaRecords::new()
                    .fields([
                        "id",
                        "actor_business_id",
                        "actor_id",
                        "category",
                        "event_type",
                        "timestamp",
                    ])
                    .limit(50)
                    .after("QVFJ"),
            )
            .await
            .unwrap();
        let first = &page.data[0];
        assert_eq!(first.id.as_deref(), Some("1234567890"));
        assert_eq!(
            first.actor_business_id.as_ref().map(BusinessId::as_str),
            Some("805021500648488")
        );
        assert_eq!(first.actor_id.as_deref(), Some("1972555232742222"));
        assert_eq!(first.category, Some(ActivityCategory::PhoneNumber));
        assert_eq!(
            first.event_type,
            Some(ActivityEventType::PhoneNumberRegisteredOnCloudApi)
        );
        assert_eq!(first.timestamp, Some(1_767_225_600));
        assert_eq!(
            page.data[1].event_type,
            Some(ActivityEventType::NameReverificationRequested)
        );
        assert_eq!(
            page.data[2].event_type,
            Some(ActivityEventType::Other("SOMETHING_NEW".into()))
        );
        assert_eq!(page.next_cursor(), None, "no `next` link: the last page");
        // What parses writes back as Meta sent it.
        assert_eq!(
            serde_json::to_value(&page.data).unwrap(),
            activities_example()["data"]
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/102290129340398/activities");
        assert_eq!(
            req.query("fields").as_deref(),
            Some("id,actor_business_id,actor_id,category,event_type,timestamp")
        );
        assert_eq!(req.query("limit").as_deref(), Some("50"));
        assert_eq!(req.query("after").as_deref(), Some("QVFJ"));
        assert_eq!(req.query("before"), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    /// Every documented category and event type parses to its own variant
    /// and writes back unchanged.
    #[test]
    fn every_documented_activity_value_round_trips() {
        let categories = [
            "ACCOUNT",
            "APP",
            "BILLING",
            "BUSINESS_PROFILE",
            "MESSAGE_TEMPLATE",
            "PAYMENT_CONFIGURATION",
            "PHONE_NUMBER",
            "USER",
            "WHATSAPP_BUSINESS_PAYMENT_TOS",
        ];
        for value in categories {
            let parsed: ActivityCategory = serde_json::from_value(json!(value)).unwrap();
            assert!(!matches!(parsed, ActivityCategory::Other(_)), "{value}");
            assert_eq!(parsed.as_str(), value);
        }
        let events = [
            "ACCOUNT_MIGRATED_FROM_OBO_TO_SHARED",
            "ACCOUNT_MIGRATION_FROM_OBO_TO_SHARED_INTENT",
            "ACCOUNT_REVIEW_STATUS_UPDATED",
            "ACCOUNT_SANDBOX_ELIGIBLE",
            "ACCOUNT_VERIFIED",
            "APP_INSTALLED",
            "APP_UNINSTALLED",
            "BILLING_ADDED",
            "BILLING_DELETED",
            "BILLING_UPDATED",
            "BOT_CREATED",
            "BOT_UPDATED",
            "BUSINESS_CAPABILITY_UPDATED",
            "BUSINESS_PRIMARY_LOCATION_COUNTRY_UPDATED",
            "BUSINESS_PROFILE_PICTURE_UPDATED",
            "BUSINESS_PROFILE_UPDATED",
            "CALLING_CONFIG_CHANGED",
            "CALLING_STATUS_CHANGED",
            "DATA_LOCALIZATION_SETTING_CHANGED",
            "DEFAULT",
            "MESSAGE_TEMPLATE_ADDED",
            "MESSAGE_TEMPLATE_DELETED",
            "MESSAGE_TEMPLATE_EDITED",
            "NAME_REVERFICATION_REQUESTED",
            "OFFICIAL_BUSINESS_ACCOUNT_REQUEST",
            "OTP_REQUESTED",
            "OTP_VERIFIED",
            "PARTNER_ADDED",
            "PARTNER_REMOVED",
            "PAYMENT_CONFIGURATION_UPDATED",
            "PHONE_MIGRATION_COMPLETED",
            "PHONE_MIGRATION_STARTED",
            "PHONE_NUMBER_ADDED",
            "PHONE_NUMBER_DELETED",
            "PHONE_NUMBER_DEREGISTERED_FROM_CLOUD_API",
            "PHONE_NUMBER_OBA_APPROVED",
            "PHONE_NUMBER_REGISTERED_ON_CLOUD_API",
            "SEARCH_STATUS_CHANGED_TO_NON_VISIBLE",
            "SEARCH_STATUS_CHANGED_TO_PENDING_VISIBLE",
            "SEARCH_STATUS_CHANGED_TO_VISIBLE",
            "TWO_STEP_VERIFICATION_PIN_CHANGED",
            "TWO_STEP_VERIFICATION_PIN_REMOVED",
            "TWO_STEP_VERIFICATION_PIN_RESET_REQUESTED",
            "USER_ADDED",
            "USER_DELETED",
            "USER_PERMISSION_ADDED",
            "USER_PERMISSION_UPDATED",
            "WELCOME_MESSAGE_SEQUENCE_ADDED",
            "WELCOME_MESSAGE_SEQUENCE_DELETED",
            "WELCOME_MESSAGE_SEQUENCE_UPDATED",
            "WHATSAPP_BUSINESS_PAYMENT_TOS_ACCEPTED",
        ];
        assert_eq!(events.len(), 51);
        let mut seen = std::collections::HashSet::new();
        for value in events {
            let parsed: ActivityEventType = serde_json::from_value(json!(value)).unwrap();
            assert!(!matches!(parsed, ActivityEventType::Other(_)), "{value}");
            assert_eq!(serde_json::to_value(&parsed).unwrap(), json!(value));
            assert!(seen.insert(parsed), "{value} maps to a variant twice");
        }
    }

    #[tokio::test]
    async fn activities_stream_follows_cursors_and_refuses_the_callers() {
        let t = ScriptedTransport::new();
        let w = waba(&t);
        for query in [
            ListWabaRecords::new().after("x"),
            ListWabaRecords::new().before("x"),
        ] {
            let activities: Vec<_> = w.activities_stream(&query).collect().await;
            let solutions: Vec<_> = w.solutions_stream(&query).collect().await;
            assert!(
                matches!(&activities[..], [Err(Error::Validation(_))]),
                "{activities:?}"
            );
            assert!(
                matches!(&solutions[..], [Err(Error::Validation(_))]),
                "{solutions:?}"
            );
        }
        for limit in [0, 101] {
            let query = ListWabaRecords::new().limit(limit);
            assert!(
                matches!(w.activities(&query).await, Err(Error::Validation(v)) if v.field == "limit")
            );
            assert!(
                matches!(w.solutions(&query).await, Err(Error::Validation(v)) if v.field == "limit")
            );
        }
        assert!(t.requests().is_empty());

        t.push_json(
            200,
            json!({"data": [{"event_type": "USER_ADDED"}], "paging": {"cursors": {"after": "a"}, "next": "https://graph.facebook.com/x"}}),
        );
        t.push_json(200, json!({"data": [{"event_type": "USER_DELETED"}]}));
        let events: Vec<_> = w
            .activities_stream(&ListWabaRecords::new().limit(1))
            .map(|a| a.unwrap().event_type.unwrap())
            .collect()
            .await;
        assert_eq!(
            events,
            [ActivityEventType::UserAdded, ActivityEventType::UserDeleted]
        );
        let reqs = t.requests();
        assert_eq!(reqs[0].path(), "/v25.0/102290129340398/activities");
        assert_eq!(reqs[1].query("after").as_deref(), Some("a"));
        assert_eq!(reqs[1].query("limit").as_deref(), Some("1"));
        assert_eq!(t.remaining(), 0);
    }

    /// The `SolutionsList` schema with every field (no example on the page;
    /// values ours).
    fn solutions_example() -> serde_json::Value {
        json!({
            "data": [
                {
                    "id": "1386924215693432",
                    "name": "Lucky Shrub and Partner",
                    "status": "ACTIVE",
                    "status_for_pending_request": "NONE",
                    "owner_app": {"id": "670843887433847", "name": "JaspersMarket"},
                    "owner_permissions": ["DEVELOP", "MANAGE_TEMPLATES", "VIEW_COST"]
                },
                {"name": "Draft", "status": "DRAFT", "status_for_pending_request": "PENDING_ACTIVATION"}
            ],
            "paging": {"cursors": {"before": "QVFI", "after": "QVFJ"}}
        })
    }

    #[tokio::test]
    async fn solutions_send_the_documented_query_and_parse_every_field() {
        let t = ScriptedTransport::new();
        t.push_json(200, solutions_example());
        let page = waba(&t)
            .solutions(
                &ListWabaRecords::new()
                    .fields([
                        "id",
                        "name",
                        "status",
                        "status_for_pending_request",
                        "owner_app",
                        "owner_permissions",
                    ])
                    .limit(100)
                    .before("QVFI"),
            )
            .await
            .unwrap();
        let s = &page.data[0];
        assert_eq!(s.id.as_deref(), Some("1386924215693432"));
        assert_eq!(s.status, Some(SolutionStatus::Active));
        assert_eq!(
            s.status_for_pending_request,
            Some(SolutionPendingStatus::NothingPending)
        );
        let app = s.owner_app.as_ref().unwrap();
        assert_eq!(app.id.as_ref().map(AppId::as_str), Some("670843887433847"));
        assert_eq!(
            s.owner_permissions,
            [
                WabaTask::Develop,
                WabaTask::ManageTemplates,
                WabaTask::ViewCost
            ]
        );
        assert_eq!(
            page.data[1].status_for_pending_request,
            Some(SolutionPendingStatus::PendingActivation)
        );
        assert_eq!(
            serde_json::to_value(&page.data).unwrap(),
            solutions_example()["data"]
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/102290129340398/solutions");
        assert_eq!(
            req.query("fields").as_deref(),
            Some("id,name,status,status_for_pending_request,owner_app,owner_permissions")
        );
        assert_eq!(req.query("limit").as_deref(), Some("100"));
        assert_eq!(req.query("before").as_deref(), Some("QVFI"));
        assert_eq!(t.remaining(), 0);

        // Meta's defaults: no query.
        t.push_json(200, json!({"data": []}));
        waba(&t).solutions(&ListWabaRecords::new()).await.unwrap();
        assert_eq!(t.last_request().unwrap().url.query(), None);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn the_pages_error_examples_keep_their_kind() {
        let t = ScriptedTransport::new();
        // The solutions page's 404 example.
        t.push_json(
            404,
            json!({"error": {"message": "WhatsApp Business Account not found", "type": "GraphMethodException", "code": 803, "fbtrace_id": "FAKE_TRACE_ID_123ABC456DEF"}}),
        );
        // The activities page's 401 example.
        t.push_json(
            401,
            json!({"error": {"message": "Invalid OAuth access token", "type": "OAuthException", "code": 190, "error_subcode": 463, "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
        );
        let w = waba(&t);
        let missing = w.solutions(&ListWabaRecords::new()).await.unwrap_err();
        assert_eq!(missing.graph().map(|g| g.code), Some(803));
        let auth = w.activities(&ListWabaRecords::new()).await.unwrap_err();
        assert_eq!(auth.kind(), ErrorKind::Authentication);
        assert_eq!(t.remaining(), 0);
    }
}
