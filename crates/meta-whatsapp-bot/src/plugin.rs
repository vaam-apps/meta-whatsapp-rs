//! Plugins: one feature per unit, registered at compile time.
//!
//! A [`Plugin`] names itself, says which help section (`category`) its
//! commands go in, and registers its commands, middleware and listeners in
//! [`Plugin::setup`] through a [`Registrar`]. `Bot::builder().plugin(p)`
//! adds it; `build()` runs every `setup` in order.
//!
//! # No hot reload
//!
//! Plugins are Rust values compiled into your binary. There is no loading
//! of plugins at run time (no `dlopen` of a shared library, no scripting):
//! Rust has no stable ABI, so a dynamically loaded plugin must be built by
//! the exact same compiler and dependency versions or it is undefined
//! behaviour, and loading code at run time is exactly what an attacker who
//! can write to disk wants. Ship a plugin as a crate instead (depend on it,
//! call `.plugin(MyPlugin::new(..))`), and redeploy to change it.
//! [`Plugin::on_unload`] runs at shutdown (`Bot::unload`), not per reload.

use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_core::Result;

use crate::command::{Command, Handler};
use crate::middleware::Middleware;

/// The help section of commands no plugin (or a plugin without its own)
/// registered.
pub const DEFAULT_CATEGORY: &str = "General";

/// One feature of the bot. See the [module docs](self).
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Unique name.
    fn name(&self) -> &str;

    /// Help section of its commands (a command may override it).
    fn category(&self) -> &str {
        DEFAULT_CATEGORY
    }

    /// What it does.
    // `&str`, not `&'static str`: an implementor may return its own field.
    #[allow(clippy::unnecessary_literal_bound)]
    fn description(&self) -> &str {
        ""
    }

    /// Leave its commands out of the help text and the command menu.
    fn hidden(&self) -> bool {
        false
    }

    /// Register commands, middleware and listeners. An error fails
    /// `BotBuilder::build`.
    async fn setup(&self, registrar: &mut Registrar) -> Result<()>;

    /// Release what `setup` acquired; called by `Bot::unload`.
    async fn on_unload(&self) -> Result<()> {
        Ok(())
    }
}

/// Which events a listener gets. Listeners run only when no command
/// handled the event: the bot runs "a command, or listeners".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Listen {
    /// Received messages that are not a (known) command, from senders who
    /// are not banned.
    Messages,
    /// Events of this kind (`WebhookEvent::kind`, e.g. `"status_updated"`)
    /// that no command handled.
    Event(String),
    /// Every event no command handled.
    All,
}

impl Listen {
    /// Events of `kind`.
    pub fn event(kind: impl Into<String>) -> Self {
        Self::Event(kind.into())
    }
}

/// A command as registered: with its section and plugin.
#[derive(Debug, Clone)]
pub(crate) struct Registered {
    pub(crate) command: Command,
    pub(crate) category: String,
    pub(crate) plugin: Option<String>,
    pub(crate) plugin_hidden: bool,
}

/// What a plugin's [`Plugin::setup`] registers into.
#[derive(Debug)]
pub struct Registrar {
    pub(crate) commands: Vec<Registered>,
    pub(crate) middleware: Vec<Arc<dyn Middleware>>,
    pub(crate) listeners: Vec<(Listen, Arc<dyn Handler>)>,
    category: String,
    plugin: Option<String>,
    hidden: bool,
}

impl Registrar {
    pub(crate) fn new(category: &str, plugin: Option<&str>, hidden: bool) -> Self {
        Self {
            commands: Vec::new(),
            middleware: Vec::new(),
            listeners: Vec::new(),
            category: category.to_owned(),
            plugin: plugin.map(str::to_owned),
            hidden,
        }
    }

    /// Register a command.
    pub fn command(&mut self, command: Command) -> &mut Self {
        let category = command
            .category
            .clone()
            .unwrap_or_else(|| self.category.clone());
        self.commands.push(Registered {
            command,
            category,
            plugin: self.plugin.clone(),
            plugin_hidden: self.hidden,
        });
        self
    }

    /// Register a middleware, after the ones registered before it.
    pub fn middleware(&mut self, middleware: impl Middleware) -> &mut Self {
        self.middleware.push(Arc::new(middleware));
        self
    }

    /// Register a listener.
    pub fn listen(&mut self, on: Listen, handler: impl Handler) -> &mut Self {
        self.listeners.push((on, Arc::new(handler)));
        self
    }

    /// The plugin registering (`None` for the builder).
    pub fn plugin(&self) -> Option<&str> {
        self.plugin.as_deref()
    }

    /// The help section commands go in unless they name another.
    pub fn category(&self) -> &str {
        &self.category
    }

    pub(crate) fn absorb(&mut self, other: Self) {
        self.commands.extend(other.commands);
        self.middleware.extend(other.middleware);
        self.listeners.extend(other.listeners);
    }
}
