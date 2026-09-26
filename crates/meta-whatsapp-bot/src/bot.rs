//! [`Bot`] and [`BotBuilder`]: the registry, the dispatch and the
//! `EventSink` a webhook handler delivers to.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use meta_whatsapp_client::Client;
use meta_whatsapp_client::phone_numbers::{BotCommand, ConversationalAutomationConfig};
use meta_whatsapp_core::Result;
use meta_whatsapp_core::clock::Clock;
use meta_whatsapp_core::error::{ConfigError, SinkError, ValidationError};
use meta_whatsapp_core::ids::PhoneNumberId;
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::store::KvStore;
use meta_whatsapp_webhooks::WebhookEvent;
use meta_whatsapp_webhooks::fields::{InteractiveReply, MessageContent};

use crate::command::{Command, CommandInfo, Handler, Invocation, Scope, Trigger};
use crate::ctx::{Ctx, Sender};
use crate::errors::{ErrorHandler, LogErrors};
use crate::guard::{
    AccessList, AccessPolicy, CooldownKey, CooldownOutcome, Cooldowns, KvCooldowns, Refusal,
    Refusals, ReplyRefusals,
};
use crate::markdown::{Renderer, TEXT_MAX_CHARS, split};
use crate::middleware::{Middleware, Next};
use crate::outbound::{ClientOutbound, Outbound};
use crate::parse::{CommandParser, PrefixParser};
use crate::plugin::{DEFAULT_CATEGORY, Listen, Plugin, Registered, Registrar};

/// A registered command.
struct Entry {
    info: CommandInfo,
    handler: Arc<dyn Handler>,
}

/// Command match, guards, handler or listeners: what runs after the
/// middleware.
pub(crate) struct Router {
    parser: Arc<dyn CommandParser>,
    entries: Vec<Entry>,
    by_name: HashMap<String, usize>,
    by_payload: HashMap<String, usize>,
    listeners: Vec<(Listen, Arc<dyn Handler>)>,
    access: Arc<dyn AccessPolicy>,
    cooldowns: Option<Arc<dyn Cooldowns>>,
    refusals: Arc<dyn Refusals>,
}

impl Router {
    /// Whether `ctx` is a received message from a banned sender, told to
    /// the [`Refusals`] if so. Checked before the middleware chain, so a
    /// banned sender gets nothing: no read receipt, no typing indicator,
    /// no integrator middleware, no command, no listener.
    pub(crate) async fn refuse_banned(&self, ctx: &Ctx) -> Result<bool> {
        let (Some(_), Some(sender)) = (ctx.message(), ctx.sender()) else {
            return Ok(false);
        };
        if !self.access.is_banned(sender).await? {
            return Ok(false);
        }
        tracing::debug!(
            event = ctx.event().kind(),
            "bot dropped a message from a banned sender"
        );
        self.refusals.refused(ctx, &Refusal::Banned).await?;
        Ok(true)
    }

    /// After the middleware: the command a received message invokes, or
    /// the listeners.
    pub(crate) async fn dispatch(&self, ctx: Ctx) -> Result<()> {
        if ctx.sender().is_some()
            && let Some((index, invocation)) = self.find(&ctx)
        {
            ctx.invocation.set(invocation).ok();
            return self.run(&self.entries[index], ctx).await;
        }
        self.listen(ctx).await
    }

    /// The command a received message invokes: typed text the parser
    /// accepts and whose name is registered, or a tapped reply button,
    /// list row or template quick-reply button whose id is a registered
    /// payload.
    fn find(&self, ctx: &Ctx) -> Option<(usize, Invocation)> {
        let payload = |id: &str| {
            let index = *self.by_payload.get(id)?;
            Some((
                index,
                Invocation {
                    command: self.entries[index].info.name.clone(),
                    trigger: Trigger::Payload(id.to_owned()),
                    args: crate::command::Args::default(),
                },
            ))
        };
        match &ctx.message()?.content {
            MessageContent::Text(text) => {
                let parsed = self.parser.parse(&text.body)?;
                let name = parsed.name.to_lowercase();
                let index = *self.by_name.get(&name)?;
                Some((
                    index,
                    Invocation {
                        command: self.entries[index].info.name.clone(),
                        trigger: Trigger::Text {
                            prefix: parsed.prefix,
                            name,
                        },
                        args: parsed.args,
                    },
                ))
            }
            MessageContent::Interactive(InteractiveReply::ButtonReply(reply)) => payload(&reply.id),
            MessageContent::Interactive(InteractiveReply::ListReply(reply)) => payload(&reply.id),
            MessageContent::Button(button) => payload(button.payload.as_deref()?),
            _ => None,
        }
    }

    /// The guards, in order, then the handler.
    async fn run(&self, entry: &Entry, ctx: Ctx) -> Result<()> {
        let info = &entry.info;
        let group = ctx.chat().is_some_and(crate::Chat::is_group);
        let wrong_chat = match info.scope {
            Scope::GroupOnly if !group => Some(Refusal::GroupOnly),
            Scope::PrivateOnly if group => Some(Refusal::PrivateOnly),
            _ => None,
        };
        if let Some(refusal) = wrong_chat {
            return self.refusals.refused(&ctx, &refusal).await;
        }
        if info.owner_only {
            let owner = match ctx.sender() {
                Some(sender) => self.access.is_owner(sender).await?,
                None => false,
            };
            if !owner {
                return self.refusals.refused(&ctx, &Refusal::NotOwner).await;
            }
        }
        if let Some(period) = info.cooldown {
            let (Some(cooldowns), Some(user), Some(number)) = (
                self.cooldowns.as_ref(),
                ctx.sender().and_then(Sender::key),
                ctx.phone_number_id(),
            ) else {
                // `build` refuses a cooldown without a store, and a command
                // only matches a message with a sender: not reached.
                return Err(ConfigError::new("a command cooldown needs a cooldown store").into());
            };
            let key = CooldownKey::new(number, &info.name, user);
            if let CooldownOutcome::CoolingDown { retry_after } =
                cooldowns.try_start(&key, period).await?
            {
                return self
                    .refusals
                    .refused(&ctx, &Refusal::CoolingDown { retry_after })
                    .await;
            }
        }
        entry.handler.call(ctx).await
    }

    /// Every listener for this event, in registration order; they all run,
    /// and the first error is returned.
    async fn listen(&self, ctx: Ctx) -> Result<()> {
        let message = ctx.message().is_some();
        let kind = ctx.event().kind();
        let mut first_error = None;
        for (on, handler) in &self.listeners {
            let wanted = match on {
                Listen::Messages => message,
                Listen::Event(k) => k == kind,
                Listen::All => true,
            };
            if wanted && let Err(e) = handler.call(ctx.clone()).await {
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

/// A plugin's description, as registered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PluginInfo {
    /// Name.
    pub name: String,
    /// Help section of its commands.
    pub category: String,
    /// What it does.
    pub description: String,
    /// Whether its commands are left out of help and menu.
    pub hidden: bool,
}

/// One section of the help text: a category and its visible commands.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HelpSection {
    /// The category.
    pub category: String,
    /// Its commands that are not hidden, in registration order.
    pub commands: Vec<CommandInfo>,
}

struct Inner {
    middleware: Vec<Arc<dyn Middleware>>,
    router: Router,
    outbound: Arc<dyn Outbound>,
    renderer: Arc<Renderer>,
    errors: Arc<dyn ErrorHandler>,
    plugins: Vec<(PluginInfo, Arc<dyn Plugin>)>,
    unloaded: AtomicBool,
}

/// A WhatsApp bot over Cloud API webhooks: middleware → command match →
/// guards → handler, or listeners. See the [crate docs](crate).
///
/// It is an `EventSink<WebhookEvent>`: hand it to `WebhookHandler` like
/// any sink (behind a `DedupGuard`, so Meta's retries run nothing twice).
/// Cheap to clone.
#[derive(Clone)]
pub struct Bot {
    inner: Arc<Inner>,
}

impl fmt::Debug for Bot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Bot")
            .field("commands", &self.inner.router.entries.len())
            .field("middleware", &self.inner.middleware.len())
            .field("listeners", &self.inner.router.listeners.len())
            .field(
                "plugins",
                &self
                    .inner
                    .plugins
                    .iter()
                    .map(|(info, _)| info.name.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Bot {
    /// Start building a bot.
    pub fn builder() -> BotBuilder {
        BotBuilder::new()
    }

    /// Handle one event: a received message from a banned sender stops
    /// here (before any middleware); anything else goes through the
    /// middleware chain, then a command or the listeners. An error goes to
    /// the bot's [`ErrorHandler`] (by default logged and acknowledged).
    /// After [`Self::unload`], every event is an error (so Meta redelivers
    /// it to an instance still running).
    pub async fn handle(&self, event: WebhookEvent) -> Result<()> {
        let inner = &self.inner;
        if inner.unloaded.load(Ordering::Acquire) {
            return Err(ConfigError::new("the bot was unloaded").into());
        }
        let ctx = Ctx::new(event, inner.outbound.clone(), inner.renderer.clone());
        let result = match inner.router.refuse_banned(&ctx).await {
            Ok(true) => Ok(()),
            Ok(false) => {
                Next::new(&inner.middleware, &inner.router)
                    .run(ctx.clone())
                    .await
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) => inner.errors.on_error(&ctx, error).await,
        }
    }

    /// Every registered command, hidden ones included, in registration
    /// order.
    pub fn commands(&self) -> Vec<CommandInfo> {
        self.inner
            .router
            .entries
            .iter()
            .map(|e| e.info.clone())
            .collect()
    }

    /// The registered plugins, in registration order.
    pub fn plugins(&self) -> Vec<PluginInfo> {
        self.inner
            .plugins
            .iter()
            .map(|(info, _)| info.clone())
            .collect()
    }

    /// The commands that are not hidden, grouped by category in the order
    /// categories first appear.
    pub fn help_sections(&self) -> Vec<HelpSection> {
        help_sections(&self.inner.router.entries)
    }

    /// The help text: each category in bold, then one line per visible
    /// command (`/name, /alias — description`, with the parser's help
    /// prefix). Format your own from [`Self::help_sections`].
    pub fn help(&self) -> String {
        help_text(
            &self.inner.router.entries,
            self.inner.router.parser.help_prefix(),
        )
    }

    /// The commands for Meta's command menu: every command neither hidden
    /// nor taken out with `Command::menu(false)`, named without its prefix
    /// (Meta's menu sends `/name`, so the parser must accept `/`).
    ///
    /// Fails with the client's own checks
    /// (`ConversationalAutomationConfig::validate`: at most 30 commands,
    /// names of 1–32 characters, unique, descriptions of 1–256) and when a
    /// listed command has no description: hide such commands from the menu
    /// rather than have them cut silently.
    pub fn command_menu(&self) -> std::result::Result<Vec<BotCommand>, ValidationError> {
        let mut commands = Vec::new();
        for entry in &self.inner.router.entries {
            let info = &entry.info;
            if info.hidden || !info.in_menu {
                continue;
            }
            let Some(description) = &info.description else {
                return Err(ValidationError::new(
                    format!("commands.{}.description", info.name),
                    "a command in Meta's menu needs a description (or `Command::menu(false)`)",
                ));
            };
            commands.push(BotCommand::new(info.name.clone(), description.clone()));
        }
        ConversationalAutomationConfig::new()
            .commands(commands.clone())
            .validate()?;
        Ok(commands)
    }

    /// Send [`Self::command_menu`] as the `commands` of `phone_number_id`
    /// (`POST /{phone_number_id}/conversational_automation` with `commands`
    /// only: the welcome message and ice breakers are not sent).
    ///
    /// Meta's page (`business-phone-numbers/conversational-components`)
    /// calls it "a list of commands to be configured" and does not say
    /// whether a command left out is removed; this always sends the whole
    /// list. A menu tap arrives as the text `/name …`.
    pub async fn sync_command_menu(
        &self,
        client: &Client,
        phone_number_id: impl Into<PhoneNumberId>,
    ) -> Result<()> {
        let config = ConversationalAutomationConfig::new().commands(self.command_menu()?);
        client
            .phone_number(phone_number_id)
            .configure_conversational_automation(&config)
            .await
    }

    /// Run every plugin's `on_unload`, last registered first, once; the
    /// first error is returned after all ran. The bot then refuses events.
    pub async fn unload(&self) -> Result<()> {
        if self.inner.unloaded.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut first_error = None;
        for (info, plugin) in self.inner.plugins.iter().rev() {
            if let Err(e) = plugin.on_unload().await {
                tracing::warn!(plugin = info.name, error_kind = ?e.kind(), "plugin unload failed");
                first_error.get_or_insert(e);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

#[async_trait]
impl EventSink<WebhookEvent> for Bot {
    /// [`Bot::handle`]; an error the [`ErrorHandler`] returns becomes
    /// `SinkError::Delivery` (the webhook answers `500`, Meta redelivers).
    async fn deliver(&self, event: WebhookEvent) -> std::result::Result<(), SinkError> {
        self.handle(event)
            .await
            .map_err(|e| SinkError::Delivery(anyhow::Error::new(e)))
    }
}

fn help_sections(entries: &[Entry]) -> Vec<HelpSection> {
    let mut sections: Vec<HelpSection> = Vec::new();
    for entry in entries.iter().filter(|e| !e.info.hidden) {
        let info = entry.info.clone();
        match sections.iter_mut().find(|s| s.category == info.category) {
            Some(section) => section.commands.push(info),
            None => sections.push(HelpSection {
                category: info.category.clone(),
                commands: vec![info],
            }),
        }
    }
    sections
}

fn help_text(entries: &[Entry], prefix: &str) -> String {
    help_sections(entries)
        .iter()
        .map(|section| {
            let mut text = format!("*{}*", section.category);
            for command in &section.commands {
                text.push('\n');
                text.push_str(prefix);
                text.push_str(&command.name);
                for alias in &command.aliases {
                    text.push_str(", ");
                    text.push_str(prefix);
                    text.push_str(alias);
                }
                if let Some(description) = &command.description {
                    text.push_str(" — ");
                    text.push_str(description);
                }
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A registration, kept in call order so middleware run in the order the
/// builder and the plugins registered them.
enum Step {
    Command(Command),
    Middleware(Arc<dyn Middleware>),
    Listen(Listen, Arc<dyn Handler>),
    Plugin(Arc<dyn Plugin>),
    Help,
}

/// Builds a [`Bot`]. Every decision has a default and a trait to replace
/// it with; only the [`Outbound`] (or a [`Client`]) is required.
#[must_use]
pub struct BotBuilder {
    outbound: Option<Arc<dyn Outbound>>,
    parser: Arc<dyn CommandParser>,
    access: Arc<dyn AccessPolicy>,
    cooldowns: Option<Arc<dyn Cooldowns>>,
    refusals: Arc<dyn Refusals>,
    errors: Arc<dyn ErrorHandler>,
    renderer: Renderer,
    steps: Vec<Step>,
}

impl fmt::Debug for BotBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BotBuilder")
            .field("steps", &self.steps.len())
            .finish_non_exhaustive()
    }
}

impl Default for BotBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl BotBuilder {
    /// The defaults: [`PrefixParser`] with `/`, an empty [`AccessList`], no
    /// cooldown store, [`ReplyRefusals`], [`LogErrors`], the default
    /// [`Renderer`].
    pub fn new() -> Self {
        Self {
            outbound: None,
            parser: Arc::new(PrefixParser::default()),
            access: Arc::new(AccessList::new()),
            cooldowns: None,
            refusals: Arc::new(ReplyRefusals),
            errors: Arc::new(LogErrors),
            renderer: Renderer::default(),
            steps: Vec::new(),
        }
    }

    /// Send through `outbound`.
    pub fn outbound(mut self, outbound: impl Outbound) -> Self {
        self.outbound = Some(Arc::new(outbound));
        self
    }

    /// Send with `client` ([`ClientOutbound`]).
    pub fn client(self, client: Client) -> Self {
        self.outbound(ClientOutbound::new(client))
    }

    /// Read commands with `parser`.
    pub fn parser(mut self, parser: impl CommandParser) -> Self {
        self.parser = Arc::new(parser);
        self
    }

    /// Read commands with a [`PrefixParser`] over `prefixes`.
    pub fn prefixes<I, S>(self, prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.parser(PrefixParser::new(prefixes))
    }

    /// Owners and banned users.
    pub fn access(mut self, access: impl AccessPolicy) -> Self {
        self.access = Arc::new(access);
        self
    }

    /// Where cooldowns are kept (required when a command has one).
    pub fn cooldowns(mut self, cooldowns: impl Cooldowns) -> Self {
        self.cooldowns = Some(Arc::new(cooldowns));
        self
    }

    /// Keep cooldowns in `kv` ([`KvCooldowns`]).
    pub fn cooldown_store(self, kv: Arc<dyn KvStore>, clock: Arc<dyn Clock>) -> Self {
        self.cooldowns(KvCooldowns::new(kv, clock))
    }

    /// What refused users are told.
    pub fn refusals(mut self, refusals: impl Refusals) -> Self {
        self.refusals = Arc::new(refusals);
        self
    }

    /// What a failure becomes.
    pub fn errors(mut self, errors: impl ErrorHandler) -> Self {
        self.errors = Arc::new(errors);
        self
    }

    /// How `Ctx::reply_markdown` renders.
    pub fn markdown(mut self, renderer: Renderer) -> Self {
        self.renderer = renderer;
        self
    }

    /// Register a command (help section: its own category, else
    /// [`DEFAULT_CATEGORY`]).
    pub fn command(mut self, command: Command) -> Self {
        self.steps.push(Step::Command(command));
        self
    }

    /// Register a middleware, after those registered before it (by the
    /// builder or by a plugin added before it).
    pub fn middleware(mut self, middleware: impl Middleware) -> Self {
        self.steps.push(Step::Middleware(Arc::new(middleware)));
        self
    }

    /// Register a listener.
    pub fn listen(mut self, on: Listen, handler: impl Handler) -> Self {
        self.steps.push(Step::Listen(on, Arc::new(handler)));
        self
    }

    /// Add a plugin; its `setup` runs in `build`, in order.
    pub fn plugin(mut self, plugin: impl Plugin) -> Self {
        self.steps.push(Step::Plugin(Arc::new(plugin)));
        self
    }

    /// Add a `help` command ([`DEFAULT_CATEGORY`]) that replies with
    /// [`Bot::help`], split into messages when long.
    pub fn help_command(mut self) -> Self {
        self.steps.push(Step::Help);
        self
    }

    /// Run the plugins' `setup` and check the registrations: a name, alias
    /// or payload registered twice, an empty or blank one, a zero cooldown,
    /// a cooldown without a store, two plugins of one name, or no outbound
    /// is a `ConfigError`; a failed `setup` is its error in the step
    /// `"plugin_setup"`.
    pub async fn build(self) -> Result<Bot> {
        let outbound = self
            .outbound
            .ok_or_else(|| ConfigError::new("a bot needs an outbound (`BotBuilder::client`)"))?;
        let help = Arc::new(OnceLock::<String>::new());
        let mut registrar = Registrar::new(DEFAULT_CATEGORY, None, false);
        let mut plugins: Vec<(PluginInfo, Arc<dyn Plugin>)> = Vec::new();
        for step in self.steps {
            match step {
                Step::Command(command) => {
                    registrar.command(command);
                }
                Step::Middleware(middleware) => registrar.middleware.push(middleware),
                Step::Listen(on, handler) => registrar.listeners.push((on, handler)),
                Step::Help => {
                    let text = Arc::clone(&help);
                    registrar.command(
                        Command::new("help", move |ctx: Ctx| {
                            let text = text.get().cloned().unwrap_or_default();
                            async move {
                                ctx.reply_parts(split(&text, TEXT_MAX_CHARS))
                                    .await
                                    .map(drop)
                            }
                        })
                        .description("Show the commands")
                        .category(DEFAULT_CATEGORY),
                    );
                }
                Step::Plugin(plugin) => {
                    let info = PluginInfo {
                        name: plugin.name().to_owned(),
                        category: plugin.category().to_owned(),
                        description: plugin.description().to_owned(),
                        hidden: plugin.hidden(),
                    };
                    if plugins.iter().any(|(p, _)| p.name == info.name) {
                        return Err(ConfigError::new(format!(
                            "two plugins are named `{}`",
                            info.name
                        ))
                        .into());
                    }
                    let mut own = Registrar::new(&info.category, Some(&info.name), info.hidden);
                    if let Err(e) = plugin.setup(&mut own).await {
                        tracing::error!(plugin = info.name, error_kind = ?e.kind(), "plugin setup failed");
                        return Err(e.in_step("plugin_setup"));
                    }
                    registrar.absorb(own);
                    plugins.push((info, plugin));
                }
            }
        }
        let router = router(
            registrar.commands,
            registrar.listeners,
            self.parser,
            self.access,
            self.cooldowns,
            self.refusals,
        )?;
        help.set(help_text(&router.entries, router.parser.help_prefix()))
            .ok();
        Ok(Bot {
            inner: Arc::new(Inner {
                middleware: registrar.middleware,
                router,
                outbound,
                renderer: Arc::new(self.renderer),
                errors: self.errors,
                plugins,
                unloaded: AtomicBool::new(false),
            }),
        })
    }
}

/// A name or alias as matched: lowercased; refused when blank or holding
/// whitespace.
fn command_word(word: &str, what: &str, command: &str) -> Result<String> {
    if word.is_empty() || word.chars().any(char::is_whitespace) {
        return Err(ConfigError::new(format!(
            "command `{command}`: {what} `{word}` is empty or holds whitespace"
        ))
        .into());
    }
    Ok(word.to_lowercase())
}

fn router(
    commands: Vec<Registered>,
    listeners: Vec<(Listen, Arc<dyn Handler>)>,
    parser: Arc<dyn CommandParser>,
    access: Arc<dyn AccessPolicy>,
    cooldowns: Option<Arc<dyn Cooldowns>>,
    refusals: Arc<dyn Refusals>,
) -> Result<Router> {
    let mut entries = Vec::with_capacity(commands.len());
    let mut by_name = HashMap::new();
    let mut by_payload = HashMap::new();
    for registered in commands {
        let command = registered.command;
        let index = entries.len();
        let name = command_word(&command.name, "the name", &command.name)?;
        let mut aliases = Vec::new();
        for word in std::iter::once(&command.name).chain(&command.aliases) {
            let word = command_word(word, "a name or alias", &command.name)?;
            if by_name.insert(word.clone(), index).is_some() {
                return Err(
                    ConfigError::new(format!("the command word `{word}` is taken twice")).into(),
                );
            }
            if word != name {
                aliases.push(word);
            }
        }
        for payload in &command.payloads {
            if payload.trim().is_empty() {
                return Err(ConfigError::new(format!("command `{name}`: an empty payload")).into());
            }
            if by_payload.insert(payload.clone(), index).is_some() {
                return Err(
                    ConfigError::new(format!("the payload `{payload}` is taken twice")).into(),
                );
            }
        }
        if let Some(period) = command.cooldown {
            if period.is_zero() {
                return Err(ConfigError::new(format!("command `{name}`: a zero cooldown")).into());
            }
            if cooldowns.is_none() {
                return Err(ConfigError::new(format!(
                    "command `{name}` has a cooldown but the bot has no cooldown store \
                     (`BotBuilder::cooldowns`)"
                ))
                .into());
            }
        }
        entries.push(Entry {
            info: CommandInfo {
                name,
                aliases,
                description: command.description,
                category: registered.category,
                plugin: registered.plugin,
                hidden: command.hidden || registered.plugin_hidden,
                in_menu: command.in_menu,
                scope: command.scope,
                owner_only: command.owner_only,
                cooldown: command.cooldown,
                payloads: command.payloads,
            },
            handler: command.handler,
        });
    }
    Ok(Router {
        parser,
        entries,
        by_name,
        by_payload,
        listeners,
        access,
        cooldowns,
        refusals,
    })
}
