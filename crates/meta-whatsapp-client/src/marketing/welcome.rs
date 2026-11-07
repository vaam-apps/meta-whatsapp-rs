//! Click-to-WhatsApp welcome message sequences
//! (`/{WABA_ID}/welcome_message_sequences`): what a user sees on opening a
//! chat from a Click-to-WhatsApp ad that names the sequence.
//!
//! Source: `ctwa/welcome-message-sequences`.
//!
//! Where this client departs from the guide's examples:
//!
//! - **Create and update** are sent as `multipart/form-data`, as the
//!   guide's `curl -F` examples are: `name` and `sequence_id` as text
//!   (curl strips the double quotes around `-F 'name="…"'`), and
//!   `welcome_message_sequence` as its JSON text.
//! - **Get and delete** pass `sequence_id` as a query parameter: the guide
//!   writes `curl -X GET -F …` and `curl -X DELETE -F …`, a form body on a
//!   `GET` or `DELETE`, where Graph reads the query.
//! - **The list** is a bare JSON array in the guide, not Graph's usual
//!   `{"data": […]}`; both are read. The guide documents `limit` and no
//!   cursor, so there is no paging.
//!
//! The ad side (the sequence id in an ad creative's
//! `asset_feed_spec.additional_data.partner_app_welcome_message_flow_id`)
//! belongs to Meta's Marketing API, not to this client. The referral of a
//! chat started from an ad arrives with the message
//! (`meta_whatsapp_webhooks::fields::Referral`).
//!
//! Error codes from the guide (not classified into an
//! [`ErrorKind`](meta_whatsapp_core::ErrorKind) of their own: read
//! [`GraphApiError::code`](meta_whatsapp_core::GraphApiError)): `4027001`
//! invalid input, `4027005` create failed, `4027006` update failed,
//! `4027007` API not available yet, `4027010` missing parameter, `4027012`
//! sequence used in an active ad (cannot be updated or deleted), `4027017`
//! sequence not found or not accessible.

use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{WabaId, WelcomeSequenceId};
use meta_whatsapp_core::transport::Multipart;
use meta_whatsapp_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Client;

/// Welcome message sequences of one WABA. See [`Client::welcome_sequences`].
#[derive(Debug, Clone)]
pub struct WelcomeSequences {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`WelcomeSequences`] API for `waba_id`: create, update, list, read
    /// and delete Click-to-WhatsApp welcome message sequences (needs
    /// `whatsapp_business_management`).
    pub fn welcome_sequences(&self, waba_id: impl Into<WabaId>) -> WelcomeSequences {
        WelcomeSequences {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

/// The welcome message a sequence sends when a user clicks the ad
/// (`welcome_message_sequence`): a text, a message prefilled in the user's
/// composer, and quick replies (ice breakers). The guide states no limits
/// for them, so none is checked here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WelcomeMessage {
    /// The welcome text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The message prefilled for the user to send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autofill_message: Option<AutofillMessage>,
    /// Quick replies offered to the user.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice_breakers: Vec<IceBreaker>,
}

impl WelcomeMessage {
    /// An empty welcome message; add its parts with the setters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the welcome text.
    #[must_use]
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Set the message prefilled in the user's composer.
    #[must_use]
    pub fn autofill(mut self, content: impl Into<String>) -> Self {
        self.autofill_message = Some(AutofillMessage {
            content: content.into(),
        });
        self
    }

    /// Add a quick reply.
    #[must_use]
    pub fn ice_breaker(mut self, title: impl Into<String>) -> Self {
        self.ice_breakers.push(IceBreaker {
            title: title.into(),
        });
        self
    }
}

/// `autofill_message` of a [`WelcomeMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutofillMessage {
    /// The prefilled text.
    pub content: String,
}

/// One quick reply of a [`WelcomeMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceBreaker {
    /// The reply's title.
    pub title: String,
}

/// What [`WelcomeSequences::create`] answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WelcomeSequenceCreated {
    /// The new sequence's id, for the ad creative and later calls.
    pub sequence_id: WelcomeSequenceId,
}

/// The changes of [`WelcomeSequences::update`]: at least one of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WelcomeSequenceUpdate {
    /// A new name.
    pub name: Option<String>,
    /// A new welcome message.
    pub welcome_message_sequence: Option<WelcomeMessage>,
}

impl WelcomeSequenceUpdate {
    /// No change yet; add one with the setters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rename the sequence.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Replace the welcome message.
    #[must_use]
    pub fn message(mut self, message: WelcomeMessage) -> Self {
        self.welcome_message_sequence = Some(message);
        self
    }
}

/// A sequence as [`WelcomeSequences::list`] and [`WelcomeSequences::get`]
/// return it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WelcomeSequence {
    /// The sequence's id.
    pub sequence_id: WelcomeSequenceId,
    /// Its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The welcome message, as Meta returns it (the guide shows only a
    /// `<JSON_OBJECT>` placeholder); [`Self::message`] reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome_message_sequence: Option<Value>,
    /// Whether an ad uses it (then it can be neither updated nor deleted
    /// while the ad is active).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_used_in_ad: Option<bool>,
}

impl WelcomeSequence {
    /// The welcome message, typed: read from a JSON object, or from a
    /// string holding one. `None` when absent or in neither shape.
    pub fn message(&self) -> Option<WelcomeMessage> {
        match self.welcome_message_sequence.as_ref()? {
            Value::String(text) => serde_json::from_str(text).ok(),
            object @ Value::Object(_) => serde_json::from_value(object.clone()).ok(),
            _ => None,
        }
    }
}

/// Query of [`WelcomeSequences::list`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListWelcomeSequences {
    /// How many sequences to fetch (`limit`).
    pub limit: Option<u32>,
}

impl ListWelcomeSequences {
    /// Every sequence (Meta's default count).
    pub fn new() -> Self {
        Self::default()
    }

    /// At most `limit` sequences.
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// The list's two shapes: the guide's bare array, or Graph's `data`.
#[derive(Deserialize)]
#[serde(untagged)]
enum SequenceList {
    Bare(Vec<WelcomeSequence>),
    Data { data: Vec<WelcomeSequence> },
}

impl SequenceList {
    fn into_vec(self) -> Vec<WelcomeSequence> {
        match self {
            Self::Bare(list) | Self::Data { data: list } => list,
        }
    }
}

fn not_blank(value: &str, field: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(field, "must not be empty"));
    }
    Ok(())
}

fn message_json(message: &WelcomeMessage) -> Result<String> {
    serde_json::to_string(message).map_err(|e| {
        Error::from(ValidationError::new(
            "welcome_message_sequence",
            format!("not serializable: {e}"),
        ))
    })
}

impl WelcomeSequences {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Create a sequence (`POST /{WABA_ID}/welcome_message_sequences` with
    /// `name` and `welcome_message_sequence`).
    ///
    /// Not retried on timeouts: a replay would create a second sequence.
    pub async fn create(
        &self,
        name: &str,
        message: &WelcomeMessage,
    ) -> Result<WelcomeSequenceCreated> {
        not_blank(name, "name")?;
        let form = Multipart::new()
            .text("welcome_message_sequence", message_json(message)?)
            .text("name", name);
        self.client
            .post_at(&[self.waba_id.as_str(), "welcome_message_sequences"])
            .multipart(form)
            .context("create welcome message sequence response")
            .send()
            .await
    }

    /// Change a sequence's name or message (`POST` with `sequence_id`).
    /// Meta refuses a sequence an active ad uses (`4027012`).
    ///
    /// Replayed on transient errors: setting the same values twice leaves
    /// the same sequence.
    pub async fn update(
        &self,
        sequence_id: &WelcomeSequenceId,
        update: &WelcomeSequenceUpdate,
    ) -> Result<()> {
        not_blank(sequence_id.as_str(), "sequence_id")?;
        if update.name.is_none() && update.welcome_message_sequence.is_none() {
            return Err(ValidationError::new(
                "welcome_message_sequence",
                "an update needs a name or a welcome message",
            )
            .into());
        }
        let mut form = Multipart::new().text("sequence_id", sequence_id.as_str());
        if let Some(name) = &update.name {
            not_blank(name, "name")?;
            form = form.text("name", name.as_str());
        }
        if let Some(message) = &update.welcome_message_sequence {
            form = form.text("welcome_message_sequence", message_json(message)?);
        }
        self.client
            .post_at(&[self.waba_id.as_str(), "welcome_message_sequences"])
            .multipart(form)
            .idempotent(true)
            .context("update welcome message sequence response")
            .send_success()
            .await
    }

    /// The WABA's sequences (`GET /{WABA_ID}/welcome_message_sequences`).
    /// The guide documents `limit` and no paging.
    pub async fn list(&self, query: &ListWelcomeSequences) -> Result<Vec<WelcomeSequence>> {
        let list: SequenceList = self
            .client
            .get_at(&[self.waba_id.as_str(), "welcome_message_sequences"])
            .query_opt("limit", query.limit)
            .context("welcome message sequences response")
            .send()
            .await?;
        Ok(list.into_vec())
    }

    /// One sequence (`GET … ?sequence_id=…`); `None` when Meta lists none.
    pub async fn get(&self, sequence_id: &WelcomeSequenceId) -> Result<Option<WelcomeSequence>> {
        not_blank(sequence_id.as_str(), "sequence_id")?;
        let list: SequenceList = self
            .client
            .get_at(&[self.waba_id.as_str(), "welcome_message_sequences"])
            .query("sequence_id", sequence_id)
            .context("welcome message sequence response")
            .send()
            .await?;
        Ok(list.into_vec().into_iter().next())
    }

    /// Delete a sequence (`DELETE … ?sequence_id=…`). Meta refuses one an
    /// active ad uses (`4027012`). The id is required here although the
    /// guide marks it optional: a delete without it names no sequence.
    pub async fn delete(&self, sequence_id: &WelcomeSequenceId) -> Result<()> {
        not_blank(sequence_id.as_str(), "sequence_id")?;
        self.client
            .delete_at(&[self.waba_id.as_str(), "welcome_message_sequences"])
            .query("sequence_id", sequence_id)
            .context("delete welcome message sequence response")
            .send_success()
            .await
    }
}
