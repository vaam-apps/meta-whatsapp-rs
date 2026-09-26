//! Commands: [`Command`] (name, aliases, guards, cooldown, interactive
//! payloads), the [`CommandHandler`] that runs it, and what a handler
//! learns about the invocation ([`Invocation`], [`Args`]).

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_core::Result;

use crate::ctx::Ctx;

/// Runs a command, a listener or the unknown-command handler. Implemented
/// for every `Fn(Ctx) -> impl Future<Output = Result<()>>`, so a closure or
/// an `async fn(Ctx) -> Result<()>` is a handler.
///
/// A handler is a function of its [`Ctx`], so it can be unit-tested
/// without a bot: build the context with [`Ctx::new`] and, for a command,
/// [`Ctx::with_invocation`], then call it.
#[async_trait]
pub trait CommandHandler: Send + Sync + 'static {
    /// Handle one event.
    async fn call(&self, ctx: Ctx) -> Result<()>;
}

#[async_trait]
impl<F, Fut> CommandHandler for F
where
    F: Fn(Ctx) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    async fn call(&self, ctx: Ctx) -> Result<()> {
        self(ctx).await
    }
}

impl fmt::Debug for dyn CommandHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CommandHandler")
    }
}

/// Where a command may run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Scope {
    /// Private chats and groups.
    #[default]
    Anywhere,
    /// One-to-one chats only.
    PrivateOnly,
    /// Group chats only (the message carries a `group_id`).
    GroupOnly,
}

/// A command: a name, aliases, and the guards checked before its handler
/// runs. Build with [`Command::new`] and the chained setters; register with
/// `BotBuilder::command` or `Registrar::command`.
///
/// Names and aliases are matched as the bot's [`CommandParser`] normalizes
/// them (the default [`PrefixParser`] ignores case); interactive payloads
/// exactly.
///
/// [`CommandParser`]: crate::CommandParser
/// [`PrefixParser`]: crate::PrefixParser
#[derive(Clone)]
#[must_use]
pub struct Command {
    pub(crate) name: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) description: Option<String>,
    pub(crate) usage: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) hidden: bool,
    pub(crate) in_menu: bool,
    pub(crate) scope: Scope,
    pub(crate) owner_only: bool,
    pub(crate) cooldown: Option<Duration>,
    pub(crate) payloads: Vec<String>,
    pub(crate) metadata: BTreeMap<String, String>,
    pub(crate) handler: Arc<dyn CommandHandler>,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("name", &self.name)
            .field("aliases", &self.aliases)
            .field("scope", &self.scope)
            .field("owner_only", &self.owner_only)
            .field("cooldown", &self.cooldown)
            .field("hidden", &self.hidden)
            .finish_non_exhaustive()
    }
}

impl Command {
    /// A command `name` (without prefix) run by `handler`.
    pub fn new(name: impl Into<String>, handler: impl CommandHandler) -> Self {
        Self {
            name: name.into(),
            aliases: Vec::new(),
            description: None,
            usage: None,
            category: None,
            hidden: false,
            in_menu: true,
            scope: Scope::Anywhere,
            owner_only: false,
            cooldown: None,
            payloads: Vec::new(),
            metadata: BTreeMap::new(),
            handler: Arc::new(handler),
        }
    }

    /// Another name for the command.
    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    /// Shown in the help text and in Meta's command menu.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// An argument hint the help text shows after the name, e.g.
    /// `<order number>` (never sent to Meta's command menu).
    pub fn usage(mut self, usage: impl Into<String>) -> Self {
        self.usage = Some(usage.into());
        self
    }

    /// Keep `value` under `key` with the command, for your own help or
    /// menus ([`CommandInfo::metadata`]); the bot never reads it.
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// The help section; defaults to the registering plugin's category.
    pub fn category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    /// Leave the command out of the help text and the command menu (it
    /// still runs).
    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Whether to list the command in Meta's command menu (default: yes,
    /// unless hidden).
    pub fn menu(mut self, in_menu: bool) -> Self {
        self.in_menu = in_menu;
        self
    }

    /// Where the command may run.
    pub fn scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    /// Only in one-to-one chats.
    pub fn private_only(self) -> Self {
        self.scope(Scope::PrivateOnly)
    }

    /// Only in groups.
    pub fn group_only(self) -> Self {
        self.scope(Scope::GroupOnly)
    }

    /// Only for the owners the bot's `AccessPolicy` names.
    pub fn owner_only(mut self) -> Self {
        self.owner_only = true;
        self
    }

    /// At most once per `period` per user, per business number. Needs a
    /// cooldown store on the bot (`BotBuilder::cooldowns`).
    pub fn cooldown(mut self, period: Duration) -> Self {
        self.cooldown = Some(period);
        self
    }

    /// Also run when the user taps an interactive reply button or list row,
    /// or a template quick-reply button, whose id (payload) is `id`.
    pub fn payload(mut self, id: impl Into<String>) -> Self {
        self.payloads.push(id.into());
        self
    }

    /// The name, as given.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A command's arguments: the text after its name, split on whitespace,
/// with `"double quoted strings"` (or `“curly quoted”` ones, as phone
/// keyboards type them) kept as one argument.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    raw: String,
    items: Vec<String>,
}

impl Args {
    /// Split `raw` (see the type docs). An unterminated quote runs to the
    /// end; `""` is an empty argument.
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        let mut items = Vec::new();
        let mut current = String::new();
        let mut started = false;
        let mut closing: Option<char> = None;
        for c in raw.chars() {
            if let Some(close) = closing {
                if c == close {
                    closing = None;
                } else {
                    current.push(c);
                }
                continue;
            }
            match c {
                '"' => {
                    closing = Some('"');
                    started = true;
                }
                '\u{201C}' => {
                    closing = Some('\u{201D}');
                    started = true;
                }
                c if c.is_whitespace() => {
                    if started {
                        items.push(std::mem::take(&mut current));
                        started = false;
                    }
                }
                c => {
                    current.push(c);
                    started = true;
                }
            }
        }
        if started {
            items.push(current);
        }
        Self {
            raw: raw.to_owned(),
            items,
        }
    }

    /// The text after the command name, trimmed.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Argument `index`.
    pub fn get(&self, index: usize) -> Option<&str> {
        self.items.get(index).map(String::as_str)
    }

    /// All arguments.
    pub fn as_slice(&self) -> &[String] {
        &self.items
    }

    /// Number of arguments.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Iterate over the arguments.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(String::as_str)
    }
}

/// How a command was invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Trigger {
    /// Typed: `prefix` then `name` (the name or alias used, as the parser
    /// gave it).
    Text {
        /// The prefix matched.
        prefix: String,
        /// The name or alias used, as the parser gave it.
        name: String,
    },
    /// The caption of an image or a video: `prefix` then `name`. The media
    /// is in `ctx.message()`'s content.
    Caption {
        /// The prefix matched.
        prefix: String,
        /// The name or alias used, as the parser gave it.
        name: String,
    },
    /// A tapped reply button, list row or template quick-reply button with
    /// this id.
    Payload(String),
}

/// The command an event invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Invocation {
    /// The command's name as registered (normalized by the parser),
    /// whatever alias or payload was used.
    pub command: String,
    /// How it was invoked.
    pub trigger: Trigger,
    /// Its arguments (empty for a payload).
    pub args: Args,
}

impl Invocation {
    /// An invocation of `command`, e.g. to unit-test a handler with
    /// [`Ctx::with_invocation`].
    pub fn new(command: impl Into<String>, trigger: Trigger, args: Args) -> Self {
        Self {
            command: command.into(),
            trigger,
            args,
        }
    }
}

/// What the bot knows about a registered command, for help texts and menus.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommandInfo {
    /// Name, as the parser normalizes it (lowercased by default).
    pub name: String,
    /// Aliases, normalized the same way.
    pub aliases: Vec<String>,
    /// Description.
    pub description: Option<String>,
    /// Argument hint ([`Command::usage`]).
    pub usage: Option<String>,
    /// Help section.
    pub category: String,
    /// The plugin that registered it (`None`: the builder).
    pub plugin: Option<String>,
    /// Left out of help and menu (itself, or its plugin, is hidden).
    pub hidden: bool,
    /// Listed in Meta's command menu when not hidden.
    pub in_menu: bool,
    /// Where it may run.
    pub scope: Scope,
    /// Owners only.
    pub owner_only: bool,
    /// Per-user cooldown.
    pub cooldown: Option<Duration>,
    /// Interactive payload ids that run it.
    pub payloads: Vec<String>,
    /// What [`Command::metadata`] kept; the bot never reads it.
    pub metadata: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(raw: &str) -> Vec<String> {
        Args::parse(raw).as_slice().to_vec()
    }

    #[test]
    fn arguments_split_on_whitespace_and_keep_quoted_strings() {
        assert_eq!(
            items("cars racing on Mars"),
            ["cars", "racing", "on", "Mars"]
        );
        assert_eq!(items("  a \t b\n c  "), ["a", "b", "c"]);
        assert_eq!(items(r#"add "New York" 3"#), ["add", "New York", "3"]);
        assert_eq!(
            items("say \u{201C}hi there\u{201D} x"),
            ["say", "hi there", "x"]
        );
        assert_eq!(items(r#"a "" b"#), ["a", "", "b"]);
        assert_eq!(items(r#"a "unterminated rest"#), ["a", "unterminated rest"]);
        assert_eq!(items(r#"pre"fix mid"post"#), ["prefix midpost"]);
        assert!(Args::parse("   ").is_empty());
        let args = Args::parse("  one two  ");
        assert_eq!(args.raw(), "one two");
        assert_eq!(args.get(1), Some("two"));
        assert_eq!(args.get(2), None);
        assert_eq!(args.len(), 2);
        assert_eq!(args.iter().collect::<Vec<_>>(), ["one", "two"]);
    }
}
