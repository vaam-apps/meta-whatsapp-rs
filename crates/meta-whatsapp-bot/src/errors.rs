//! [`ErrorHandler`]: what happens when a middleware, command or listener
//! fails.
//!
//! The bot is a webhook sink: an error it returns makes the endpoint answer
//! non-`200`, and Meta redelivers the whole batch, running every command in
//! it again (a reply sent before the failure goes out twice). So the
//! default, [`LogErrors`], logs the failure and acknowledges the event;
//! [`PropagateErrors`] opts into redelivery.

use std::fmt;

use async_trait::async_trait;
use meta_whatsapp_core::{Error, Result};

use crate::ctx::Ctx;

/// Decides what a failed event becomes.
#[async_trait]
pub trait ErrorHandler: Send + Sync + fmt::Debug + 'static {
    /// `error` ended the handling of `ctx` (its invocation is set when a
    /// command had matched). `Ok` acknowledges the event; `Err` makes the
    /// bot's `deliver` fail, so Meta redelivers the batch.
    async fn on_error(&self, ctx: &Ctx, error: Error) -> Result<()>;
}

/// The default [`ErrorHandler`]: logs at `error` the event kind, the
/// command, the error kind, whether a send may have gone out and the Graph
/// error code, never the error's text (an integrator's error may quote
/// the message), then acknowledges the event.
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
/// idempotent.
#[derive(Debug, Clone, Copy, Default)]
pub struct PropagateErrors;

#[async_trait]
impl ErrorHandler for PropagateErrors {
    async fn on_error(&self, _: &Ctx, error: Error) -> Result<()> {
        Err(error)
    }
}
