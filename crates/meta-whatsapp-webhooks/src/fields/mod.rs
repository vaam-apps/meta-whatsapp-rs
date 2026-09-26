//! Typed `value`s, one module per webhook field family.
//!
//! Every type is also re-exported here, flat, so `fields::InboundMessage`
//! works as well as `fields::messages::InboundMessage`.
//!
//! Parsing rules shared by all of them:
//!
//! - Unknown properties are ignored, never rejected (no
//!   `deny_unknown_fields`).
//! - Enums are open (`Other(String)`), so a new Meta value parses.
//! - A property is required only when the docs show it on every example of
//!   that shape; conditional ones are `Option`/empty `Vec`.
//! - If a value still fails its typed parse, the change falls back to
//!   [`crate::ChangeValue::Unknown`] with the error recorded on
//!   [`crate::Change::parse_error`]; the rest of the delivery is unaffected.

pub mod account;
pub mod automatic_events;
pub mod calls;
pub mod coexistence;
pub mod common;
pub mod flows;
pub mod groups;
pub mod messages;
pub mod routing;
pub mod templates;
pub mod users;

pub use account::*;
pub use automatic_events::*;
pub use calls::*;
pub use coexistence::*;
pub use common::{Contact, Metadata, Profile};
pub use flows::*;
pub use groups::*;
pub use messages::*;
pub use routing::*;
pub use templates::*;
pub use users::*;
