//! WABA migration intents: tag a client's WABA for a Multi-Partner
//! Solution, and read the intent back to learn whether the client
//! accepted.
//!
//! Docs: `solution-providers/support/adding-waba-to-mps` and
//! `solution-providers/support/migrating-wabas-among-solutions-via-meta-business-suite`
//! (the flow and the examples), with
//! `reference/whatsapp-business-account-migration-intent/migration-intent-details-api`
//! (the intent's fields and statuses).
//!
//! | Meta path | Method |
//! | --- | --- |
//! | `POST /{WABA_ID}/set_solution_migration_intent` | [`MigrationIntents::create`] |
//! | `GET /{MIGRATION_INTENT_ID}` | [`MigrationIntents::get`] |
//!
//! # The flow
//!
//! A Tech Provider (moving a client between solutions) or a Solution
//! Partner (adding a WABA it onboarded to an active solution) creates the
//! intent with the client's business token: Meta sends the client a
//! request in Meta Business Suite, which the client accepts there (no API
//! call accepts it). The Solution Partner of the destination solution then
//! reads the intent, also with the client's business token
//! ([`PartnerSolutions::access_token`](crate::partners::solutions::PartnerSolutions::access_token)
//! gives it), and goes on only when it is
//! [`ACCEPTED`](MigrationStatus::Accepted). A request the client leaves
//! unanswered for 90 days is accepted for them (`adding-waba-to-mps`).
//!
//! # Not wrapped
//!
//! An unindexed reference page (`reference/whatsapp-business-account/migration-intent-api`)
//! describes a different creation call, `POST /{WABA-ID}/migration_intent`,
//! with required `destination_waba_id`, `solution_id` and
//! `migration_reason`, and no example. The guides link
//! `set_solution_migration_intent` with an example instead, so that is the
//! one wrapped; the other is left until Meta's guides use it.

use meta_whatsapp_core::Result;
use meta_whatsapp_core::ids::{MigrationIntentId, SolutionId, WabaId};
use serde::{Deserialize, Serialize};

use crate::Client;
use crate::partners::solutions::SolutionInfo;
use crate::phone_numbers::fields_param;
use crate::templates::macros::string_enum;

/// Entry point, see [`Client::migration_intents`].
#[derive(Debug, Clone)]
pub struct MigrationIntents {
    client: Client,
}

impl Client {
    /// Migration intent operations, authenticated with this client's token
    /// (the client's business token, see the
    /// [module docs](crate::partners::migration)).
    pub fn migration_intents(&self) -> MigrationIntents {
        MigrationIntents {
            client: self.clone(),
        }
    }
}

string_enum! {
    /// A migration intent's `status`
    /// (`WhatsAppBusinessAccountMigrationStatus`). The guide: go on only
    /// at `ACCEPTED`.
    pub enum MigrationStatus {
        /// Created; the client has not answered.
        Initiated => "INITIATED",
        /// The client accepted: the destination partner may go on.
        Accepted => "ACCEPTED",
        /// The client rejected it.
        Rejected => "REJECTED",
        /// Ready to complete.
        ReadyToComplete => "READY_TO_COMPLETE",
        /// The migration is running.
        InProgress => "IN_PROGRESS",
        /// The migration finished.
        Completed => "COMPLETED",
        /// The migration failed.
        Failed => "FAILED",
        /// The migration failed to complete.
        FailedToComplete => "FAILED_TO_COMPLETE",
    }
}

/// Answer of `POST /{WABA_ID}/set_solution_migration_intent`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct MigrationIntentCreated {
    /// The intent: send it to the destination solution's Solution Partner,
    /// who reads it with [`MigrationIntents::get`].
    pub id: MigrationIntentId,
}

/// A WABA as a migration intent names it (`waba`, `destination_waba`).
/// The reference gives only `id`; the guide's example adds the rest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct MigrationWaba {
    /// The WABA id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<WabaId>,
    /// The WABA's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The WABA's currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// The WABA's timezone id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone_id: Option<String>,
    /// The WABA's business type (`ent` in the example).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_type: Option<String>,
    /// The WABA's message template namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template_namespace: Option<String>,
}

/// A migration intent (`GET /{MIGRATION_INTENT_ID}`). Meta's defaults are
/// `solution`, `status` and `destination_waba`; `waba` (the source) on
/// request.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct MigrationIntentInfo {
    /// The intent id.
    pub id: MigrationIntentId,
    /// Where the intent stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<MigrationStatus>,
    /// The destination solution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solution: Option<SolutionInfo>,
    /// The client's WABA the migration lands on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_waba: Option<MigrationWaba>,
    /// The source WABA (`fields=waba`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba: Option<MigrationWaba>,
}

impl MigrationIntentInfo {
    /// Whether the client accepted the intent: the one status at which
    /// the guide lets the destination partner go on.
    pub fn is_accepted(&self) -> bool {
        self.status == Some(MigrationStatus::Accepted)
    }
}

#[derive(Serialize)]
struct CreateBody<'a> {
    solution_id: &'a SolutionId,
}

impl MigrationIntents {
    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `POST /{WABA_ID}/set_solution_migration_intent`: tag `waba` for
    /// migration to `solution` (the destination, which must be active).
    /// Meta then asks the client, in Meta Business Suite, to accept.
    /// Token: the client's business token.
    ///
    /// Never replayed: a lost answer may have created the intent and sent
    /// the client its request.
    pub async fn create(
        &self,
        waba: &WabaId,
        solution: &SolutionId,
    ) -> Result<MigrationIntentCreated> {
        self.client
            .post_at(&[waba.as_str(), "set_solution_migration_intent"])
            .json(&CreateBody {
                solution_id: solution,
            })
            .idempotent(false)
            .context("set solution migration intent response")
            .send()
            .await
    }

    /// `GET /{MIGRATION_INTENT_ID}`: the intent's fields (`fields` empty =
    /// Meta's defaults; the reference lists `id`, `status`,
    /// `destination_waba`, `solution` and `waba`). Token: the client's
    /// business token.
    pub async fn get(
        &self,
        intent: &MigrationIntentId,
        fields: &[&str],
    ) -> Result<MigrationIntentInfo> {
        self.client
            .get_at(&[intent.as_str()])
            .query_opt("fields", fields_param(fields))
            .context("migration intent")
            .send()
            .await
    }
}
