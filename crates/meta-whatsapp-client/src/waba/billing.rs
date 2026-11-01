//! Changing a WABA's billing currency or payment method
//! (`pricing/change-billing-currency`): set the migration intent on the
//! source WABA, poll the migration, then resume it to switch billing. Two
//! explicit calls, see the module docs of [`crate::waba`].

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::CreditLineId;
use serde::{Deserialize, Serialize};

use super::Waba;
use super::accounts::check_currency;
use super::types::WabaInfo;
use crate::Client;
use crate::templates::macros::string_enum;

/// Body of `POST /{WABA_ID}/set_payment_method_migration_intent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct PaymentMethodMigrationIntent {
    /// The clone's currency (required), e.g. `INR`.
    pub currency: String,
    /// The Line of Credit to attach to the clone; only when moving to Line
    /// of Credit billing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extended_credit_id: Option<CreditLineId>,
}

impl PaymentMethodMigrationIntent {
    /// Migrate to `currency` (an ISO 4217 code).
    pub fn new(currency: impl Into<String>) -> Self {
        Self {
            currency: currency.into(),
            extended_credit_id: None,
        }
    }

    /// Attach this Line of Credit to the clone.
    #[must_use]
    pub fn extended_credit_id(mut self, id: impl Into<CreditLineId>) -> Self {
        self.extended_credit_id = Some(id.into());
        self
    }

    /// The currency is an ISO 4217 code (three uppercase letters, the
    /// WABA's `currency` format); a credit line id, when set, is not blank.
    ///
    /// # Errors
    ///
    /// A [`ValidationError`] on `currency` or `extended_credit_id`.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        check_currency("currency", &self.currency)?;
        if self
            .extended_credit_id
            .as_ref()
            .is_some_and(|id| id.as_str().trim().is_empty())
        {
            return Err(ValidationError::new(
                "extended_credit_id",
                "must not be blank when set",
            ));
        }
        Ok(())
    }
}

string_enum! {
    /// Where a migration stands (the page's § Migration status values).
    pub enum PaymentMigrationStatus {
        /// Initiated: the source WABA is being validated.
        Initiated => "INITIATED",
        /// Accepted and queued.
        Accepted => "ACCEPTED",
        /// Templates and their Flows are being cloned.
        InProgress => "IN_PROGRESS",
        /// Cloning is done: [`PaymentMethodMigration::resume_migration`]
        /// may be called.
        ReadyToComplete => "READY_TO_COMPLETE",
        /// The clone is the billing account.
        Completed => "COMPLETED",
        /// Cloning failed: set a new intent to start again.
        Failed => "FAILED",
        /// Switching billing failed: resume again.
        FailedToComplete => "FAILED_TO_COMPLETE",
        /// Refused on validation.
        Rejected => "REJECTED",
    }
}

/// The answer to [`Waba::set_payment_method_migration_intent`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct MigrationStarted {
    /// The migration's id (`mig_…` in Meta's example): what
    /// [`Client::payment_method_migration`] takes.
    pub migration_id: String,
    /// Its status.
    pub migration_status: PaymentMigrationStatus,
}

/// A migration, from [`PaymentMethodMigration::status`] or
/// [`PaymentMethodMigration::resume_migration`].
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct MigrationState {
    /// The migration's id.
    pub id: String,
    /// Its status.
    pub status: PaymentMigrationStatus,
    /// The clone: `id`, `name`, `currency`, `timezone_id`,
    /// `message_template_namespace` in Meta's example.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_waba: Option<WabaInfo>,
}

impl Waba {
    /// `POST /{WABA_ID}/set_payment_method_migration_intent`: start moving
    /// this WABA to another currency or payment method. Meta validates it,
    /// creates the clone and starts migrating its assets; poll
    /// [`PaymentMethodMigration::status`] with the returned id.
    ///
    /// An explicit call that nothing else in the library makes (see the
    /// [module docs](crate::waba#the-billing-migration-two-explicit-calls)).
    /// Checked first
    /// ([`PaymentMethodMigrationIntent::validate`]), with no request on a
    /// refusal. **Never replayed** on a timeout or a transient error: it
    /// creates a WABA; only an error proving Meta refused it is resent
    /// ([`crate::RetryPolicy`]).
    pub async fn set_payment_method_migration_intent(
        &self,
        intent: &PaymentMethodMigrationIntent,
    ) -> Result<MigrationStarted> {
        intent.validate()?;
        self.client
            .post_at(&[self.waba_id.as_str(), "set_payment_method_migration_intent"])
            .json(intent)
            .context("payment method migration intent response")
            .send()
            .await
    }
}

/// Entry point, see [`Client::payment_method_migration`].
#[derive(Debug, Clone)]
pub struct PaymentMethodMigration {
    client: Client,
    migration_id: String,
}

impl Client {
    /// [`PaymentMethodMigration`] API for the `migration_id`
    /// [`Waba::set_payment_method_migration_intent`] returned.
    pub fn payment_method_migration(
        &self,
        migration_id: impl Into<String>,
    ) -> PaymentMethodMigration {
        PaymentMethodMigration {
            client: self.clone(),
            migration_id: migration_id.into(),
        }
    }
}

impl PaymentMethodMigration {
    /// The id this API is scoped to.
    pub fn id(&self) -> &str {
        &self.migration_id
    }

    fn checked_id(&self) -> Result<&str> {
        if self.migration_id.trim().is_empty() {
            return Err(ValidationError::new("migration_id", "required").into());
        }
        Ok(&self.migration_id)
    }

    /// `GET /{MIGRATION_ID}`: where the migration stands. Poll until
    /// [`PaymentMigrationStatus::ReadyToComplete`] or [`PaymentMigrationStatus::Completed`].
    pub async fn status(&self) -> Result<MigrationState> {
        self.client
            .get_at(&[self.checked_id()?])
            .context("payment method migration")
            .send()
            .await
    }

    /// `POST /{MIGRATION_ID}/resume_migration`: complete the migration,
    /// making the clone the billing account of the numbers; the old WABA
    /// can no longer send, and keeps its history. Call it once the status
    /// is [`PaymentMigrationStatus::ReadyToComplete`], with a payment method on the
    /// clone (Meta does not complete the migration otherwise).
    ///
    /// An explicit call that nothing else in the library makes (see the
    /// [module docs](crate::waba#the-billing-migration-two-explicit-calls)).
    /// **Never replayed** on a timeout or a transient
    /// error ([`crate::RetryPolicy`]): read [`Self::status`] first, and
    /// call again only on [`PaymentMigrationStatus::FailedToComplete`], as Meta's
    /// troubleshooting table says.
    pub async fn resume_migration(&self) -> Result<MigrationState> {
        self.client
            .post_at(&[self.checked_id()?, "resume_migration"])
            .context("resume migration response")
            .send()
            .await
    }
}

#[cfg(test)]
mod tests {
    use http::Method;
    use meta_whatsapp_core::Error;
    use meta_whatsapp_core::error::TransportError;
    use meta_whatsapp_core::ids::WabaId;
    use meta_whatsapp_core::testing::{RecordedBody, ScriptedTransport};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::RetryPolicy;

    fn client(t: &ScriptedTransport, retry: RetryPolicy) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(retry)
            .build()
            .unwrap()
    }

    fn retrying() -> RetryPolicy {
        RetryPolicy {
            max_retries: 3,
            base_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
        }
    }

    /// The page's § Initiate migration, example request and response.
    #[tokio::test]
    async fn the_intent_sends_the_pages_example() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"migration_id": "mig_01HZYK3ABCDEF4567890", "migration_status": "INITIATED"}),
        );
        let started = client(&t, RetryPolicy::NONE)
            .waba(WabaId::new("123456789012345").unwrap())
            .set_payment_method_migration_intent(
                &PaymentMethodMigrationIntent::new("INR").extended_credit_id("987654321098765"),
            )
            .await
            .unwrap();
        assert_eq!(started.migration_id, "mig_01HZYK3ABCDEF4567890");
        assert_eq!(started.migration_status, PaymentMigrationStatus::Initiated);
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            "/v25.0/123456789012345/set_payment_method_migration_intent"
        );
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({"currency": "INR", "extended_credit_id": "987654321098765"}))
        );
        assert_eq!(t.remaining(), 0);

        // Credit card billing: the credit line is omitted.
        t.push_json(
            200,
            json!({"migration_id": "mig_2", "migration_status": "ACCEPTED"}),
        );
        client(&t, RetryPolicy::NONE)
            .waba(WabaId::new("123456789012345").unwrap())
            .set_payment_method_migration_intent(&PaymentMethodMigrationIntent::new("MXN"))
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().json(),
            Some(json!({"currency": "MXN"}))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn the_intent_is_checked_before_any_request() {
        let t = ScriptedTransport::new();
        let w = client(&t, RetryPolicy::NONE).waba(WabaId::new("1").unwrap());
        for (intent, field) in [
            (PaymentMethodMigrationIntent::new(""), "currency"),
            (PaymentMethodMigrationIntent::new("inr"), "currency"),
            (PaymentMethodMigrationIntent::new("INRR"), "currency"),
            (
                PaymentMethodMigrationIntent::new("INR").extended_credit_id(" "),
                "extended_credit_id",
            ),
        ] {
            let err = w
                .set_payment_method_migration_intent(&intent)
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == field),
                "{field}: {err:?}"
            );
        }
        assert!(t.requests().is_empty());
    }

    /// The page's § Check migration status, example response.
    #[tokio::test]
    async fn the_status_parses_the_pages_example() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
              "status": "READY_TO_COMPLETE",
              "destination_waba": {
                "id": "998877665544332",
                "name": "Acme WABA (INR)",
                "currency": "INR",
                "timezone_id": "1",
                "message_template_namespace": "1234abcd_namespace"
              },
              "id": "mig_01HZYK3ABCDEF4567890"
            }),
        );
        let state = client(&t, RetryPolicy::NONE)
            .payment_method_migration("mig_01HZYK3ABCDEF4567890")
            .status()
            .await
            .unwrap();
        assert_eq!(state.id, "mig_01HZYK3ABCDEF4567890");
        assert_eq!(state.status, PaymentMigrationStatus::ReadyToComplete);
        let clone = state.destination_waba.unwrap();
        assert_eq!(clone.id.as_str(), "998877665544332");
        assert_eq!(clone.name.as_deref(), Some("Acme WABA (INR)"));
        assert_eq!(clone.currency.as_deref(), Some("INR"));
        assert_eq!(clone.timezone_id.as_deref(), Some("1"));
        assert_eq!(
            clone.message_template_namespace.as_deref(),
            Some("1234abcd_namespace")
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/mig_01HZYK3ABCDEF4567890");
        assert_eq!(req.url.query(), None);
        assert_eq!(t.remaining(), 0);
    }

    /// The page's § Complete migration: a `POST` with no body; the answer
    /// in the shape of its response example.
    #[tokio::test]
    async fn resume_posts_to_resume_migration() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
              "status": "COMPLETED",
              "destination_waba": {"id": "998877665544332", "currency": "INR"},
              "id": "mig_01HZYK3ABCDEF4567890"
            }),
        );
        let state = client(&t, RetryPolicy::NONE)
            .payment_method_migration("mig_01HZYK3ABCDEF4567890")
            .resume_migration()
            .await
            .unwrap();
        assert_eq!(state.status, PaymentMigrationStatus::Completed);
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            "/v25.0/mig_01HZYK3ABCDEF4567890/resume_migration"
        );
        assert_eq!(req.body, RecordedBody::Empty);
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);

        // An id with `/` stays one segment; a blank one is refused.
        t.push_json(200, json!({"status": "IN_PROGRESS", "id": "a/b"}));
        client(&t, RetryPolicy::NONE)
            .payment_method_migration("a/b")
            .status()
            .await
            .unwrap();
        assert_eq!(t.last_request().unwrap().path(), "/v25.0/a%2Fb");
        let m = client(&t, RetryPolicy::NONE).payment_method_migration(" ");
        assert!(matches!(m.status().await, Err(Error::Validation(v)) if v.field == "migration_id"));
        assert!(
            matches!(m.resume_migration().await, Err(Error::Validation(v)) if v.field == "migration_id")
        );
        assert_eq!(t.requests().len(), 2);
    }

    /// Neither the intent (it creates a WABA) nor the resume (it switches
    /// billing) is replayed after a timeout; the status read is.
    #[tokio::test]
    async fn neither_write_is_replayed_after_a_timeout() {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        assert!(
            client(&t, retrying())
                .waba(WabaId::new("123456789012345").unwrap())
                .set_payment_method_migration_intent(&PaymentMethodMigrationIntent::new("INR"))
                .await
                .is_err()
        );
        assert_eq!(t.requests().len(), 1, "the intent is not replayed");

        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        let m = client(&t, retrying()).payment_method_migration("mig_1");
        assert!(m.resume_migration().await.is_err());
        assert_eq!(t.requests().len(), 1, "the resume is not replayed");

        t.push_error(|| TransportError::Timeout);
        t.push_json(200, json!({"status": "FAILED_TO_COMPLETE", "id": "mig_1"}));
        assert_eq!(
            m.status().await.unwrap().status,
            PaymentMigrationStatus::FailedToComplete
        );
        assert_eq!(t.requests().len(), 3, "a read is replayed");
        assert_eq!(t.remaining(), 0);
    }

    #[test]
    fn every_documented_status_parses_and_an_unknown_one_is_kept() {
        for value in [
            "INITIATED",
            "ACCEPTED",
            "IN_PROGRESS",
            "READY_TO_COMPLETE",
            "COMPLETED",
            "FAILED",
            "FAILED_TO_COMPLETE",
            "REJECTED",
        ] {
            let parsed: PaymentMigrationStatus = serde_json::from_value(json!(value)).unwrap();
            assert!(
                !matches!(parsed, PaymentMigrationStatus::Other(_)),
                "{value}"
            );
            assert_eq!(parsed.as_str(), value);
        }
        let other: PaymentMigrationStatus = serde_json::from_value(json!("PAUSED")).unwrap();
        assert_eq!(other, PaymentMigrationStatus::Other("PAUSED".into()));
    }
}
