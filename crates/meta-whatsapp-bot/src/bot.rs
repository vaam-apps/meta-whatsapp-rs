//! [`Bot`] and [`BotBuilder`]: the registry, the dispatch and the
//! `EventSink` a webhook handler delivers to.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

use crate::command::{Args, Command, CommandHandler, CommandInfo, Invocation, Scope, Trigger};
use crate::ctx::{BotSender, Ctx};
use crate::errors::{ErrorHandler, LogErrors};
use crate::guard::{
    AccessList, AccessPolicy, CooldownKey, CooldownOutcome, Cooldowns, KvCooldowns, Refusal,
    Refusals, ReplyRefusals,
};
use crate::help::{Catalog, CategoryHelp, HelpFormatter, HelpSection};
use crate::markdown::{MarkdownRenderer, Renderer, TEXT_MAX_CHARS, split};
use crate::middleware::{Middleware, Next};
use crate::outbound::{ClientOutbound, Outbound};
use crate::parse::{CommandParser, ParsedCommand, PrefixParser};
use crate::plugin::{DEFAULT_CATEGORY, Listen, Plugin, Registered, Registrar};

/// What the match found in a received message.
enum Found {
    Command(Invocation),
    Unknown(ParsedCommand),
}

/// The ban check and the match (before the middleware), then the guards
/// and the handler, the unknown-command handler, or the listeners (after
/// it).
pub(crate) struct Router {
    parser: Arc<dyn CommandParser>,
    catalog: Arc<Catalog>,
    /// Parallel to `catalog.commands`.
    handlers: Vec<Arc<dyn CommandHandler>>,
    by_name: HashMap<String, usize>,
    by_payload: HashMap<String, usize>,
    listeners: Vec<(Listen, Arc<dyn CommandHandler>)>,
    unknown: Option<Arc<dyn CommandHandler>>,
    access: Arc<dyn AccessPolicy>,
    cooldowns: Option<Arc<dyn Cooldowns>>,
    refusals: Arc<dyn Refusals>,
    /// Whether image and video captions are read as commands.
    captions: bool,
}

impl Router {
    /// Whether `ctx` is a received message from a banned sender, told to
    /// the [`Refusals`] if so. Checked first, so a banned sender gets
    /// nothing: no match, no read receipt, no typing indicator, no
    /// integrator middleware, no command, no listener.
    async fn refuse_banned(&self, ctx: &Ctx) -> Result<bool> {
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

    /// Before the middleware: the command a received message invokes
    /// (`ctx.invocation()`), or the unknown name the parser read
    /// (`ctx.unknown_command()`).
    fn route(&self, mut ctx: Ctx) -> Ctx {
        if ctx.sender().is_none() {
            return ctx;
        }
        match self.find(&ctx) {
            Some(Found::Command(invocation)) => ctx.with_invocation(invocation),
            Some(Found::Unknown(parsed)) => {
                ctx.set_unknown(parsed);
                ctx
            }
            None => ctx,
        }
    }

    fn name(&self, index: usize) -> String {
        self.catalog.commands[index].name.clone()
    }

    /// Typed text or (unless turned off) an image or video caption the
    /// parser accepts (a registered name, else an unknown one), or a
    /// tapped reply button, list row or template quick-reply button whose
    /// id is a registered payload.
    fn find(&self, ctx: &Ctx) -> Option<Found> {
        let payload = |id: &str| {
            let index = *self.by_payload.get(id)?;
            Some(Found::Command(Invocation::new(
                self.name(index),
                Trigger::Payload(id.to_owned()),
                Args::default(),
            )))
        };
        let typed = |text: &str, caption: bool| {
            let parsed = self.parser.parse(text)?;
            let Some(&index) = self.by_name.get(&parsed.name) else {
                return Some(Found::Unknown(parsed));
            };
            let (prefix, name) = (parsed.prefix, parsed.name);
            let trigger = if caption {
                Trigger::Caption { prefix, name }
            } else {
                Trigger::Text { prefix, name }
            };
            Some(Found::Command(Invocation::new(
                self.name(index),
                trigger,
                parsed.args,
            )))
        };
        match &ctx.message()?.content {
            MessageContent::Text(text) => typed(&text.body, false),
            MessageContent::Image(media) | MessageContent::Video(media) if self.captions => {
                typed(media.caption.as_deref()?, true)
            }
            MessageContent::Interactive(InteractiveReply::ButtonReply(reply)) => payload(&reply.id),
            MessageContent::Interactive(InteractiveReply::ListReply(reply)) => payload(&reply.id),
            MessageContent::Button(button) => payload(button.payload.as_deref()?),
            _ => None,
        }
    }

    /// After the middleware: the invoked command's guards and handler, the
    /// unknown-command handler, or the listeners.
    pub(crate) async fn dispatch(&self, ctx: Ctx) -> Result<()> {
        if let Some(invocation) = ctx.invocation() {
            let Some(&index) = self.by_name.get(&invocation.command) else {
                return Err(ConfigError::new(
                    "the context's invocation names no registered command",
                )
                .into());
            };
            return self.run(index, ctx).await;
        }
        if ctx.unknown_command().is_some()
            && let Some(handler) = &self.unknown
        {
            return handler.call(ctx).await;
        }
        self.listen(ctx).await
    }

    /// The command's guards, in order, then its handler.
    async fn run(&self, index: usize, ctx: Ctx) -> Result<()> {
        let info = &self.catalog.commands[index];
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
                ctx.sender().and_then(BotSender::key),
                ctx.phone_number_id(),
            ) else {
                // `build` refuses a cooldown without a store, and the match
                // needs a sender: reached only by a context a middleware
                // gave an invocation of its own.
                return Err(ConfigError::new(
                    "a command cooldown needs a cooldown store, a sender and a business number",
                )
                .into());
            };
            let key = CooldownKey::new(number, &info.name, user);
            if let CooldownOutcome::CoolingDown {
                retry_after,
                notify,
            } = cooldowns.try_start(&key, period).await?
            {
                return self
                    .refusals
                    .refused(
                        &ctx,
                        &Refusal::CoolingDown {
                            retry_after,
                            notify,
                        },
                    )
                    .await;
            }
        }
        self.handlers[index].call(ctx).await
    }

    /// Every listener for this event, in registration order; they all run,
    /// and the first error is returned.
    async fn listen(&self, ctx: Ctx) -> Result<()> {
        let message_type = ctx.message().map(|m| m.message_type());
        let kind = ctx.event().kind();
        let mut first_error = None;
        for (on, handler) in &self.listeners {
            let wanted = match on {
                Listen::Messages => message_type.is_some(),
                Listen::MessageType(t) => message_type.flatten() == Some(t.as_str()),
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

struct Inner {
    middleware: Vec<Arc<dyn Middleware>>,
    router: Router,
    outbound: Arc<dyn Outbound>,
    renderer: Arc<dyn MarkdownRenderer>,
    errors: Arc<dyn ErrorHandler>,
    help: Arc<dyn HelpFormatter>,
    plugins: Vec<(PluginInfo, Arc<dyn Plugin>)>,
    unloaded: AtomicBool,
}

/// A WhatsApp bot over Cloud API webhooks: ban check → command match →
/// middleware → the command's guards and handler, or listeners. See the
/// [crate docs](crate).
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
            .field("commands", &self.inner.router.catalog.commands.len())
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

    /// Handle one event, in this order:
    ///
    /// 1. a received message from a banned sender stops here (told to the
    ///    [`Refusals`] as [`Refusal::Banned`]);
    /// 2. the command match: `ctx.invocation()`, or
    ///    `ctx.unknown_command()` for a name no command has;
    /// 3. the middleware chain (any may stop the event);
    /// 4. the command's guards (scope, owner, cooldown) and its handler,
    ///    else the unknown-command handler, else the listeners.
    ///
    /// An error goes to the bot's [`ErrorHandler`] (by default logged and
    /// acknowledged). After [`Self::unload`], every event fails with
    /// `SinkError::Closed`, so Meta redelivers it to an instance still
    /// running.
    pub async fn handle(&self, event: WebhookEvent) -> Result<()> {
        let inner = &self.inner;
        if self.is_unloaded() {
            return Err(SinkError::Closed.into());
        }
        let ctx = Ctx::new(event, inner.outbound.clone(), inner.renderer.clone())
            .with_catalog(Arc::clone(&inner.router.catalog));
        match inner.router.refuse_banned(&ctx).await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => return inner.errors.on_error(&ctx, error).await,
        }
        let ctx = inner.router.route(ctx);
        match Next::new(&inner.middleware, &inner.router)
            .run(ctx.clone())
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => inner.errors.on_error(&ctx, error).await,
        }
    }

    fn is_unloaded(&self) -> bool {
        self.inner.unloaded.load(Ordering::Acquire)
    }

    /// Every registered command, hidden ones included, in registration
    /// order (also `Ctx::commands`, inside a handler).
    pub fn commands(&self) -> Vec<CommandInfo> {
        self.inner.router.catalog.commands.clone()
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
    /// categories first appear (also `Ctx::help_sections`).
    pub fn help_sections(&self) -> Vec<HelpSection> {
        crate::help::sections(&self.inner.router.catalog.commands)
    }

    /// The help text the bot's help command sends: formatted by its
    /// [`HelpFormatter`] (the first help command's, if several; else
    /// [`CategoryHelp`]) with the parser's help prefix.
    pub fn help(&self) -> String {
        self.inner.help.format(
            &self.help_sections(),
            self.inner.router.parser.help_prefix(),
        )
    }

    /// The commands for Meta's command menu: every command neither hidden
    /// nor taken out with `Command::menu(false)`, named without its prefix
    /// (Meta's menu sends `/name`, so the parser must accept `/`).
    ///
    /// Fails with the client's own checks
    /// (`ConversationalAutomationConfig::validate`: at most 30 commands,
    /// names of 1–32 characters, unique, descriptions of 1–256), when a
    /// listed command has no description (hide such commands from the menu
    /// rather than have them cut silently), and when the bot's parser does
    /// not read `/name` as the command (a tap would be plain text).
    pub fn command_menu(&self) -> std::result::Result<Vec<BotCommand>, ValidationError> {
        let router = &self.inner.router;
        let mut commands = Vec::new();
        for (index, info) in router.catalog.commands.iter().enumerate() {
            if info.hidden || !info.in_menu {
                continue;
            }
            let Some(description) = &info.description else {
                return Err(ValidationError::new(
                    format!("commands.{}.description", info.name),
                    "a command in Meta's menu needs a description (or `Command::menu(false)`)",
                ));
            };
            let tapped = router
                .parser
                .parse(&format!("/{}", info.name))
                .and_then(|parsed| router.by_name.get(&parsed.name).copied());
            if tapped != Some(index) {
                return Err(ValidationError::new(
                    format!("commands.{}.command_name", info.name),
                    "a tap in Meta's menu sends `/name`, which the bot's parser does not \
                     read as this command (keep `/` among the prefixes)",
                ));
            }
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
    /// first error is returned after all ran. The bot then refuses events
    /// (`SinkError::Closed`).
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
    /// [`Bot::handle`]. After [`Bot::unload`]: `SinkError::Closed`. An
    /// error the [`ErrorHandler`] returns becomes `SinkError::Delivery`
    /// (the webhook answers `500`, Meta redelivers).
    async fn deliver(&self, event: WebhookEvent) -> std::result::Result<(), SinkError> {
        if self.is_unloaded() {
            return Err(SinkError::Closed);
        }
        self.handle(event)
            .await
            .map_err(|e| SinkError::Delivery(anyhow::Error::new(e)))
    }
}

/// A registration, kept in call order so middleware run in the order the
/// builder and the plugins registered them.
enum Step {
    Command(Command),
    Middleware(Arc<dyn Middleware>),
    Listen(Listen, Arc<dyn CommandHandler>),
    Plugin(Arc<dyn Plugin>),
    Help {
        name: String,
        description: String,
        formatter: Arc<dyn HelpFormatter>,
    },
}

/// Builds a [`Bot`]. Only the [`Outbound`] (or a [`Client`]) is required;
/// the rest has a default, each behind the trait named on its setter.
#[must_use]
pub struct BotBuilder {
    outbound: Option<Arc<dyn Outbound>>,
    parser: Arc<dyn CommandParser>,
    access: Arc<dyn AccessPolicy>,
    cooldowns: Option<Arc<dyn Cooldowns>>,
    refusals: Arc<dyn Refusals>,
    errors: Arc<dyn ErrorHandler>,
    renderer: Arc<dyn MarkdownRenderer>,
    default_category: String,
    unknown: Option<Arc<dyn CommandHandler>>,
    captions: bool,
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
    /// The defaults: [`PrefixParser`] with `/` (names case-insensitive),
    /// an empty [`AccessList`], no cooldown store, [`ReplyRefusals`],
    /// [`LogErrors`], the default [`Renderer`], [`DEFAULT_CATEGORY`], no
    /// unknown-command handler, commands read from captions too.
    pub fn new() -> Self {
        Self {
            outbound: None,
            parser: Arc::new(PrefixParser::default()),
            access: Arc::new(AccessList::new()),
            cooldowns: None,
            refusals: Arc::new(ReplyRefusals),
            errors: Arc::new(LogErrors),
            renderer: Arc::new(Renderer::default()),
            default_category: DEFAULT_CATEGORY.to_owned(),
            unknown: None,
            captions: true,
            steps: Vec::new(),
        }
    }

    /// Send through `outbound` ([`Outbound`]).
    pub fn outbound(mut self, outbound: impl Outbound) -> Self {
        self.outbound = Some(Arc::new(outbound));
        self
    }

    /// Send through an outbound shared with other bots or code.
    pub fn shared_outbound(mut self, outbound: Arc<dyn Outbound>) -> Self {
        self.outbound = Some(outbound);
        self
    }

    /// Send with `client` ([`ClientOutbound`]).
    pub fn client(self, client: Client) -> Self {
        self.outbound(ClientOutbound::new(client))
    }

    /// Read commands with `parser` ([`CommandParser`]).
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

    /// Owners and banned users ([`AccessPolicy`]).
    pub fn access(mut self, access: impl AccessPolicy) -> Self {
        self.access = Arc::new(access);
        self
    }

    /// Where cooldowns are kept ([`Cooldowns`]; required when a command
    /// has one).
    pub fn cooldowns(mut self, cooldowns: impl Cooldowns) -> Self {
        self.cooldowns = Some(Arc::new(cooldowns));
        self
    }

    /// Keep cooldowns in `kv` ([`KvCooldowns`]).
    pub fn cooldown_store(self, kv: Arc<dyn KvStore>, clock: Arc<dyn Clock>) -> Self {
        self.cooldowns(KvCooldowns::new(kv, clock))
    }

    /// What refused users are told ([`Refusals`]).
    pub fn refusals(mut self, refusals: impl Refusals) -> Self {
        self.refusals = Arc::new(refusals);
        self
    }

    /// What a failure becomes ([`ErrorHandler`]).
    pub fn errors(mut self, errors: impl ErrorHandler) -> Self {
        self.errors = Arc::new(errors);
        self
    }

    /// How `Ctx::reply_markdown` renders ([`MarkdownRenderer`]; a
    /// [`Renderer`] with other options, or rules of your own).
    pub fn markdown(mut self, renderer: impl MarkdownRenderer) -> Self {
        self.renderer = Arc::new(renderer);
        self
    }

    /// The help section of commands registered without a category, by the
    /// builder or by a plugin without its own (default:
    /// [`DEFAULT_CATEGORY`]). The help commands go in it too.
    pub fn default_category(mut self, category: impl Into<String>) -> Self {
        self.default_category = category.into();
        self
    }

    /// Register a command (help section: its own category, else the
    /// default category).
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
    pub fn listen(mut self, on: Listen, handler: impl CommandHandler) -> Self {
        self.steps.push(Step::Listen(on, Arc::new(handler)));
        self
    }

    /// Run `handler` for a text (or caption) the parser reads as a command
    /// whose name no command has, instead of the listeners;
    /// `ctx.unknown_command()` holds what was typed. Without one, such a
    /// message goes to the listeners like any text.
    pub fn unknown_command(mut self, handler: impl CommandHandler) -> Self {
        self.unknown = Some(Arc::new(handler));
        self
    }

    /// Whether the caption of an image or a video is read as a command,
    /// like a text (default: `true`). With `false`, a captioned `/name` is
    /// a plain media message: the listeners get it, never a command or the
    /// unknown-command handler.
    pub fn commands_from_captions(mut self, read: bool) -> Self {
        self.captions = read;
        self
    }

    /// Add a plugin; its `setup` runs in `build`, in order.
    pub fn plugin(mut self, plugin: impl Plugin) -> Self {
        self.steps.push(Step::Plugin(Arc::new(plugin)));
        self
    }

    /// Add a `help` command ("Show the commands", in the default category)
    /// that replies with [`Bot::help`] in the [`CategoryHelp`] format,
    /// split into messages when long.
    pub fn help_command(self) -> Self {
        self.help_command_with("help", "Show the commands", CategoryHelp)
    }

    /// Add a help command named `name`, described as `description` (the
    /// help text and Meta's menu show it), in the default category, that
    /// replies with the visible commands formatted by `formatter`, split
    /// into messages when long.
    pub fn help_command_with(
        mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        formatter: impl HelpFormatter,
    ) -> Self {
        self.steps.push(Step::Help {
            name: name.into(),
            description: description.into(),
            formatter: Arc::new(formatter),
        });
        self
    }

    /// Run the plugins' `setup` and check the registrations: a name, alias
    /// or payload registered twice, an empty or blank one, a zero cooldown,
    /// a cooldown without a store, a listener for an event kind that is
    /// not in `WebhookEvent::KINDS` or for a blank message type, two
    /// plugins of one name, or no outbound is a `ConfigError`; a failed
    /// `setup` is its error in the step `"plugin_setup"`.
    pub async fn build(self) -> Result<Bot> {
        let outbound = self
            .outbound
            .ok_or_else(|| ConfigError::new("a bot needs an outbound (`BotBuilder::client`)"))?;
        let default_category = self.default_category;
        let mut registrar = Registrar::new(&default_category, None, false);
        let mut plugins: Vec<(PluginInfo, Arc<dyn Plugin>)> = Vec::new();
        let mut help: Option<Arc<dyn HelpFormatter>> = None;
        for step in self.steps {
            match step {
                Step::Command(command) => {
                    registrar.command(command);
                }
                Step::Middleware(middleware) => registrar.middleware.push(middleware),
                Step::Listen(on, handler) => registrar.listeners.push((on, handler)),
                Step::Help {
                    name,
                    description,
                    formatter,
                } => {
                    help.get_or_insert_with(|| Arc::clone(&formatter));
                    registrar.command(
                        Command::new(name, move |ctx: Ctx| {
                            let text =
                                formatter.format(&ctx.help_sections(), &ctx.catalog.help_prefix);
                            async move {
                                ctx.reply_parts(split(&text, TEXT_MAX_CHARS))
                                    .await
                                    .map(drop)
                            }
                        })
                        .description(description)
                        .category(default_category.clone()),
                    );
                }
                Step::Plugin(plugin) => {
                    let info = PluginInfo {
                        name: plugin.name().to_owned(),
                        category: plugin.category().unwrap_or(&default_category).to_owned(),
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
        check_listeners(&registrar.listeners)?;
        let mut router = router(
            registrar.commands,
            registrar.listeners,
            self.unknown,
            self.parser,
            self.access,
            self.cooldowns,
            self.refusals,
        )?;
        router.captions = self.captions;
        Ok(Bot {
            inner: Arc::new(Inner {
                middleware: registrar.middleware,
                router,
                outbound,
                renderer: self.renderer,
                errors: self.errors,
                help: help.unwrap_or_else(|| Arc::new(CategoryHelp)),
                plugins,
                unloaded: AtomicBool::new(false),
            }),
        })
    }
}

/// A listener for a kind no event has, or for a blank message type, would
/// never run: refused at build.
fn check_listeners(listeners: &[(Listen, Arc<dyn CommandHandler>)]) -> Result<()> {
    for (on, _) in listeners {
        match on {
            Listen::Event(kind) if !WebhookEvent::KINDS.contains(&kind.as_str()) => {
                return Err(ConfigError::new(format!(
                    "a listener for `{kind}`, which is not a webhook event kind \
                     (`WebhookEvent::KINDS`)"
                ))
                .into());
            }
            Listen::MessageType(message_type) if message_type.trim().is_empty() => {
                return Err(ConfigError::new("a listener for a blank message type").into());
            }
            _ => {}
        }
    }
    Ok(())
}

/// A name or alias as matched: in the parser's form; refused when blank or
/// holding whitespace.
fn command_word(
    parser: &dyn CommandParser,
    word: &str,
    what: &str,
    command: &str,
) -> Result<String> {
    let normalized = parser.normalize(word);
    if word.is_empty()
        || word.chars().any(char::is_whitespace)
        || normalized.is_empty()
        || normalized.chars().any(char::is_whitespace)
    {
        return Err(ConfigError::new(format!(
            "command `{command}`: {what} `{word}` is empty or holds whitespace"
        ))
        .into());
    }
    Ok(normalized)
}

fn router(
    commands: Vec<Registered>,
    listeners: Vec<(Listen, Arc<dyn CommandHandler>)>,
    unknown: Option<Arc<dyn CommandHandler>>,
    parser: Arc<dyn CommandParser>,
    access: Arc<dyn AccessPolicy>,
    cooldowns: Option<Arc<dyn Cooldowns>>,
    refusals: Arc<dyn Refusals>,
) -> Result<Router> {
    let mut infos = Vec::with_capacity(commands.len());
    let mut handlers = Vec::with_capacity(commands.len());
    let mut by_name = HashMap::new();
    let mut by_payload = HashMap::new();
    for registered in commands {
        let command = registered.command;
        let index = infos.len();
        let name = command_word(parser.as_ref(), &command.name, "the name", &command.name)?;
        let mut aliases = Vec::new();
        for word in std::iter::once(&command.name).chain(&command.aliases) {
            let word = command_word(parser.as_ref(), word, "a name or alias", &command.name)?;
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
        infos.push(CommandInfo {
            name,
            aliases,
            description: command.description,
            usage: command.usage,
            category: registered.category,
            plugin: registered.plugin,
            hidden: command.hidden || registered.plugin_hidden,
            in_menu: command.in_menu,
            scope: command.scope,
            owner_only: command.owner_only,
            cooldown: command.cooldown,
            payloads: command.payloads,
            metadata: command.metadata,
        });
        handlers.push(command.handler);
    }
    let catalog = Arc::new(Catalog {
        commands: infos,
        help_prefix: parser.help_prefix().to_owned(),
    });
    Ok(Router {
        parser,
        catalog,
        handlers,
        by_name,
        by_payload,
        listeners,
        unknown,
        access,
        cooldowns,
        refusals,
        captions: true,
    })
}
