//! The bot (scope `bot`; docs/design/server.md, section 4.2; roadmap M5k,
//! slice 2): the rules a tenant keeps for a number, which the service
//! evaluates on the number's inbound messages and answers from, and the
//! number's switch and lists.
//!
//! | Route | Does |
//! | --- | --- |
//! | `GET`, `PUT /v1/numbers/{pn}/bot` | the bot's switch, owners and banned; `PUT` replaces all three |
//! | `GET`, `POST /v1/numbers/{pn}/bot/rules` | the rules in the order they are tried; add one (`Idempotency-Key`) |
//! | `GET`, `PUT`, `DELETE /v1/numbers/{pn}/bot/rules/{rule_id}` | one rule |
//!
//! The core's [`meta_whatsapp_server_core::bot`] decides everything: a bot
//! is its tenant's or it does not exist (`404`), every word of a rule is
//! literal text, and no word, text or person reaches a log, an audit
//! event or an error.

use meta_whatsapp_rs::core::ids::PhoneNumberId;
use meta_whatsapp_rs::webhooks::axum::Json;
use meta_whatsapp_rs::webhooks::axum::extract::{Path, State};
use meta_whatsapp_rs::webhooks::axum::http::StatusCode;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_core::bot::{
    Action, Bot, BotView, Chat, DEFAULT_PRIORITY, KeywordMode, Rule, RuleSpec, Trigger,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use super::common::{ApiJson, rfc3339};
use crate::auth::Caller;
use crate::error::{ApiError, ErrorBody};
use crate::idempotency::{Fingerprint, KeyHeader, Success};
use crate::state::Grant;

/// How a keyword trigger compares.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = BotKeywordMode)]
pub enum KeywordModeBody {
    /// The whole message is the word, ignoring case and the space around.
    Exact,
    /// The message contains the word, ignoring case.
    Contains,
}

/// What a rule listens for. Every word is literal text: nothing a tenant
/// writes is a pattern.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = BotTrigger)]
pub enum TriggerBody {
    /// A text that starts with a prefix and then a name, ignoring case:
    /// `/help`.
    Command {
        /// The command's name and aliases (1 to 20, up to 64 characters
        /// each, no space, no prefix).
        names: Vec<String>,
        /// The prefixes (up to 5 of 1 or 2 symbols; `["/"]` when none).
        #[serde(default)]
        prefixes: Vec<String>,
    },
    /// A text that is, or contains, one of the words, ignoring case.
    Keyword {
        /// The words (1 to 20, up to 64 characters each).
        words: Vec<String>,
        /// Whole message or contained.
        mode: KeywordModeBody,
    },
    /// A tapped reply button or list row, or a template quick-reply
    /// button, by the id (or payload) it was sent with. Case matters.
    Payload {
        /// The ids (1 to 20, up to 64 characters each).
        ids: Vec<String>,
    },
    /// Any message with something to answer: a text, a tap or a media.
    /// Never a reaction, an edit, a deletion or a system message.
    Any {},
}

/// Which chats a rule answers in.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = BotChat)]
pub enum ChatBody {
    /// One-to-one chats.
    Private,
    /// Groups.
    Group,
    /// Both.
    Any,
}

/// What a rule does when it fires.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[schema(as = BotAction)]
pub enum ActionBody {
    /// Answer with a text.
    Reply {
        /// The text, at most 4,096 characters.
        text: String,
        /// Meta's template object (`name`, `language`, `components`) sent
        /// instead when the 24-hour window is closed; none: nothing is
        /// sent outside the window.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(value_type = Option<HashMap<String, Value>>)]
        outside_window_template: Option<Value>,
    },
    /// Answer with a template (a template needs no open window).
    Template {
        /// Meta's template object.
        #[schema(value_type = HashMap<String, Value>)]
        template: Value,
    },
    /// Answer with the list of the number's command rules that have a
    /// `description`, generated. Only for a `command` or `keyword`
    /// trigger.
    Help {},
}

const fn yes() -> bool {
    true
}

const fn default_priority() -> u16 {
    DEFAULT_PRIORITY
}

const fn private() -> ChatBody {
    ChatBody::Private
}

/// A rule.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = BotRuleRequest)]
pub struct RuleBody {
    /// A label for your own use (up to 64 characters).
    pub name: Option<String>,
    /// What the rule is for (up to 200 characters); a command rule's is
    /// its line in the generated help.
    pub description: Option<String>,
    /// A rule switched off is kept and never tried. Default `true`.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The order rules are tried in, from 0 to 10,000 (default 100); equal
    /// priorities go by age.
    #[serde(default = "default_priority")]
    pub priority: u16,
    /// What it listens for.
    pub trigger: TriggerBody,
    /// Which chats it answers in (default `private`).
    #[serde(default = "private")]
    pub chat: ChatBody,
    /// Only the number's owners may fire it (default `false`).
    #[serde(default)]
    pub owners_only: bool,
    /// Seconds (1 to 86,400) before the same person can fire it again.
    pub cooldown_seconds: Option<u32>,
    /// What it does.
    pub action: ActionBody,
    /// Let the rules after it be tried when it answered (default
    /// `false`: the first that answers decides).
    #[serde(default)]
    pub continue_after: bool,
}

/// The switch, the owners and the banned of a number's bot.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
#[schema(as = BotSettingsRequest)]
pub struct SettingsBody {
    /// Whether the bot answers. A bot is off until a tenant turns it on.
    pub enabled: bool,
    /// Phone numbers (`+16505551234`, or without the `+`) and business-
    /// scoped user ids who pass `owners_only` rules (up to 200).
    #[serde(default)]
    pub owners: Vec<String>,
    /// Who gets no answer at all (up to 200), named the same way.
    #[serde(default)]
    pub banned: Vec<String>,
}

/// A number's bot.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BotSettings)]
pub struct SettingsView {
    /// Whether the bot answers.
    pub enabled: bool,
    /// The owners, without a `+`.
    pub owners: Vec<String>,
    /// The banned, without a `+`.
    pub banned: Vec<String>,
    /// How many rules it has.
    pub rules: usize,
    /// The most rules a number may have.
    pub max_rules: usize,
}

impl From<BotView> for SettingsView {
    fn from(view: BotView) -> Self {
        Self {
            enabled: view.enabled,
            owners: view.owners,
            banned: view.banned,
            rules: view.rules,
            max_rules: view.max_rules,
        }
    }
}

/// A rule of a number's bot.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BotRule)]
pub struct RuleView {
    /// Its id.
    pub id: String,
    /// When it was made (RFC 3339).
    pub created_at: String,
    /// The label.
    pub name: Option<String>,
    /// What it is for.
    pub description: Option<String>,
    /// Whether it is tried.
    pub enabled: bool,
    /// The order it is tried in.
    pub priority: u16,
    /// What it listens for, normalised (words in lower case).
    pub trigger: TriggerBody,
    /// Which chats.
    pub chat: ChatBody,
    /// Owners only.
    pub owners_only: bool,
    /// The cooldown, in seconds.
    pub cooldown_seconds: Option<u32>,
    /// What it does.
    pub action: ActionBody,
    /// Whether the rules after it are tried when it answered.
    pub continue_after: bool,
}

/// The rules of a number, in the order they are tried.
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = BotRuleList)]
pub struct RuleList {
    /// The rules.
    pub data: Vec<RuleView>,
}

impl From<TriggerBody> for Trigger {
    fn from(body: TriggerBody) -> Self {
        match body {
            TriggerBody::Command { names, prefixes } => Self::Command { names, prefixes },
            TriggerBody::Keyword { words, mode } => Self::Keyword {
                words,
                mode: match mode {
                    KeywordModeBody::Exact => KeywordMode::Exact,
                    KeywordModeBody::Contains => KeywordMode::Contains,
                },
            },
            TriggerBody::Payload { ids } => Self::Payload { ids },
            TriggerBody::Any {} => Self::Any,
        }
    }
}

impl From<Trigger> for TriggerBody {
    fn from(trigger: Trigger) -> Self {
        match trigger {
            Trigger::Command { names, prefixes } => Self::Command { names, prefixes },
            Trigger::Keyword { words, mode } => Self::Keyword {
                words,
                mode: match mode {
                    KeywordMode::Exact => KeywordModeBody::Exact,
                    KeywordMode::Contains => KeywordModeBody::Contains,
                },
            },
            Trigger::Payload { ids } => Self::Payload { ids },
            Trigger::Any => Self::Any {},
        }
    }
}

impl From<ChatBody> for Chat {
    fn from(body: ChatBody) -> Self {
        match body {
            ChatBody::Private => Self::Private,
            ChatBody::Group => Self::Group,
            ChatBody::Any => Self::Any,
        }
    }
}

impl From<Chat> for ChatBody {
    fn from(chat: Chat) -> Self {
        match chat {
            Chat::Private => Self::Private,
            Chat::Group => Self::Group,
            Chat::Any => Self::Any,
        }
    }
}

impl From<ActionBody> for Action {
    fn from(body: ActionBody) -> Self {
        match body {
            ActionBody::Reply {
                text,
                outside_window_template,
            } => Self::Reply {
                text,
                outside_window_template,
            },
            ActionBody::Template { template } => Self::Template { template },
            ActionBody::Help {} => Self::Help,
        }
    }
}

impl From<Action> for ActionBody {
    fn from(action: Action) -> Self {
        match action {
            Action::Reply {
                text,
                outside_window_template,
            } => Self::Reply {
                text,
                outside_window_template,
            },
            Action::Template { template } => Self::Template { template },
            Action::Help => Self::Help {},
        }
    }
}

impl From<RuleBody> for RuleSpec {
    fn from(body: RuleBody) -> Self {
        Self {
            name: body.name,
            description: body.description,
            enabled: body.enabled,
            priority: body.priority,
            trigger: body.trigger.into(),
            chat: body.chat.into(),
            owners_only: body.owners_only,
            cooldown_seconds: body.cooldown_seconds,
            action: body.action.into(),
            continue_after: body.continue_after,
        }
    }
}

impl From<Rule> for RuleView {
    fn from(rule: Rule) -> Self {
        Self {
            created_at: rfc3339(
                time::OffsetDateTime::from_unix_timestamp_nanos(
                    i128::from(rule.created_ms) * 1_000_000,
                )
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH),
            ),
            id: rule.id,
            name: rule.spec.name,
            description: rule.spec.description,
            enabled: rule.spec.enabled,
            priority: rule.spec.priority,
            trigger: rule.spec.trigger.into(),
            chat: rule.spec.chat.into(),
            owners_only: rule.spec.owners_only,
            cooldown_seconds: rule.spec.cooldown_seconds,
            action: rule.spec.action.into(),
            continue_after: rule.spec.continue_after,
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, ApiError> {
    serde_json::from_value(body).map_err(|_| ApiError::invalid("body"))
}

fn number(pn: String) -> Result<PhoneNumberId, ApiError> {
    // Only a numeric id can be bound: anything else does not exist.
    PhoneNumberId::new(pn).map_err(|_| ApiError::not_found())
}

fn operations<'a>(state: &'a Grant, caller: &'a Caller) -> Result<Bot<'a>, ApiError> {
    Ok(Bot::new(state.authz(), caller, state.bot())?)
}

/// `GET /v1/numbers/{pn}/bot`: the bot's switch and lists.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/bot",
    tag = "bot",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The bot (off, with no rules, for a number never configured)", body = SettingsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn get_bot_settings(
    State(state): State<Grant>,
    caller: Caller,
    Path(pn): Path<String>,
) -> Result<Json<SettingsView>, ApiError> {
    let view = operations(&state, &caller)?.settings(&number(pn)?).await?;
    Ok(Json(view.into()))
}

/// `PUT /v1/numbers/{pn}/bot`: switch the bot and set its lists.
#[utoipa::path(
    put,
    path = "/v1/numbers/{pn}/bot",
    tag = "bot",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = SettingsBody,
    responses(
        (status = 200, description = "The bot as it now stands; audited `bot_settings_updated`", body = SettingsView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `body`, `owners` or `banned`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn update_bot_settings(
    State(state): State<Grant>,
    caller: Caller,
    Path(pn): Path<String>,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<SettingsView>, ApiError> {
    let body = parse::<SettingsBody>(body)?;
    let view = operations(&state, &caller)?
        .update_settings(&number(pn)?, body.enabled, &body.owners, &body.banned)
        .await?;
    Ok(Json(view.into()))
}

/// `GET /v1/numbers/{pn}/bot/rules`: the rules, in the order they are
/// tried.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/bot/rules",
    tag = "bot",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The rules by priority, then age", body = RuleList),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn list_bot_rules(
    State(state): State<Grant>,
    caller: Caller,
    Path(pn): Path<String>,
) -> Result<Json<RuleList>, ApiError> {
    let rules = operations(&state, &caller)?
        .list_rules(&number(pn)?)
        .await?;
    Ok(Json(RuleList {
        data: rules.into_iter().map(RuleView::from).collect(),
    }))
}

/// `POST /v1/numbers/{pn}/bot/rules`: add a rule.
#[utoipa::path(
    post,
    path = "/v1/numbers/{pn}/bot/rules",
    tag = "bot",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
        ("Idempotency-Key" = Option<String>, Header, description = "1 to 255 visible ASCII characters, scoped to the tenant: the same key and body never add two rules"),
    ),
    request_body = RuleBody,
    responses(
        (status = 201, description = "The rule, normalised; audited `bot_rule_created`", body = RuleView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number for this tenant", body = ErrorBody),
        (status = 409, description = "`idempotency_in_progress`", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `body`, `rules` (the number has as many as allowed) or the rule's field at fault; `idempotency_key_reused`", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn create_bot_rule(
    State(state): State<Grant>,
    caller: Caller,
    Path(pn): Path<String>,
    KeyHeader(key): KeyHeader,
    ApiJson(body): ApiJson<Value>,
) -> Response {
    let pn = match number(pn) {
        Ok(pn) => pn,
        Err(error) => return error.into_response(),
    };
    let fingerprint = Fingerprint::create_bot_rule(&pn, &body);
    let spec = match parse::<RuleBody>(body) {
        Ok(body) => RuleSpec::from(body),
        Err(error) => return error.into_response(),
    };
    let act = async {
        let rule = operations(&state, &caller)?.create_rule(&pn, spec).await?;
        Success::json(StatusCode::CREATED, &RuleView::from(rule))
    };
    state.idempotent(&caller, key, fingerprint, act).await
}

/// `GET /v1/numbers/{pn}/bot/rules/{rule_id}`: one rule.
#[utoipa::path(
    get,
    path = "/v1/numbers/{pn}/bot/rules/{rule_id}",
    tag = "bot",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("rule_id" = String, Path, description = "The rule's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 200, description = "The rule", body = RuleView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number or rule for this tenant", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `read` budget", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn get_bot_rule(
    State(state): State<Grant>,
    caller: Caller,
    Path((pn, id)): Path<(String, String)>,
) -> Result<Json<RuleView>, ApiError> {
    let rule = operations(&state, &caller)?
        .get_rule(&number(pn)?, &id)
        .await?;
    Ok(Json(rule.into()))
}

/// `PUT /v1/numbers/{pn}/bot/rules/{rule_id}`: replace a rule.
#[utoipa::path(
    put,
    path = "/v1/numbers/{pn}/bot/rules/{rule_id}",
    tag = "bot",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("rule_id" = String, Path, description = "The rule's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    request_body = RuleBody,
    responses(
        (status = 200, description = "The rule as it now stands (its id and date stay); audited `bot_rule_updated`", body = RuleView),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number or rule for this tenant", body = ErrorBody),
        (status = 422, description = "`invalid_request` on `body` or the rule's field at fault", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn replace_bot_rule(
    State(state): State<Grant>,
    caller: Caller,
    Path((pn, id)): Path<(String, String)>,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<RuleView>, ApiError> {
    let spec = RuleSpec::from(parse::<RuleBody>(body)?);
    let rule = operations(&state, &caller)?
        .replace_rule(&number(pn)?, &id, spec)
        .await?;
    Ok(Json(rule.into()))
}

/// `DELETE /v1/numbers/{pn}/bot/rules/{rule_id}`: remove a rule.
#[utoipa::path(
    delete,
    path = "/v1/numbers/{pn}/bot/rules/{rule_id}",
    tag = "bot",
    security(("api_key" = [])),
    params(
        ("pn" = String, Path, description = "Phone number id"),
        ("rule_id" = String, Path, description = "The rule's id"),
        ("WA-Tenant" = Option<String>, Header, description = "The tenant a platform key acts as"),
    ),
    responses(
        (status = 204, description = "Removed; audited `bot_rule_deleted`"),
        (status = 401, description = "No valid key", body = ErrorBody),
        (status = 403, description = "`forbidden`, `tenant_suspended`", body = ErrorBody),
        (status = 404, description = "`not_found`: no such number or rule for this tenant", body = ErrorBody),
        (status = 429, description = "`too_many_requests`: the tenant's `send` budget", body = ErrorBody),
        (status = 503, description = "`storage_unavailable`", body = ErrorBody),
    )
)]
pub(crate) async fn delete_bot_rule(
    State(state): State<Grant>,
    caller: Caller,
    Path((pn, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    operations(&state, &caller)?
        .delete_rule(&number(pn)?, &id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A body is the documented object and no more; its defaults are the
    /// documented ones, and a rule round-trips through the core's type.
    #[test]
    fn a_body_is_the_documented_object_with_its_defaults() {
        let body = json!({
            "trigger": {"type": "command", "names": ["Help"]},
            "action": {"type": "reply", "text": "hello"},
        });
        let parsed = parse::<RuleBody>(body).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.priority, 100);
        assert!(matches!(parsed.chat, ChatBody::Private));
        assert!(!parsed.owners_only && !parsed.continue_after);
        let spec = RuleSpec::from(parsed).checked().unwrap();
        let Trigger::Command { names, prefixes } = &spec.trigger else {
            panic!("a command");
        };
        assert_eq!((names.as_slice(), prefixes.as_slice()), (&["help".to_owned()][..], &["/".to_owned()][..]));
        for bad in [
            json!({"trigger": {"type": "regex", "pattern": ".*"}, "action": {"type": "help"}}),
            json!({"trigger": {"type": "any"}, "action": {"type": "reply", "text": "x"}, "to": "x"}),
            json!({"trigger": {"type": "any", "names": []}, "action": {"type": "help"}}),
            json!({"action": {"type": "help"}}),
        ] {
            assert!(parse::<RuleBody>(bad).is_err());
        }
    }
}
