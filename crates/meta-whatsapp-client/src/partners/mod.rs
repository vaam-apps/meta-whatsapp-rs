//! Partner APIs: Multi-Partner Solutions ([`solutions`]) and WABA
//! migration intents ([`migration`]).
//!
//! Docs: `solution-providers/multi-partner-solutions`,
//! `solution-providers/multi-partner-solution-embedded-creation`,
//! `solution-providers/support/adding-waba-to-mps`,
//! `solution-providers/support/migrating-wabas-among-solutions-via-meta-business-suite`;
//! the references `reference/whatsapp-business-solution/*`,
//! `reference/application/solution-creation-api`,
//! `reference/application/application-solutions-api`,
//! `reference/application/application-connected-client-businesses`,
//! `reference/whatsapp-business-account/whatsapp-business-account-solutions-list-api`
//! and
//! `reference/whatsapp-business-account-migration-intent/migration-intent-details-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown).
//!
//! | Accessor | Meta paths |
//! | --- | --- |
//! | [`Client::partner_solutions`](crate::Client::partner_solutions) | `POST /{APP_ID}/whatsapp_business_solution`, `GET /{APP_ID}/whatsapp_business_solutions`, `GET /{APP_ID}/connected_client_businesses`, `GET /{WABA_ID}/solutions`, `GET /{SOLUTION_ID}`, `GET /{SOLUTION_ID}/partners`, `GET /{SOLUTION_ID}/access_token`, `POST /{SOLUTION_ID}/accept`, `…/reject`, `…/send_deactivation_request`, `…/accept_deactivation_request`, `…/reject_deactivation_request` |
//! | [`Client::migration_intents`](crate::Client::migration_intents) | `POST /{WABA_ID}/set_solution_migration_intent`, `GET /{MIGRATION_INTENT_ID}` |
//!
//! # Which token
//!
//! Every method authenticates with the token of the [`Client`](crate::Client)
//! it was built from; no method stores a second one. Which token Meta
//! expects is said on each method, from the pages: the Tech Provider's
//! user token for creating a solution from the embedded creation flow, a
//! system user token for accepting it and for reading a client's
//! business token, and the client's business token for migration
//! intents.
//!
//! # No legal act here
//!
//! None of these pages ties a call to accepting terms or signing an
//! agreement: accepting a solution grants the partners' apps the
//! permissions the solution names, and the contracts between partners are
//! "left to the discretion of you and your partner"
//! (`multi-partner-solutions`, step 1). Each state change is still its own
//! explicit call, made by no other method: [`solutions::PartnerSolutions::accept`]
//! cannot be undone (a solution cannot be declined once accepted), nor can
//! [`solutions::PartnerSolutions::reject`], and a migration intent the
//! client leaves unanswered for 90 days is accepted for them
//! (`adding-waba-to-mps`).

pub mod migration;
pub mod solutions;

#[cfg(test)]
mod tests;
