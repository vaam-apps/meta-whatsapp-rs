//! The bot over HTTP (docs/design/server.md, section 4.2; roadmap M5k,
//! slice 2): declarative rules a tenant keeps for one of its numbers, which
//! the service evaluates on that number's inbound messages and answers
//! from, because the service cannot run a tenant's code.
//!
//! | Service | Operations |
//! | --- | --- |
//! | [`Bot`] | [`Bot::settings`], [`Bot::update_settings`] (on or off, the owners and the banned); [`Bot::list_rules`], [`Bot::create_rule`], [`Bot::get_rule`], [`Bot::replace_rule`], [`Bot::delete_rule`] |
//! | [`BotReplies`] | what the webhook pipeline hands each recorded inbound message to ([`crate::inbound::MessageActions`]): match, guard, answer |
//!
//! # What a rule is
//!
//! A [`RuleSpec`] has a [`Trigger`] (a command by name and prefix; a
//! keyword, whole or contained; a tapped button or list row by its id; or
//! anything), guards (a chat kind, owners only, a per-user cooldown) and an
//! [`Action`] (a text, a template, or the generated help). Rules are tried
//! in order of `priority` (then age); the first that matches decides: a
//! rule that matches but is refused by a guard ends the message too, and
//! `continue_after` lets the rules after a rule that answered go on (at
//! most [`MAX_REPLIES_PER_MESSAGE`] answers to one message). **No pattern
//! of a tenant's is ever compiled**: a trigger is literal text (D461), so
//! a message is matched in time linear in its length, and a rule has at
//! most [`MAX_WORDS`] words of at most [`MAX_WORD_CHARS`] characters.
//!
//! # How it is kept
//!
//! No port, no table (D460): one record per number in the bundle's
//! `KvStore` ([`CONFIG_NAMESPACE`]), naming its tenant, changed by
//! compare-and-swap. **A bot is its tenant's or it does not exist**
//! (M5.2): every operation takes the number from the caller's own tenant
//! (`404` otherwise), and a record that names another tenant than the
//! number's holder reads as an empty, switched-off bot (a number rebound
//! to another tenant brings none of the first's rules; the new holder's
//! first write replaces the record).
//!
//! # How it answers
//!
//! After the pipeline recorded an inbound message for the tenant holding
//! its number, [`BotReplies`] reads the number's record. Off, empty, or
//! of another tenant: nothing. A banned sender: nothing. A message older
//! than [`MAX_MESSAGE_AGE`] (an outage's backlog): nothing. Else the rules,
//! in order. Each answer is **claimed before it is sent**: a marker per
//! (number, message, rule) is created with `put_if_absent`
//! ([`MARKER_NAMESPACE`], [`MARKER_TTL`], longer than Meta's seven days of
//! retries), so a redelivered webhook, a second replica and a retry after a
//! failure never answer twice; the residual is an answer never sent (a
//! replica dying between the claim and the request), never a second one
//! (D462). The cooldown is the bot crate's `KvCooldowns` (`wa.bot.cooldown`,
//! per number, rule and user). The answer waits for a slot of the number's
//! pacer, the budget broadcasts share, at most [`MAX_PACE_WAIT`] (else it is
//! dropped, counted `limited`). It is sent as the inbox sends a reply
//! ([`crate::inbox::Conversations::reply`]): the 24-hour window, the
//! thread's owner and the number's binding epoch are checked before any
//! request, with the number's token found by the number's *current*
//! binding to the tenant that holds it (`Authorizer::bot_sender`), so a
//! number moved to another tenant never answers from the first's rules. A
//! free-form answer outside the window is not sent; a rule's
//! `outside_window_template`, when it names one, is sent instead.
//!
//! # What is logged
//!
//! Rule ids, outcomes and counts. Never a message, a reply text, a trigger
//! word or a person (target `meta_whatsapp_server::api::bot`).

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::bot::{CooldownKey, CooldownOutcome, Cooldowns, KvCooldowns, Pacer};
use meta_whatsapp_rs::core::clock::Clock;
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::core::recipient::Recipient;
use meta_whatsapp_rs::core::store::{ConversationStore, Expiry, JsonStore, KvStore, StoreKey};
use meta_whatsapp_rs::inbox::conversation_key;
use meta_whatsapp_rs::webhooks::WebhookEvent;
use meta_whatsapp_rs::webhooks::fields::common::Contact;
use meta_whatsapp_rs::webhooks::fields::messages::{
    InboundMessage, InteractiveReply, MessageContent as In,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::audit::{BotFacts, Subject, bot_audit};
use crate::authz::{Authorizer, Caller};
use crate::error::{ErrorCode, ServiceError, kind_code};
use crate::inbound::MessageActions;
use crate::inbox::{Conversations, ReplyChecksByNumber};
use crate::messages::{MessageType, ReplyMessage, TextObject, names_an_object, reply_message};
use crate::model::TenantId;

/// The log target of this module (the API module's, as the broadcasts').
const LOG_TARGET: &str = "meta_whatsapp_server::api::bot";

/// The `KvStore` namespace of a number's bot record: its tenant, switch,
/// lists and rules, keyed by the number's id. A stable identifier,
/// pending the owner's confirmation before the first release (D460).
pub const CONFIG_NAMESPACE: &str = "wa.server.bot";

/// The `KvStore` namespace of the markers that claim an answer, keyed by
/// the SHA-256 of the number, the message and the rule (D462). A stable
/// identifier, pending the owner's confirmation before the first release.
pub const MARKER_NAMESPACE: &str = "wa.server.bot.reply";

/// An answer's messages carry this, then the rule's id, as their
/// `biz_opaque_callback_data`: Meta echoes it in their status events, so a
/// tenant can tell the bot's answers apart.
pub const CALLBACK_PREFIX: &str = "wa-bot:";

/// The audit action of a rule created.
pub const RULE_CREATED: &str = "bot_rule_created";

/// The audit action of a rule replaced.
pub const RULE_UPDATED: &str = "bot_rule_updated";

/// The audit action of a rule deleted.
pub const RULE_DELETED: &str = "bot_rule_deleted";

/// The audit action of a number's bot switched or its lists set.
pub const SETTINGS_UPDATED: &str = "bot_settings_updated";

/// The default of [`BotSettings::max_message_age`]: a message older than
/// this when the pipeline records it is not answered, for an outage's
/// backlog, or a replay, is not a conversation (D462).
pub const MAX_MESSAGE_AGE: Duration = Duration::from_hours(1);

/// How long an answer's marker lives: past Meta's seven days of webhook
/// retries (`webhooks/create-webhook-endpoint`).
pub const MARKER_TTL: Duration = Duration::from_hours(8 * 24);

/// The longest an answer waits for a slot of the number's pacer: past it,
/// the answer is dropped (counted `limited`) rather than hold a webhook
/// delivery behind a broadcast.
pub const MAX_PACE_WAIT: Duration = Duration::from_secs(5);

/// The longest the bot holds one webhook delivery, all its answers
/// included: past it, the delivery goes on (an answer in flight was
/// claimed, so it is never repeated).
pub const BUDGET: Duration = Duration::from_secs(20);

/// The most answers one message gets (rules with `continue_after`).
pub const MAX_REPLIES_PER_MESSAGE: usize = 3;

/// The most words (names, keywords, ids) one trigger lists.
pub const MAX_WORDS: usize = 20;

/// The longest word of a trigger, in characters.
pub const MAX_WORD_CHARS: usize = 64;

/// The most prefixes a command trigger lists.
pub const MAX_PREFIXES: usize = 5;

/// The most owners, and the most banned, a number lists.
pub const MAX_LIST: usize = 200;

/// The longest cooldown a rule may ask: a day.
pub const MAX_COOLDOWN_SECONDS: u32 = 86_400;

/// The largest priority: rules are tried from 0 up.
pub const MAX_PRIORITY: u16 = 10_000;

/// A priority a rule has when it names none.
pub const DEFAULT_PRIORITY: u16 = 100;

/// Attempts at a compare-and-swap before giving up.
const CAS_ATTEMPTS: usize = 8;

// ─── The settings ────────────────────────────────────────────────────────

/// The deployment's bot settings (D463), conservative by default.
/// `WA_SERVER_BOT_MAX_RULES` in the binary. The answers' rate is the
/// broadcasts' (`WA_SERVER_BROADCAST_RATE`): one budget per number.
#[derive(Debug, Clone)]
pub struct BotSettings {
    /// The most rules one number's bot may have.
    pub max_rules: usize,
    /// A message older than this when the pipeline records it is not
    /// answered ([`MAX_MESSAGE_AGE`] by default): an outage's backlog is
    /// not a conversation.
    pub max_message_age: Duration,
}

impl BotSettings {
    /// The default most rules a number has.
    pub const DEFAULT_MAX_RULES: usize = 50;
}

impl Default for BotSettings {
    fn default() -> Self {
        Self {
            max_rules: Self::DEFAULT_MAX_RULES,
            max_message_age: MAX_MESSAGE_AGE,
        }
    }
}

// ─── The rule ────────────────────────────────────────────────────────────

/// How a keyword trigger compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeywordMode {
    /// The whole message is the word (ignoring case and the space around).
    Exact,
    /// The message contains the word (ignoring case).
    Contains,
}

/// What a rule listens for. Every word is literal text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    /// A text that starts with one of the prefixes and then one of the
    /// names (ignoring case): `/help`, `!menu`.
    Command {
        /// The command's names and aliases, lower case, without a prefix.
        names: Vec<String>,
        /// The prefixes (`/` when none are given).
        prefixes: Vec<String>,
    },
    /// A text that is, or contains, one of the words.
    Keyword {
        /// The words, lower case.
        words: Vec<String>,
        /// How they compare.
        mode: KeywordMode,
    },
    /// A tapped reply button or list row, or a template quick-reply
    /// button, by the id (or payload) it was sent with.
    Payload {
        /// The ids, as they were set (case matters).
        ids: Vec<String>,
    },
    /// Any message with something to answer: a text, a tap or a media.
    /// Never a reaction, an edit, a deletion or a system message.
    Any,
}

/// Which chats a rule answers in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Chat {
    /// One-to-one chats only.
    Private,
    /// Groups only.
    Group,
    /// Both.
    Any,
}

/// What a rule does when it fires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Answer with a text (at most 4,096 characters).
    Reply {
        /// The text.
        text: String,
        /// Meta's template object (`name`, `language`, `components`) sent
        /// instead when the 24-hour window is closed. None: nothing is
        /// sent outside the window.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outside_window_template: Option<Value>,
    },
    /// Answer with a template (templates need no open window).
    Template {
        /// Meta's template object.
        template: Value,
    },
    /// Answer with the list of the number's command rules that have a
    /// `description`, generated.
    Help,
}

/// A rule, without the id and date the service gives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleSpec {
    /// A label for the tenant's own use.
    pub name: Option<String>,
    /// What the rule is for; a command rule's is its line in the help.
    pub description: Option<String>,
    /// A rule switched off is kept and never tried.
    pub enabled: bool,
    /// The order rules are tried in, from 0 up ([`MAX_PRIORITY`]).
    pub priority: u16,
    /// What it listens for.
    pub trigger: Trigger,
    /// Which chats.
    pub chat: Chat,
    /// Only the number's owners may fire it.
    pub owners_only: bool,
    /// Seconds before the same user can fire it again.
    pub cooldown_seconds: Option<u32>,
    /// What it does.
    pub action: Action,
    /// Let the rules after it be tried when it answered.
    pub continue_after: bool,
}

/// A rule of a number's bot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    /// Its id: `br_` and 32 hex characters.
    pub id: String,
    /// When it was made.
    pub created_ms: i64,
    /// Its content.
    #[serde(flatten)]
    pub spec: RuleSpec,
}

fn word(value: &str, field: &str, lower: bool) -> Result<String, ServiceError> {
    let word = value.trim();
    let chars = word.chars().count();
    if chars == 0
        || chars > MAX_WORD_CHARS
        || word.chars().any(char::is_control)
        || (field != "trigger.words"
            && field != "trigger.ids"
            && word.chars().any(char::is_whitespace))
    {
        return Err(ServiceError::invalid(field));
    }
    Ok(if lower {
        word.to_lowercase()
    } else {
        word.to_owned()
    })
}

fn words(values: &[String], field: &str, lower: bool) -> Result<Vec<String>, ServiceError> {
    if values.is_empty() || values.len() > MAX_WORDS {
        return Err(ServiceError::invalid(field));
    }
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let checked = word(value, field, lower)?;
        if !out.contains(&checked) {
            out.push(checked);
        }
    }
    Ok(out)
}

fn label(value: Option<String>, field: &str, max: usize) -> Result<Option<String>, ServiceError> {
    match value {
        None => Ok(None),
        Some(text) => {
            let text = text.trim().to_owned();
            if text.is_empty() || text.chars().count() > max || text.chars().any(char::is_control) {
                Err(ServiceError::invalid(field))
            } else {
                Ok(Some(text))
            }
        }
    }
}

/// A recipient to build a message for, only to check its content as a send
/// does: it is never sent to.
fn probe() -> Recipient {
    Recipient::phone("+16505551234")
}

fn text_message(text: &str) -> ReplyMessage {
    let mut request = ReplyMessage::new(MessageType::Text);
    request.text = Some(TextObject {
        body: text.to_owned(),
        preview_url: None,
    });
    request
}

fn template_message(template: &Value) -> ReplyMessage {
    let mut request = ReplyMessage::new(MessageType::Template);
    request.template = Some(template.clone());
    request
}

/// A template, checked as a send checks it (and not naming a catalog).
fn check_template(template: &Value, field: &str) -> Result<(), ServiceError> {
    let built = reply_message(template_message(template), probe())
        .map_err(|_| ServiceError::invalid(field))?;
    if names_an_object(&built) {
        return Err(ServiceError::invalid(field));
    }
    Ok(())
}

impl RuleSpec {
    /// The rule `self` says, checked and normalised (words lower case,
    /// blanks refused, text and templates checked as a send checks them).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on the field at fault (`name`, `description`,
    /// `priority`, `trigger…`, `cooldown_seconds`, `action…`), never
    /// quoting it.
    pub fn checked(mut self) -> Result<Self, ServiceError> {
        self.name = label(self.name, "name", 64)?;
        self.description = label(self.description, "description", 200)?;
        if self.priority > MAX_PRIORITY {
            return Err(ServiceError::invalid("priority"));
        }
        if self.cooldown_seconds == Some(0) || self.cooldown_seconds > Some(MAX_COOLDOWN_SECONDS) {
            return Err(ServiceError::invalid("cooldown_seconds"));
        }
        self.trigger = match self.trigger {
            Trigger::Command { names, prefixes } => {
                let names = words(&names, "trigger.names", true)?;
                let prefixes = if prefixes.is_empty() {
                    vec!["/".to_owned()]
                } else {
                    prefixes
                };
                let mut checked = Vec::new();
                for prefix in &prefixes {
                    let valid = (1..=2).contains(&prefix.chars().count())
                        && prefix
                            .chars()
                            .all(|c| !c.is_alphanumeric() && !c.is_whitespace() && !c.is_control());
                    if !valid || checked.len() >= MAX_PREFIXES {
                        return Err(ServiceError::invalid("trigger.prefixes"));
                    }
                    if !checked.contains(prefix) {
                        checked.push(prefix.clone());
                    }
                }
                Trigger::Command {
                    names,
                    prefixes: checked,
                }
            }
            Trigger::Keyword { words: w, mode } => Trigger::Keyword {
                words: words(&w, "trigger.words", true)?,
                mode,
            },
            Trigger::Payload { ids } => Trigger::Payload {
                ids: words(&ids, "trigger.ids", false)?,
            },
            Trigger::Any => Trigger::Any,
        };
        match &self.action {
            Action::Reply {
                text,
                outside_window_template,
            } => {
                reply_message(text_message(text), probe())
                    .map_err(|_| ServiceError::invalid("action.text"))?;
                if text.trim().is_empty() {
                    return Err(ServiceError::invalid("action.text"));
                }
                if let Some(template) = outside_window_template {
                    check_template(template, "action.outside_window_template")?;
                }
            }
            Action::Template { template } => check_template(template, "action.template")?,
            Action::Help => {
                if !matches!(
                    self.trigger,
                    Trigger::Command { .. } | Trigger::Keyword { .. }
                ) {
                    // Help answers a request for it, not every message.
                    return Err(ServiceError::invalid("action"));
                }
            }
        }
        Ok(self)
    }
}

/// A rule's id: `br_` and 32 hex characters (128 random bits).
fn new_rule_id() -> Result<String, ServiceError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ServiceError::internal())?;
    Ok(format!("br_{}", hex::encode(bytes)))
}

/// Whether `id` has the shape of [`new_rule_id`]'s.
fn is_rule_id(id: &str) -> bool {
    id.strip_prefix("br_")
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn millis(at: OffsetDateTime) -> i64 {
    i64::try_from(at.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}

// ─── Matching ────────────────────────────────────────────────────────────

/// What a message offers to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input<'a> {
    Text(&'a str),
    Payload(&'a str),
    /// A media, a location, contacts, an order: answerable by `any` only.
    Other,
}

fn input_of(content: &In) -> Option<Input<'_>> {
    Some(match content {
        In::Text(text) => Input::Text(&text.body),
        In::Button(button) => Input::Payload(button.payload.as_deref()?),
        In::Interactive(InteractiveReply::ButtonReply(reply)) => Input::Payload(&reply.id),
        In::Interactive(InteractiveReply::ListReply(reply)) => Input::Payload(&reply.id),
        In::Image(_)
        | In::Audio(_)
        | In::Video(_)
        | In::Document(_)
        | In::Sticker(_)
        | In::Location(_)
        | In::Contacts(_)
        | In::Order(_) => Input::Other,
        _ => return None,
    })
}

impl Trigger {
    fn matches(&self, input: Input<'_>) -> bool {
        match (self, input) {
            (Self::Any, _) => true,
            (Self::Command { names, prefixes }, Input::Text(text)) => {
                let text = text.trim_start();
                prefixes.iter().any(|prefix| {
                    text.strip_prefix(prefix.as_str()).is_some_and(|rest| {
                        let first = rest.split_whitespace().next().unwrap_or("");
                        !first.is_empty() && names.contains(&first.to_lowercase())
                    })
                })
            }
            (Self::Keyword { words, mode }, Input::Text(text)) => {
                let text = text.trim().to_lowercase();
                match mode {
                    KeywordMode::Exact => words.contains(&text),
                    KeywordMode::Contains => words.iter().any(|w| text.contains(w.as_str())),
                }
            }
            (Self::Payload { ids }, Input::Payload(id)) => ids.iter().any(|known| known == id),
            _ => false,
        }
    }
}

/// The help text: a line per enabled command rule that has a description.
fn help_text(rules: &[Rule]) -> String {
    let mut lines = Vec::new();
    for rule in rules.iter().filter(|rule| rule.spec.enabled) {
        if let (Trigger::Command { names, prefixes }, Some(description)) =
            (&rule.spec.trigger, &rule.spec.description)
            && let (Some(name), Some(prefix)) = (names.first(), prefixes.first())
        {
            lines.push(format!("{prefix}{name} - {description}"));
        }
    }
    if lines.is_empty() {
        "No commands are available.".to_owned()
    } else {
        let mut text = lines.join("\n");
        // Whole characters, within the text limit.
        while text.chars().count() > 4096 {
            text.pop();
        }
        text
    }
}

// ─── The record ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    v: u8,
    tenant: String,
    number: String,
    enabled: bool,
    owners: Vec<String>,
    banned: Vec<String>,
    rules: Vec<Rule>,
}

impl Config {
    fn empty(tenant: &TenantId, number: &PhoneNumberId) -> Self {
        Self {
            v: 1,
            tenant: tenant.as_str().to_owned(),
            number: number.as_str().to_owned(),
            enabled: false,
            owners: Vec::new(),
            banned: Vec::new(),
            rules: Vec::new(),
        }
    }

    fn sort(&mut self) {
        self.rules.sort_by(|a, b| {
            (a.spec.priority, a.created_ms, &a.id).cmp(&(b.spec.priority, b.created_ms, &b.id))
        });
    }
}

/// An owner or banned entry as the lists keep it: a BSUID, or a phone
/// number or `wa_id` without its `+`.
fn identity(value: &str) -> Result<String, ServiceError> {
    let id = value.trim().trim_start_matches('+');
    if id.is_empty() || id.len() > 128 || id.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ServiceError::invalid("owners"));
    }
    Ok(id.to_owned())
}

fn identities(list: &[String], field: &'static str) -> Result<Vec<String>, ServiceError> {
    if list.len() > MAX_LIST {
        return Err(ServiceError::invalid(field));
    }
    let mut out = BTreeSet::new();
    for entry in list {
        out.insert(identity(entry).map_err(|_| ServiceError::invalid(field))?);
    }
    Ok(out.into_iter().collect())
}

/// The bots' typed store on the bundle's `KvStore` (no port, no table),
/// shared by the operations and the replies. Cheap to clone.
#[derive(Clone)]
pub struct BotRecords {
    configs: JsonStore<Config>,
    kv: Arc<dyn KvStore>,
    clock: Arc<dyn Clock>,
    settings: BotSettings,
}

impl fmt::Debug for BotRecords {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotRecords")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl BotRecords {
    /// The records in `kv`, dated by `clock`.
    pub fn new(kv: Arc<dyn KvStore>, clock: Arc<dyn Clock>, settings: BotSettings) -> Self {
        Self {
            configs: JsonStore::new(kv.clone(), CONFIG_NAMESPACE),
            kv,
            clock,
            settings,
        }
    }

    /// The settings it was made with.
    pub fn settings(&self) -> &BotSettings {
        &self.settings
    }

    /// `tenant`'s bot on `number`, and the version of the record it was
    /// read from. A record of another tenant reads as an empty bot (the
    /// number was rebound), to be replaced by the first write.
    async fn mine(
        &self,
        tenant: &TenantId,
        number: &PhoneNumberId,
    ) -> Result<(Config, Option<u64>), StorageError> {
        Ok(match self.configs.get(number.as_str()).await? {
            Some((config, version, _)) if config.tenant == tenant.as_str() => {
                (config, Some(version))
            }
            Some((_, version, _)) => (Config::empty(tenant, number), Some(version)),
            None => (Config::empty(tenant, number), None),
        })
    }

    /// Change `tenant`'s bot on `number` by `change`, by compare-and-swap.
    async fn update<F>(
        &self,
        tenant: &TenantId,
        number: &PhoneNumberId,
        mut change: F,
    ) -> Result<Config, ServiceError>
    where
        F: FnMut(&mut Config) -> Result<(), ServiceError>,
    {
        for _ in 0..CAS_ATTEMPTS {
            let (mut config, version) = self.mine(tenant, number).await?;
            change(&mut config)?;
            config.sort();
            let written = match version {
                Some(version) => {
                    self.configs
                        .compare_and_swap(number.as_str(), version, Some(&config), Expiry::Never)
                        .await?
                }
                None => {
                    self.configs
                        .put_if_absent(number.as_str(), &config, Expiry::Never)
                        .await?
                }
            };
            if written.is_some() {
                return Ok(config);
            }
        }
        Err(ServiceError::new(ErrorCode::STORAGE_UNAVAILABLE).retryable(true))
    }
}

// ─── The answers of the operations ───────────────────────────────────────

/// A number's bot: whether it answers, and its lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotView {
    /// Whether the bot answers inbound messages.
    pub enabled: bool,
    /// The owners: they pass `owners_only` rules.
    pub owners: Vec<String>,
    /// The banned: their messages get no answer.
    pub banned: Vec<String>,
    /// How many rules it has.
    pub rules: usize,
    /// The most rules a number may have.
    pub max_rules: usize,
}

// ─── The service ─────────────────────────────────────────────────────────

/// The bot operations for `caller`, whose capabilities `authz` admits.
pub struct Bot<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    records: &'a BotRecords,
}

impl fmt::Debug for Bot<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bot").finish_non_exhaustive()
    }
}

impl<'a> Bot<'a> {
    /// The operations for `caller`.
    ///
    /// # Errors
    ///
    /// `caller` another [`Authorizer`]'s: `403 forbidden`.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        records: &'a BotRecords,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        Ok(Self {
            authz,
            caller,
            records,
        })
    }

    /// Step 4: the number is the caller's tenant's, else it does not exist.
    async fn number(&self, pn: &PhoneNumberId) -> Result<(), ServiceError> {
        self.authz.tenant_number(self.caller, pn).await
    }

    /// Step 4 alone: an adapter asks it before it reads a request's body,
    /// so that another tenant's number is `404` whatever the body holds.
    ///
    /// # Errors
    ///
    /// `404 not_found` for a number that is not the caller's tenant's;
    /// `503 storage_unavailable`.
    pub async fn admits(&self, pn: &PhoneNumberId) -> Result<(), ServiceError> {
        self.number(pn).await
    }

    fn view(&self, config: &Config) -> BotView {
        BotView {
            enabled: config.enabled,
            owners: config.owners.clone(),
            banned: config.banned.clone(),
            rules: config.rules.len(),
            max_rules: self.records.settings.max_rules,
        }
    }

    /// The number's bot.
    ///
    /// # Errors
    ///
    /// `404 not_found` for a number that is not the caller's tenant's;
    /// `503 storage_unavailable`.
    pub async fn settings(&self, pn: &PhoneNumberId) -> Result<BotView, ServiceError> {
        self.number(pn).await?;
        let (config, _) = self.records.mine(self.caller.tenant(), pn).await?;
        Ok(self.view(&config))
    }

    /// Switch the number's bot and set its lists (all three replaced).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `owners` or `banned` (too many, or an
    /// entry that is blank or holds a space); `404`, `503` as
    /// [`Self::settings`].
    pub async fn update_settings(
        &self,
        pn: &PhoneNumberId,
        enabled: bool,
        owners: &[String],
        banned: &[String],
    ) -> Result<BotView, ServiceError> {
        self.number(pn).await?;
        let owners = identities(owners, "owners")?;
        let banned = identities(banned, "banned")?;
        let config = self
            .records
            .update(self.caller.tenant(), pn, |config| {
                config.enabled = enabled;
                config.owners.clone_from(&owners);
                config.banned.clone_from(&banned);
                Ok(())
            })
            .await?;
        self.audit(SETTINGS_UPDATED, pn, None, &config).await;
        Ok(self.view(&config))
    }

    /// The number's rules, in the order they are tried.
    ///
    /// # Errors
    ///
    /// As [`Self::settings`].
    pub async fn list_rules(&self, pn: &PhoneNumberId) -> Result<Vec<Rule>, ServiceError> {
        self.number(pn).await?;
        let (config, _) = self.records.mine(self.caller.tenant(), pn).await?;
        Ok(config.rules)
    }

    /// One rule.
    ///
    /// # Errors
    ///
    /// `404 not_found` for a number or a rule that is not the caller's
    /// tenant's, or none at all (one answer); `503`.
    pub async fn get_rule(&self, pn: &PhoneNumberId, id: &str) -> Result<Rule, ServiceError> {
        self.number(pn).await?;
        let (config, _) = self.records.mine(self.caller.tenant(), pn).await?;
        config
            .rules
            .into_iter()
            .find(|rule| rule.id == id)
            .ok_or_else(ServiceError::not_found)
    }

    /// Add a rule to the number's bot.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on the rule's field at fault, or on `rules`
    /// at [`BotSettings::max_rules`]; `404`, `503` as [`Self::settings`].
    pub async fn create_rule(
        &self,
        pn: &PhoneNumberId,
        spec: RuleSpec,
    ) -> Result<Rule, ServiceError> {
        self.number(pn).await?;
        let spec = spec.checked()?;
        let rule = Rule {
            id: new_rule_id()?,
            created_ms: millis(self.records.clock.now()),
            spec,
        };
        let max = self.records.settings.max_rules;
        let config = self
            .records
            .update(self.caller.tenant(), pn, |config| {
                if config.rules.len() >= max {
                    return Err(ServiceError::invalid("rules"));
                }
                config.rules.push(rule.clone());
                Ok(())
            })
            .await?;
        self.audit(RULE_CREATED, pn, Some(&rule.id), &config).await;
        Ok(rule)
    }

    /// Replace the rule `id` (its id and date stay).
    ///
    /// # Errors
    ///
    /// As [`Self::get_rule`], and `422` on the rule's field at fault.
    pub async fn replace_rule(
        &self,
        pn: &PhoneNumberId,
        id: &str,
        spec: RuleSpec,
    ) -> Result<Rule, ServiceError> {
        self.number(pn).await?;
        if !is_rule_id(id) {
            return Err(ServiceError::not_found());
        }
        let spec = spec.checked()?;
        let mut replaced = None;
        let config = self
            .records
            .update(self.caller.tenant(), pn, |config| {
                let rule = config
                    .rules
                    .iter_mut()
                    .find(|rule| rule.id == id)
                    .ok_or_else(ServiceError::not_found)?;
                rule.spec = spec.clone();
                replaced = Some(rule.clone());
                Ok(())
            })
            .await?;
        let rule = replaced.ok_or_else(ServiceError::not_found)?;
        self.audit(RULE_UPDATED, pn, Some(&rule.id), &config).await;
        Ok(rule)
    }

    /// Remove the rule `id`.
    ///
    /// # Errors
    ///
    /// As [`Self::get_rule`].
    pub async fn delete_rule(&self, pn: &PhoneNumberId, id: &str) -> Result<(), ServiceError> {
        self.number(pn).await?;
        if !is_rule_id(id) {
            return Err(ServiceError::not_found());
        }
        let config = self
            .records
            .update(self.caller.tenant(), pn, |config| {
                let before = config.rules.len();
                config.rules.retain(|rule| rule.id != id);
                if config.rules.len() == before {
                    return Err(ServiceError::not_found());
                }
                Ok(())
            })
            .await?;
        self.audit(RULE_DELETED, pn, Some(id), &config).await;
        Ok(())
    }

    async fn audit(
        &self,
        action: &'static str,
        pn: &PhoneNumberId,
        rule_id: Option<&str>,
        config: &Config,
    ) {
        let waba = match self.authz.records().number(pn).await {
            Ok(Some(binding)) => Some(binding.waba_id.as_str().to_owned()),
            _ => None,
        };
        bot_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: waba.as_deref(),
                phone_number_id: Some(pn.as_str()),
                ..Subject::default()
            },
            &BotFacts {
                rule_id,
                rules: config.rules.len(),
                enabled: config.enabled,
            },
        );
    }
}

// ─── The answers to inbound messages ─────────────────────────────────────

/// What became of a rule that fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Sent,
    Duplicate,
    CoolingDown,
    Limited,
    WindowClosed,
    ThreadOwned,
    Unusable,
    Failed(&'static str),
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Duplicate => "duplicate",
            Self::CoolingDown => "cooling_down",
            Self::Limited => "limited",
            Self::WindowClosed => "window_closed",
            Self::ThreadOwned => "thread_owned_elsewhere",
            Self::Unusable => "number_unusable",
            Self::Failed(_) => "failed",
        }
    }
}

/// The bot's replies to inbound messages: what the webhook pipeline hands
/// each recorded message to ([`MessageActions`]). Built on the bundle's
/// stores, the authorizer (the token of the tenant holding the number) and
/// the pacer broadcasts share.
pub struct BotReplies {
    records: BotRecords,
    authz: Arc<Authorizer>,
    conversations: Arc<dyn ConversationStore>,
    checks: ReplyChecksByNumber,
    names_account: bool,
    pacer: Pacer,
    cooldowns: KvCooldowns,
}

impl fmt::Debug for BotReplies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotReplies").finish_non_exhaustive()
    }
}

/// Who sent a message: every way the event names them, BSUID first.
fn sender_ids(message: &InboundMessage, contact: Option<&Contact>) -> Vec<String> {
    let mut ids = Vec::new();
    let mut push = |id: String| {
        let id = id.trim_start_matches('+').to_owned();
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    };
    if let Some(user) = message
        .from_user_id
        .as_ref()
        .or_else(|| contact.and_then(|c| c.user_id.as_ref()))
    {
        push(user.to_string());
    }
    if let Some(parent) = &message.from_parent_user_id {
        push(parent.to_string());
    }
    if let Some(wa_id) = message
        .from
        .as_ref()
        .or_else(|| contact.and_then(|c| c.wa_id.as_ref()))
    {
        push(wa_id.to_string());
    }
    ids
}

fn marker_key(number: &PhoneNumberId, message: &str, rule: &str) -> StoreKey {
    let mut hasher = Sha256::new();
    for part in [number.as_str(), message, rule] {
        hasher.update(format!("{}:", part.len()));
        hasher.update(part);
        hasher.update(",");
    }
    StoreKey::new(MARKER_NAMESPACE, hex::encode(hasher.finalize()))
}

impl BotReplies {
    /// Replies on `records`, sending as `authz` admits, recording into
    /// `conversations` (the bundle's), with each number's reply `checks`,
    /// paced by `pacer`; `names_account` is
    /// `WA_SERVER_SEND_MESSAGING_ACCOUNT_ID`.
    pub fn new(
        records: BotRecords,
        authz: Arc<Authorizer>,
        conversations: Arc<dyn ConversationStore>,
        checks: ReplyChecksByNumber,
        names_account: bool,
        pacer: Pacer,
    ) -> Self {
        let cooldowns = KvCooldowns::new(records.kv.clone(), records.clock.clone());
        Self {
            records,
            authz,
            conversations,
            checks,
            names_account,
            pacer,
            cooldowns,
        }
    }

    #[allow(clippy::too_many_lines)] // one pass over the rules, top to bottom
    async fn handle(&self, tenant: &TenantId, event: &WebhookEvent) -> Result<(), Error> {
        let WebhookEvent::MessageReceived {
            phone_number_id,
            contact,
            message,
            ..
        } = event
        else {
            return Ok(());
        };
        if self.records.clock.now() - message.timestamp
            > time::Duration::try_from(self.records.settings.max_message_age)
                .unwrap_or(time::Duration::HOUR)
        {
            return Ok(());
        }
        let config = match self.records.configs.get(phone_number_id.as_str()).await {
            Ok(Some((config, _, _))) => config,
            Ok(None) => return Ok(()),
            Err(StorageError::Corrupt { .. }) => {
                tracing::warn!(target: LOG_TARGET, "a number's bot record is unreadable: no answer");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        // M5.2: the rules answer only for the tenant that wrote them and
        // that holds the number now.
        if config.tenant != tenant.as_str() || !config.enabled || config.rules.is_empty() {
            return Ok(());
        }
        let ids = sender_ids(message, contact.as_ref());
        if ids.iter().any(|id| config.banned.contains(id)) {
            tracing::debug!(target: LOG_TARGET, "a banned sender's message: no answer");
            return Ok(());
        }
        let Some(input) = input_of(&message.content) else {
            return Ok(());
        };
        let Some(key) = conversation_key(phone_number_id, contact.as_ref(), message) else {
            return Ok(());
        };
        let owner = ids.iter().any(|id| config.owners.contains(id));
        let group = message.group_id.is_some();
        let mut answered = 0;
        for rule in config.rules.iter().filter(|rule| rule.spec.enabled) {
            if !rule.spec.trigger.matches(input) {
                continue;
            }
            let chat_ok = match rule.spec.chat {
                Chat::Private => !group,
                Chat::Group => group,
                Chat::Any => true,
            };
            if !chat_ok || (rule.spec.owners_only && !owner) {
                tracing::debug!(target: LOG_TARGET, rule_id = rule.id, "a rule fired but its guard refused");
                break;
            }
            let outcome = self
                .answer(
                    tenant,
                    &config,
                    rule,
                    phone_number_id,
                    message.id.as_str(),
                    &key.contact,
                    &ids,
                )
                .await?;
            if let Outcome::Failed(code) = outcome {
                tracing::warn!(
                    target: LOG_TARGET,
                    rule_id = rule.id,
                    outcome = outcome.as_str(),
                    code,
                    "a bot answer failed"
                );
            } else {
                tracing::info!(
                    target: LOG_TARGET,
                    rule_id = rule.id,
                    outcome = outcome.as_str(),
                    "a bot rule fired"
                );
            }
            if outcome != Outcome::Sent && outcome != Outcome::Duplicate {
                break;
            }
            answered += 1;
            if !rule.spec.continue_after || answered >= MAX_REPLIES_PER_MESSAGE {
                break;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn answer(
        &self,
        tenant: &TenantId,
        config: &Config,
        rule: &Rule,
        pn: &PhoneNumberId,
        message_id: &str,
        contact: &str,
        ids: &[String],
    ) -> Result<Outcome, Error> {
        // The claim, before anything else is spent: a message is answered
        // by a rule once (D462).
        let claimed = self
            .records
            .kv
            .put_if_absent(
                &marker_key(pn, message_id, &rule.id),
                b"1".to_vec(),
                Expiry::After(MARKER_TTL),
            )
            .await?
            .is_some();
        if !claimed {
            return Ok(Outcome::Duplicate);
        }
        if let Some(seconds) = rule.spec.cooldown_seconds {
            let user = ids.first().map_or(contact, String::as_str);
            let key = CooldownKey::new(pn, &rule.id, user);
            match self
                .cooldowns
                .try_start(&key, Duration::from_secs(u64::from(seconds)))
                .await
            {
                Ok(CooldownOutcome::Started) => {}
                Ok(_) => return Ok(Outcome::CoolingDown),
                // Past the claim nothing is retried: the answer is lost, and
                // the delivery goes on.
                Err(error) => return Ok(Outcome::Failed(kind_code(error.kind()))),
            }
        }
        let (text, template, fallback) = match &rule.spec.action {
            Action::Reply {
                text,
                outside_window_template,
            } => (Some(text.clone()), None, outside_window_template.clone()),
            Action::Template { template } => (None, Some(template.clone()), None),
            Action::Help => (Some(help_text(&config.rules)), None, None),
        };
        // A slot of the number's budget, which broadcasts share.
        if !matches!(
            tokio::time::timeout(MAX_PACE_WAIT, self.pacer.acquire(pn)).await,
            Ok(Ok(()))
        ) {
            return Ok(Outcome::Limited);
        }
        let owned = match self.authz.bot_sender(tenant, pn.clone()).await {
            Ok(owned) => owned,
            Err(refused) => {
                tracing::warn!(
                    target: LOG_TARGET,
                    code = refused.code(),
                    "a bot's number cannot send: nothing sent"
                );
                return Ok(Outcome::Unusable);
            }
        };
        let Ok(conversations) = Conversations::new(
            &self.authz,
            &owned,
            self.conversations.clone(),
            self.checks.of(pn),
            self.records.clock.clone(),
        ) else {
            return Ok(Outcome::Unusable);
        };
        let conversations = conversations
            .naming_the_account(self.names_account)
            .withholding_texts();
        let callback = format!("{CALLBACK_PREFIX}{}", rule.id);
        let with_callback = |mut request: ReplyMessage| {
            request.callback_data = Some(callback.clone());
            request
        };
        let first = match (&text, &template) {
            (Some(text), _) => with_callback(text_message(text)),
            (None, Some(template)) => with_callback(template_message(template)),
            (None, None) => return Ok(Outcome::Unusable),
        };
        Ok(match conversations.reply(contact, first).await {
            Ok(_) => Outcome::Sent,
            Err(error) if error.error_code() == ErrorCode::CUSTOMER_SERVICE_WINDOW_CLOSED => {
                match fallback {
                    Some(template) => {
                        match conversations
                            .reply(contact, with_callback(template_message(&template)))
                            .await
                        {
                            Ok(_) => Outcome::Sent,
                            Err(error) => Outcome::Failed(error.code()),
                        }
                    }
                    None => Outcome::WindowClosed,
                }
            }
            Err(error) if error.error_code() == ErrorCode::THREAD_OWNED_ELSEWHERE => {
                Outcome::ThreadOwned
            }
            Err(error) => Outcome::Failed(error.code()),
        })
    }
}

#[async_trait]
impl MessageActions for BotReplies {
    async fn on_message(&self, tenant: &TenantId, event: &WebhookEvent) -> Result<(), Error> {
        if let Ok(result) = tokio::time::timeout(BUDGET, self.handle(tenant, event)).await {
            result
        } else {
            tracing::warn!(
                target: LOG_TARGET,
                "a bot took too long: the delivery goes on (an answer in flight is never repeated)"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a panic is the report
mod tests {
    use super::*;

    fn spec(trigger: Trigger) -> RuleSpec {
        RuleSpec {
            name: None,
            description: None,
            enabled: true,
            priority: DEFAULT_PRIORITY,
            trigger,
            chat: Chat::Private,
            owners_only: false,
            cooldown_seconds: None,
            action: Action::Reply {
                text: "hello".to_owned(),
                outside_window_template: None,
            },
            continue_after: false,
        }
    }

    fn command(names: &[&str]) -> Trigger {
        Trigger::Command {
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            prefixes: Vec::new(),
        }
    }

    /// The names and ids are stable identifiers (design D460, D462): data
    /// and the messages' callback data carry them. Decisive: each constant
    /// and the id's shape.
    #[test]
    fn the_namespaces_and_ids_are_pinned() {
        assert_eq!(CONFIG_NAMESPACE, "wa.server.bot");
        assert_eq!(MARKER_NAMESPACE, "wa.server.bot.reply");
        assert_eq!(CALLBACK_PREFIX, "wa-bot:");
        let id = new_rule_id().unwrap();
        assert!(is_rule_id(&id), "{id}");
        assert_eq!(id.len(), 35);
        for bad in ["", "br_", "br_xyz", "bc_00000000000000000000000000000001"] {
            assert!(!is_rule_id(bad), "{bad}");
        }
        assert_eq!(BotSettings::default().max_rules, 50);
    }

    /// A command matches by prefix and name, ignoring case, and by alias;
    /// a name with trailing words matches, a longer name does not.
    #[test]
    fn a_command_matches_by_prefix_and_name() {
        let checked = spec(command(&["Help", "?"])).checked().unwrap();
        let Trigger::Command { names, prefixes } = &checked.trigger else {
            panic!("a command");
        };
        assert_eq!(names, &["help", "?"]);
        assert_eq!(prefixes, &["/"]);
        let t = &checked.trigger;
        assert!(t.matches(Input::Text("/help")));
        assert!(t.matches(Input::Text("  /HELP me please")));
        assert!(t.matches(Input::Text("/?")));
        assert!(!t.matches(Input::Text("/helpme")));
        assert!(!t.matches(Input::Text("help")));
        assert!(!t.matches(Input::Text("!help")));
        assert!(!t.matches(Input::Text("/")));
        assert!(!t.matches(Input::Payload("/help")));
    }

    /// A keyword is exact or contained, ignoring case; a payload is exact
    /// and case-sensitive; `any` takes a media but only a rule that says so.
    #[test]
    fn keywords_payloads_and_any_match_as_documented() {
        let exact = spec(Trigger::Keyword {
            words: vec!["STOP".to_owned()],
            mode: KeywordMode::Exact,
        })
        .checked()
        .unwrap()
        .trigger;
        assert!(exact.matches(Input::Text(" stop ")));
        assert!(!exact.matches(Input::Text("please stop")));
        let contains = spec(Trigger::Keyword {
            words: vec!["price".to_owned()],
            mode: KeywordMode::Contains,
        })
        .checked()
        .unwrap()
        .trigger;
        assert!(contains.matches(Input::Text("What is the PRICE?")));
        assert!(!contains.matches(Input::Other));
        let payload = spec(Trigger::Payload {
            ids: vec!["Menu-1".to_owned()],
        })
        .checked()
        .unwrap()
        .trigger;
        assert!(payload.matches(Input::Payload("Menu-1")));
        assert!(!payload.matches(Input::Payload("menu-1")));
        assert!(!payload.matches(Input::Text("Menu-1")));
        assert!(Trigger::Any.matches(Input::Other));
        assert!(Trigger::Any.matches(Input::Text("x")));
    }

    /// A pathological message costs a linear scan: a huge text against the
    /// most words of the most characters, no pattern compiled.
    #[test]
    fn a_huge_message_is_matched_in_linear_time() {
        let words: Vec<String> = (0..MAX_WORDS).map(|i| format!("{i:a<64}")).collect();
        let t = spec(Trigger::Keyword {
            words,
            mode: KeywordMode::Contains,
        })
        .checked()
        .unwrap()
        .trigger;
        let text = "a".repeat(100_000);
        let started = std::time::Instant::now();
        assert!(!t.matches(Input::Text(&text)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Each invalid field is named, and nothing is quoted.
    #[test]
    fn a_rule_is_checked_field_by_field() {
        let field = |change: &dyn Fn(&mut RuleSpec)| {
            let mut rule = spec(command(&["help"]));
            change(&mut rule);
            rule.checked().unwrap_err().field().map(str::to_owned)
        };
        assert_eq!(
            field(&|r| r.name = Some("  ".into())).as_deref(),
            Some("name")
        );
        assert_eq!(
            field(&|r| r.priority = MAX_PRIORITY + 1).as_deref(),
            Some("priority")
        );
        assert_eq!(
            field(&|r| r.cooldown_seconds = Some(0)).as_deref(),
            Some("cooldown_seconds")
        );
        assert_eq!(
            field(&|r| r.cooldown_seconds = Some(MAX_COOLDOWN_SECONDS + 1)).as_deref(),
            Some("cooldown_seconds")
        );
        assert_eq!(
            field(&|r| r.trigger = command(&[])).as_deref(),
            Some("trigger.names")
        );
        assert_eq!(
            field(&|r| r.trigger = command(&["two words"])).as_deref(),
            Some("trigger.names")
        );
        assert_eq!(
            field(&|r| {
                r.trigger = Trigger::Command {
                    names: vec!["a".into()],
                    prefixes: vec!["a".into()],
                }
            })
            .as_deref(),
            Some("trigger.prefixes")
        );
        assert_eq!(
            field(&|r| {
                r.action = Action::Reply {
                    text: "x".repeat(4097),
                    outside_window_template: None,
                }
            })
            .as_deref(),
            Some("action.text")
        );
        assert_eq!(
            field(&|r| {
                r.action = Action::Reply {
                    text: "  ".into(),
                    outside_window_template: None,
                }
            })
            .as_deref(),
            Some("action.text")
        );
        assert_eq!(
            field(&|r| {
                r.action = Action::Template {
                    template: serde_json::json!({"language": {"code": "en_US"}}),
                }
            })
            .as_deref(),
            Some("action.template")
        );
        assert_eq!(
            field(&|r| {
                r.trigger = Trigger::Any;
                r.action = Action::Help;
            })
            .as_deref(),
            Some("action")
        );
    }

    /// The help lists the enabled command rules that describe themselves,
    /// in the order given, and says so when there are none.
    #[test]
    fn the_help_lists_the_described_commands() {
        let rule = |names: &[&str], description: Option<&str>, enabled: bool| Rule {
            id: new_rule_id().unwrap(),
            created_ms: 0,
            spec: RuleSpec {
                description: description.map(str::to_owned),
                enabled,
                ..spec(command(names)).checked().unwrap()
            },
        };
        let rules = vec![
            rule(&["menu"], Some("Today's menu"), true),
            rule(&["secret"], Some("Hidden"), false),
            rule(&["x"], None, true),
            rule(&["hours", "open"], Some("Opening hours"), true),
        ];
        assert_eq!(
            help_text(&rules),
            "/menu - Today's menu\n/hours - Opening hours"
        );
        assert_eq!(help_text(&[]), "No commands are available.");
    }

    /// Owners and the banned are kept without a `+`, once each, sorted;
    /// blanks, spaces and too many are refused.
    #[test]
    fn the_lists_are_normalised() {
        let list = |entries: &[&str]| {
            identities(
                &entries.iter().map(|e| (*e).to_owned()).collect::<Vec<_>>(),
                "owners",
            )
        };
        assert_eq!(
            list(&["+16505551234", "16505551234", "US.1"]).unwrap(),
            ["16505551234", "US.1"]
        );
        assert!(list(&[" "]).is_err());
        assert!(list(&["a b"]).is_err());
        let many: Vec<String> = (0..=MAX_LIST).map(|i| i.to_string()).collect();
        assert!(identities(&many, "banned").is_err());
    }

    /// The marker is per number, message and rule, and no part can be
    /// shifted into another.
    #[test]
    fn a_marker_is_per_number_message_and_rule() {
        let pn = |s: &str| PhoneNumberId::new(s).unwrap();
        let a = marker_key(&pn("1"), "m", "r");
        assert_eq!(a, marker_key(&pn("1"), "m", "r"));
        assert_ne!(a, marker_key(&pn("2"), "m", "r"));
        assert_ne!(a, marker_key(&pn("1"), "n", "r"));
        assert_ne!(a, marker_key(&pn("1"), "m", "s"));
        assert_ne!(
            marker_key(&pn("1"), "ab", "c"),
            marker_key(&pn("1"), "a", "bc")
        );
        assert_eq!(a.namespace(), MARKER_NAMESPACE);
    }
}
