//! [`ErrorHandler`]: what happens when a middleware, command or listener
//! fails.
//!
//! The bot is a webhook sink: an error it returns makes the endpoint answer
//! non-`200`, and Meta redelivers the whole batch, running every command in
//! it again (a reply sent before the failure goes out twice). So the
//! default, [`LogErrors`], logs the failure and acknowledges the event;
//! [`PropagateErrors`] opts into redelivery.
//!
//! The trade-off of the default: a transient failure (the cooldown store
//! or an async `AccessPolicy` unreachable, a Graph 5xx on a reply) is
//! acknowledged too, so that event is lost but for the log line. The
//! library's answer to such losses is a dead-letter store, decided for the
//! whole webhook path (`OPEN_QUESTIONS.md` #30, roadmap item L21) and not
//! built yet; until then an `ErrorHandler` of your own can keep the event
//! (the context carries it) and return `Ok`.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_core::{Error, Result};

use crate::ctx::Ctx;

/// Decides what a failed event becomes.
#[async_trait]
pub trait ErrorHandler: Send + Sync + fmt::Debug + 'static {
    /// `error` ended the handling of `ctx`: the context as the bot built
    /// it, after the ban check and the command match, before the
    /// middleware. Its event, sender, [`Ctx::invocation`] and
    /// [`Ctx::unknown_command`] are set; values a middleware inserted
    /// ([`Ctx::insert`]) are not there. `Ok` acknowledges the event; `Err`
    /// makes the bot's `deliver` fail, so Meta redelivers the batch.
    async fn on_error(&self, ctx: &Ctx, error: Error) -> Result<()>;
}

#[async_trait]
impl<T: ErrorHandler + ?Sized> ErrorHandler for Arc<T> {
    async fn on_error(&self, ctx: &Ctx, error: Error) -> Result<()> {
        (**self).on_error(ctx, error).await
    }
}

/// The default [`ErrorHandler`]: logs at `error` the event kind, the
/// command, the error kind, whether a send may have gone out and the Graph
/// error code, never the error's text (an integrator's error may quote
/// the message), then acknowledges the event (see the
/// [module docs](self) for the trade-off).
#[derive(Debug, Clone, Copy, Default)]
pub struct LogErrors;

#[async_trait]
impl ErrorHandler for LogErrors {
    async fn on_error(&self, ctx: &Ctx, error: Error) -> Result<()> {
        tracing::error!(
            event = ctx.event().kind(),
            command = ctx.invocation().map(|i| i.command.as_str()),
            error_kind = ?error.kind(),
            may_have_been_sent = error.may_have_been_sent(),
            graph_code = error.graph().map(|g| g.code),
            "bot handler failed; the event is acknowledged"
        );
        Ok(())
    }
}

/// An [`ErrorHandler`] that returns every error, so the webhook answers
/// `500` and Meta redelivers the batch. Only for handlers that are
/// idempotent. A command with a cooldown started it before it failed: its
/// redelivery inside the period is refused as `CoolingDown` (the user
/// gets one notice, the handler does not run again).
#[derive(Debug, Clone, Copy, Default)]
pub struct PropagateErrors;

#[async_trait]
impl ErrorHandler for PropagateErrors {
    async fn on_error(&self, _: &Ctx, error: Error) -> Result<()> {
        Err(error)
    }
}
