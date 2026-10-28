//! Commands: [`Command`] (name, aliases, guards, cooldown, interactive
//! payloads, subcommands, flags), the [`CommandHandler`] that runs it, and
//! what a handler learns about the invocation ([`Invocation`], [`Args`],
//! [`Flag`]).
//!
//! # Subcommands
//!
//! A command may have subcommands ([`Command::subcommand`]), one level
//! deep: `/order status 42` runs `status` under `order`, with `42` as its
//! only argument. The subcommand is the first word after the command's
//! name, typed outside quotes and compared as the parser normalizes names
//! (case-insensitive by default); anything else (`/order` alone,
//! `/order 42`, `/order "status"`, `/order --all status`) runs the parent
//! with every argument. A subcommand has its own aliases, usage,
//! description, payloads, flags, scope and owner guards and cooldown; the
//! parent's scope and owner guards apply to it too, its cooldown does not.
//!
//! # Flags
//!
//! A command that declares a flag ([`Command::flag`]), or takes any
//! ([`Command::pass_unknown_flags`]), reads flags: an argument typed as
//! `--name` or `--name=value` is then a flag ([`Flag`]), read with
//! [`Args::has_flag`], [`Args::flag`] and [`Args::flags`], and the
//! positional arguments ([`Args::get`]) are the rest. A command that does
//! neither reads none: every word is a positional argument, as typed, so
//! free text keeps its `--` words (`/say --hi there` has the arguments
//! `--hi` and `there`). Each command and subcommand has its own set: a
//! parent's flags do not make its subcommands read flags, nor the
//! reverse. The rules, kept small on purpose:
//!
//! - only the long form: `-n` is a positional argument (so is a negative
//!   number), and `--limit 5` is the flag `limit` without a value then the
//!   positional argument `5`: write `--limit=5`;
//! - the value runs from the first `=` to the end of the word and may be
//!   quoted (`--note="two words"`); `--limit=` has the empty value;
//! - a word quoted from its start (`"--limit=5"`), or whose name is
//!   quoted, is a positional argument;
//! - `--` alone ends the flags: every word after it is positional, and it
//!   is not an argument itself;
//! - a leading em dash (`—limit=5`) reads as `--`: phone keyboards type one
//!   for two hyphens, as they type curly quotes. An em dash alone is a
//!   positional argument (it is punctuation), not the end of the flags;
//! - names compare exactly, as typed; a flag given twice is kept twice
//!   ([`Args::flags`]), and [`Args::flag`] reads the last value.
//!
//! A command that declares flags and is typed with one it does not
//! declare is refused
//! ([`Refusal::UnknownFlag`](crate::Refusal::UnknownFlag), told with its
//! usage by the default [`Refusals`](crate::Refusals)) unless it takes any
//! ([`Command::pass_unknown_flags`]): a mistyped `--dryrun` must not run
//! the real thing.

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
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent setting of its own chained setter, not a state machine"
)]
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
    pub(crate) flags: Vec<String>,
    pub(crate) pass_unknown_flags: bool,
    pub(crate) subcommands: Vec<Command>,
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
            .field("flags", &self.flags)
            .field("subcommands", &self.subcommands)
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
            flags: Vec::new(),
            pass_unknown_flags: false,
            subcommands: Vec::new(),
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

    /// A subcommand: `/<name> <subcommand's name> args…` runs `subcommand`
    /// with the arguments after its name (see the [module docs](self)).
    /// Its guards are its own plus the parent's scope and owner-only; its
    /// cooldown is kept apart from the parent's. Its category and menu
    /// setting are not used: it is listed under its parent, in the help
    /// only (Meta's command menu lists commands, not subcommands).
    /// Subcommands nest one level: a subcommand with subcommands of its
    /// own fails `BotBuilder::build`.
    pub fn subcommand(mut self, subcommand: Command) -> Self {
        self.subcommands.push(subcommand);
        self
    }

    /// Declare the flag `--name` (or `--name=value`), `name` written
    /// without the dashes; read it with [`Args::has_flag`] or
    /// [`Args::flag`]. A command with a declared flag reads flags (see the
    /// [module docs](self#flags)), and a typed flag it does not declare
    /// refuses it (unless [`Self::pass_unknown_flags`]); a command without
    /// one reads every word as a positional argument. A subcommand's
    /// flags are its own: declare them on it, not on its parent. Write the
    /// flags in [`Self::usage`] for the help to show them, e.g.
    /// `<order number> [--dry-run]`.
    pub fn flag(mut self, name: impl Into<String>) -> Self {
        self.flags.push(name.into());
        self
    }

    /// Read flags, and run with ones the command does not declare too
    /// (they are in [`Args::flags`]) instead of refusing it: for a command
    /// that reads them itself, or hands them on. Without a declared flag,
    /// this alone makes the command read flags.
    pub fn pass_unknown_flags(mut self) -> Self {
        self.pass_unknown_flags = true;
        self
    }

    /// The name, as given.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A flag in a command's arguments: `--name` or `--name=value` (see the
/// [module docs](self) for the rules).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Flag {
    /// The name, without the dashes, as typed.
    pub name: String,
    /// What follows the first `=` (`Some("")` for `--name=`); `None` for
    /// `--name`.
    pub value: Option<String>,
}

/// A command's arguments: the text after its name (after its
/// subcommand's, for a subcommand), split on whitespace, with
/// `"double quoted strings"` (or `“curly quoted”` ones, as phone keyboards
/// type them) kept as one argument.
///
/// For a command that reads flags (it declares one, [`Command::flag`], or
/// takes any, [`Command::pass_unknown_flags`]), the flags (`--dry-run`,
/// `--limit=5`) are read apart ([`Self::flags`]) and [`Self::get`],
/// [`Self::len`] and [`Self::iter`] count the positional arguments only;
/// for any other command there are no flags and every word is an
/// argument. The bot reads a command's arguments in its mode, whichever
/// constructor built them; to unit-test a handler, build them with
/// [`Self::parse`] or [`Self::parse_with_flags`] as the command would. The
/// flag rules are in the [module docs](self#flags).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    raw: String,
    items: Vec<String>,
    flags: Vec<Flag>,
}

/// One word of the arguments, as split.
struct Word {
    text: String,
    /// Bytes of `text` typed before the word's first quote (all of them
    /// for a word without quotes).
    unquoted: usize,
    /// Whether a quote opened in the word.
    quoted: bool,
    /// Byte offset, in the text split, just past the word.
    end: usize,
}

impl Word {
    /// The flag this word is, if it is one: `--` (or an em dash) and a
    /// name typed outside quotes, then optionally `=` and a value, quoted
    /// or not.
    fn flag(&self) -> Option<Flag> {
        let body = self
            .text
            .strip_prefix("--")
            .or_else(|| self.text.strip_prefix('\u{2014}'))?;
        let dashes = self.text.len() - body.len();
        let (name, value) = match body.split_once('=') {
            // The dashes, the name and the `=` were typed outside quotes.
            Some((name, value)) if dashes + name.len() < self.unquoted => (name, Some(value)),
            Some(_) => return None,
            None if self.quoted => return None,
            None => (body, None),
        };
        if name.is_empty() || name.starts_with(['-', '\u{2014}']) {
            return None;
        }
        Some(Flag {
            name: name.to_owned(),
            value: value.map(str::to_owned),
        })
    }
}

/// Split `raw` into words: whitespace outside quotes separates them; an
/// unterminated quote runs to the end.
fn words(raw: &str) -> Vec<Word> {
    fn word(text: String, unquoted: Option<usize>, end: usize) -> Word {
        Word {
            unquoted: unquoted.unwrap_or(text.len()),
            quoted: unquoted.is_some(),
            text,
            end,
        }
    }
    let mut words = Vec::new();
    let mut current = String::new();
    let mut unquoted: Option<usize> = None;
    let mut started = false;
    let mut closing: Option<char> = None;
    for (at, c) in raw.char_indices() {
        if let Some(close) = closing {
            if c == close {
                closing = None;
            } else {
                current.push(c);
            }
            continue;
        }
        let opens = match c {
            '"' => Some('"'),
            '\u{201C}' => Some('\u{201D}'),
            _ => None,
        };
        if let Some(close) = opens {
            closing = Some(close);
            unquoted.get_or_insert(current.len());
            started = true;
        } else if c.is_whitespace() {
            if started {
                words.push(word(std::mem::take(&mut current), unquoted.take(), at));
                started = false;
            }
        } else {
            current.push(c);
            started = true;
        }
    }
    if started {
        words.push(word(current, unquoted, raw.len()));
    }
    words
}

impl Args {
    /// Split `raw` (see the type docs), every word a positional argument:
    /// the arguments of a command that reads no flags. An unterminated
    /// quote runs to the end; `""` is an empty argument.
    pub fn parse(raw: &str) -> Self {
        Self::split(raw, false)
    }

    /// Split `raw` as [`Self::parse`] does, reading the flags apart (the
    /// rules are in the [module docs](self#flags)): the arguments of a
    /// command that reads flags.
    pub fn parse_with_flags(raw: &str) -> Self {
        Self::split(raw, true)
    }

    fn split(raw: &str, read_flags: bool) -> Self {
        let raw = raw.trim();
        let mut items = Vec::new();
        let mut flags = Vec::new();
        let mut flags_ended = !read_flags;
        for word in words(raw) {
            if !flags_ended {
                if !word.quoted && word.text == "--" {
                    flags_ended = true;
                    continue;
                }
                if let Some(flag) = word.flag() {
                    flags.push(flag);
                    continue;
                }
            }
            items.push(word.text);
        }
        Self {
            raw: raw.to_owned(),
            items,
            flags,
        }
    }

    /// The first word, when typed outside quotes, and the text after it: a
    /// subcommand's name and its arguments, unparsed.
    pub(crate) fn split_first(&self) -> Option<(String, &str)> {
        let first = words(&self.raw).into_iter().next()?;
        if first.quoted {
            return None;
        }
        Some((first.text, &self.raw[first.end..]))
    }

    /// The text after the command name (after the subcommand's, for a
    /// subcommand), trimmed, flags included.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Positional argument `index` (flags are not counted).
    pub fn get(&self, index: usize) -> Option<&str> {
        self.items.get(index).map(String::as_str)
    }

    /// All positional arguments.
    pub fn as_slice(&self) -> &[String] {
        &self.items
    }

    /// Number of positional arguments.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there are no positional arguments (there may be flags).
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Iterate over the positional arguments.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(String::as_str)
    }

    /// The flags, in the order typed, repeats included (none when read
    /// without flags, [`Self::parse`]).
    pub fn flags(&self) -> &[Flag] {
        &self.flags
    }

    /// Whether the flag `name` (without the dashes) was typed, with or
    /// without a value.
    pub fn has_flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f.name == name)
    }

    /// The value of the last `--name=value` typed (`Some("")` for
    /// `--name=`); `None` when the flag was not typed with a value.
    pub fn flag(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .filter(|f| f.name == name)
            .find_map(|f| f.value.as_deref())
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
    /// Its arguments (empty for a payload); for a subcommand, the ones
    /// after the subcommand's name.
    pub args: Args,
    /// The subcommand of [`Self::command`] invoked, by its name as
    /// registered (normalized by the parser), whatever alias or payload
    /// was used; `None` when the command itself runs.
    pub subcommand: Option<String>,
}

impl Invocation {
    /// An invocation of `command`, e.g. to unit-test a handler with
    /// [`Ctx::with_invocation`] (of `command` itself: see
    /// [`Self::with_subcommand`]).
    pub fn new(command: impl Into<String>, trigger: Trigger, args: Args) -> Self {
        Self {
            command: command.into(),
            trigger,
            args,
            subcommand: None,
        }
    }

    /// This invocation, of `command`'s subcommand `subcommand` instead
    /// (`args` are then the subcommand's).
    #[must_use]
    pub fn with_subcommand(mut self, subcommand: impl Into<String>) -> Self {
        self.subcommand = Some(subcommand.into());
        self
    }
}

/// What the bot knows about a registered command, for help texts and menus.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each mirrors an independent setting of `Command`, not a state machine"
)]
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
    /// The flags it declares ([`Command::flag`]), without the dashes.
    pub flags: Vec<String>,
    /// Whether it runs with flags it does not declare
    /// ([`Command::pass_unknown_flags`]).
    pub pass_unknown_flags: bool,
    /// Its subcommands, in registration order (none for a subcommand).
    /// A subcommand's `category` and `plugin` are its parent's; its
    /// `hidden` is its own, or its plugin's.
    pub subcommands: Vec<CommandInfo>,
}

impl CommandInfo {
    /// Whether the command reads flags: it declares one or takes any.
    pub(crate) fn reads_flags(&self) -> bool {
        !self.flags.is_empty() || self.pass_unknown_flags
    }

    /// `raw` read as this command's arguments: with flags or without.
    pub(crate) fn args(&self, raw: &str) -> Args {
        if self.reads_flags() {
            Args::parse_with_flags(raw)
        } else {
            Args::parse(raw)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The positional arguments, flags read apart.
    fn items(raw: &str) -> Vec<String> {
        Args::parse_with_flags(raw).as_slice().to_vec()
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

    /// Decisive: read without flags (a command that declares none), every
    /// word is a positional argument exactly as before flags existed. Make
    /// `Args::parse` read flags and this fails.
    #[test]
    fn without_flags_every_word_is_positional() {
        let raw = r#"--hi there -- --x=1 —y "--q" -n --"#;
        let args = Args::parse(raw);
        assert_eq!(
            args.as_slice(),
            ["--hi", "there", "--", "--x=1", "—y", "--q", "-n", "--"]
        );
        assert_eq!(args.len(), 8);
        assert_eq!(args.get(0), Some("--hi"));
        assert_eq!(args.iter().next(), Some("--hi"));
        assert!(args.flags().is_empty());
        assert!(!args.has_flag("hi"));
        assert_eq!(args.flag("x"), None);
        assert_eq!(args.raw(), raw);
        // The same text, read with flags.
        let with = Args::parse_with_flags(raw);
        assert_eq!(with.as_slice(), ["there", "--x=1", "—y", "--q", "-n", "--"]);
        assert_eq!(
            with.flags(),
            [Flag {
                name: "hi".into(),
                value: None
            }]
        );
    }

    /// `(name, value)` of each flag, in order.
    fn flags(raw: &str) -> Vec<(String, Option<String>)> {
        Args::parse_with_flags(raw)
            .flags()
            .iter()
            .map(|f| (f.name.clone(), f.value.clone()))
            .collect()
    }

    fn flag(name: &str, value: Option<&str>) -> (String, Option<String>) {
        (name.to_owned(), value.map(str::to_owned))
    }

    #[test]
    fn flags_are_read_apart_from_the_positional_arguments() {
        let args = Args::parse_with_flags("42 --dry-run --limit=5 x");
        assert_eq!(args.as_slice(), ["42", "x"]);
        assert_eq!(args.len(), 2);
        assert_eq!(args.get(1), Some("x"));
        assert!(args.has_flag("dry-run"));
        assert_eq!(args.flag("dry-run"), None, "typed without a value");
        assert_eq!(args.flag("limit"), Some("5"));
        assert!(!args.has_flag("limit=5"));
        assert!(!args.has_flag("dry"));
        // `raw` keeps everything as typed.
        assert_eq!(args.raw(), "42 --dry-run --limit=5 x");
        // Flags only: no positional argument.
        let only = Args::parse_with_flags("--all");
        assert!(only.is_empty());
        assert!(only.has_flag("all"));
    }

    /// Decisive: remove the quote check (`Word::flag` reading a quoted
    /// word) and `"--limit=5"` becomes a flag.
    #[test]
    fn a_quoted_flag_stays_a_positional_argument() {
        let args = Args::parse_with_flags(r#""--limit=5" --x"#);
        assert_eq!(args.as_slice(), ["--limit=5"]);
        assert!(!args.has_flag("limit"));
        assert_eq!(flags(r#""--limit=5" --x"#), [flag("x", None)]);
        // Curly quotes too; a quoted name; a word quoted whole.
        assert_eq!(items("\u{201C}--limit=5\u{201D}"), ["--limit=5"]);
        assert_eq!(items(r#"--"limit"=5"#), ["--limit=5"]);
        assert_eq!(items(r#"--li"mit=5""#), ["--limit=5"]);
        assert_eq!(items(r#""--dry-run""#), ["--dry-run"]);
        assert_eq!(items(r#"--dry-run"""#), ["--dry-run"]);
        // The value may be quoted, the name and `=` typed outside.
        assert_eq!(
            flags(r#"--note="two words" --empty="""#),
            [flag("note", Some("two words")), flag("empty", Some(""))]
        );
        assert!(Args::parse_with_flags(r#"--note="two words""#).is_empty());
        // Quoted, `--` is an argument, not the end of the flags.
        let args = Args::parse_with_flags(r#""--" --x"#);
        assert_eq!(args.as_slice(), ["--"]);
        assert!(args.has_flag("x"));
    }

    #[test]
    fn a_double_dash_ends_the_flags() {
        let args = Args::parse_with_flags("--a -- --b=1 -- c");
        assert_eq!(flags("--a -- --b=1 -- c"), [flag("a", None)]);
        // The first `--` is not an argument; later ones are.
        assert_eq!(args.as_slice(), ["--b=1", "--", "c"]);
        let trailing = Args::parse_with_flags("x --");
        assert_eq!(trailing.as_slice(), ["x"]);
        assert!(trailing.flags().is_empty());
        assert!(Args::parse_with_flags("--").is_empty());
    }

    #[test]
    fn empty_values_repeats_and_what_is_not_a_flag() {
        // An empty value is a value.
        let args = Args::parse_with_flags("--limit=");
        assert_eq!(args.flag("limit"), Some(""));
        assert!(args.is_empty());
        // Only the first `=` splits.
        assert_eq!(flags("--q=a=b"), [flag("q", Some("a=b"))]);
        // Repeats are kept in order; `flag` reads the last value given.
        let args = Args::parse_with_flags("--limit=5 --tag=a --limit=7 --limit --tag=b");
        assert_eq!(
            flags("--limit=5 --tag=a --limit=7 --limit --tag=b"),
            [
                flag("limit", Some("5")),
                flag("tag", Some("a")),
                flag("limit", Some("7")),
                flag("limit", None),
                flag("tag", Some("b")),
            ]
        );
        assert_eq!(args.flag("limit"), Some("7"));
        assert_eq!(args.flag("tag"), Some("b"));
        // A space does not give a value: `5` is positional.
        let args = Args::parse_with_flags("--limit 5");
        assert_eq!(args.flag("limit"), None);
        assert!(args.has_flag("limit"));
        assert_eq!(args.as_slice(), ["5"]);
        // Short options, negative numbers, a lone dash, no name, three dashes.
        assert_eq!(
            items("-n -5 - --=5 --- ---x"),
            ["-n", "-5", "-", "--=5", "---", "---x"]
        );
        assert!(
            Args::parse_with_flags("-n -5 - --=5 --- ---x")
                .flags()
                .is_empty()
        );
        // Names compare as typed.
        let args = Args::parse_with_flags("--Limit=5");
        assert_eq!(args.flag("limit"), None);
        assert_eq!(args.flag("Limit"), Some("5"));
    }

    #[test]
    fn unicode_names_values_and_the_em_dash() {
        assert_eq!(
            flags("--größe=groß --日本=語 --emoji=🙂"),
            [
                flag("größe", Some("groß")),
                flag("日本", Some("語")),
                flag("emoji", Some("🙂")),
            ]
        );
        // A phone keyboard's em dash for `--`.
        let args = Args::parse_with_flags("—dry-run —limit=5 42");
        assert!(args.has_flag("dry-run"));
        assert_eq!(args.flag("limit"), Some("5"));
        assert_eq!(args.as_slice(), ["42"]);
        // A lone em dash is punctuation: positional, and flags go on.
        let args = Args::parse_with_flags("a — b --x");
        assert_eq!(args.as_slice(), ["a", "—", "b"]);
        assert!(args.has_flag("x"));
        assert_eq!(items("——x —-x —=1"), ["——x", "—-x", "—=1"]);
        // The en dash is not read as `--`, nor an em dash inside a word.
        assert_eq!(items("–x"), ["–x"]);
        assert_eq!(items("a—x b—limit=5 -—x"), ["a—x", "b—limit=5", "-—x"]);
        assert!(Args::parse_with_flags("a—x b—limit=5").flags().is_empty());
        // `--` inside a quoted value, or quoted mid-word, ends nothing.
        let args = Args::parse_with_flags(r#"--note="a -- b" x"--" --y"#);
        assert_eq!(args.flag("note"), Some("a -- b"));
        assert_eq!(args.as_slice(), ["x--"]);
        assert!(args.has_flag("y"));
        // A multibyte value in quotes after a multibyte name.
        assert_eq!(flags("—ü=\u{201C}ä ö\u{201D}"), [flag("ü", Some("ä ö"))]);
    }

    #[test]
    fn the_first_word_splits_off_for_a_subcommand() {
        let args = Args::parse("status 42 --all");
        let (word, rest) = args.split_first().unwrap();
        assert_eq!(word, "status");
        assert_eq!(rest, " 42 --all");
        // A quoted first word is no subcommand.
        assert!(Args::parse(r#""status" 42"#).split_first().is_none());
        assert!(Args::parse(r#"st"atus" 42"#).split_first().is_none());
        assert!(Args::parse("").split_first().is_none());
        // A multibyte first word and its rest.
        let args = Args::parse("état  ü");
        let (word, rest) = args.split_first().unwrap();
        assert_eq!((word.as_str(), Args::parse(rest).raw()), ("état", "ü"));
        let args = Args::parse("only");
        let (word, rest) = args.split_first().unwrap();
        assert_eq!(word, "only");
        assert_eq!(Args::parse(rest), Args::default());
    }
}
