//! WhatsApp Business Platform for Rust.
//!
//! Facade over the workspace crates; see the README for the map.

pub mod inbox;

pub use wa_adapters as adapters;
pub use wa_client as client;
pub use wa_core as core;
#[cfg(feature = "typst")]
pub use wa_typst as typst;
pub use wa_webhooks as webhooks;

pub use wa_client::{AppCredentials, Client, RetryPolicy};
pub use wa_core::{Error, ErrorKind, GraphApiError, Result};
