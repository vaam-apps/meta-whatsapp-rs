//! [`Middleware`]: code that runs around every event, before the command
//! match, in registration order. Each gets the context and [`Next`]; not
//! calling `next.run(ctx)` stops the event there (no command, no listener).
//! A banned sender's message never reaches them: the bot drops it first.
//!
//! Shipped: [`Logging`] (event kind, message type, outcome and duration,
//! never content or identities) and [`MarkRead`] (read receipt, optionally
//! with a typing indicator).

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use meta_whatsapp_core::Result;

use crate::bot::Router;
use crate::ctx::Ctx;

/// Runs around every event. See the [module docs](self).
///
/// ```
/// use async_trait::async_trait;
/// use meta_whatsapp_bot::{Ctx, Middleware, Next};
///
/// /// Ignores everything outside business hours.
/// #[derive(Debug)]
/// struct OfficeHours;
///
/// #[async_trait]
/// impl Middleware for OfficeHours {
///     async fn handle(&self, ctx: Ctx, next: Next<'_>) -> meta_whatsapp_core::Result<()> {
///         let open = true; // your check
///         if open { next.run(ctx).await } else { Ok(()) }
///     }
/// }
/// ```
#[async_trait]
pub trait Middleware: Send + Sync + fmt::Debug + 'static {
    /// Handle `ctx`; call `next.run(ctx)` to go on, or return without it
    /// to stop the event here.
    async fn handle(&self, ctx: Ctx, next: Next<'_>) -> Result<()>;
}

/// The rest of the chain: the middleware after this one, then the command
/// match and the handler or listeners.
pub struct Next<'a> {
    rest: &'a [Arc<dyn Middleware>],
    router: &'a Router,
}

impl fmt::Debug for Next<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Next")
            .field("middleware_left", &self.rest.len())
            .finish_non_exhaustive()
    }
}

impl<'a> Next<'a> {
    pub(crate) fn new(rest: &'a [Arc<dyn Middleware>], router: &'a Router) -> Self {
        Self { rest, router }
    }

    /// Run the rest of the chain.
    pub async fn run(self, ctx: Ctx) -> Result<()> {
        match self.rest.split_first() {
            Some((first, rest)) => first.handle(ctx, Next::new(rest, self.router)).await,
            None => self.router.dispatch(ctx).await,
        }
    }
}

/// Logs each event at `debug` when it is handled and at `warn` when it
/// fails: the event kind, the message type, the milliseconds taken and the
/// error kind. Never the message's content, the sender or their number,
/// nor the error's text (an integrator's error may quote anything).
#[derive(Debug, Clone, Copy, Default)]
pub struct Logging;

#[async_trait]
impl Middleware for Logging {
    async fn handle(&self, ctx: Ctx, next: Next<'_>) -> Result<()> {
        let event = ctx.event().kind();
        let message_type = ctx
            .message()
            .and_then(|m| m.message_type())
            .map(str::to_owned);
        let started = Instant::now();
        let result = next.run(ctx).await;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match &result {
            Ok(()) => tracing::debug!(event, message_type, elapsed_ms, "bot event handled"),
            Err(e) => tracing::warn!(
                event,
                message_type,
                elapsed_ms,
                error_kind = ?e.kind(),
                may_have_been_sent = e.may_have_been_sent(),
                "bot event failed"
            ),
        }
        result
    }
}

/// Marks every received message read before the rest of the chain runs,
/// through the bot's `Outbound` (the client's `mark_read`).
///
/// [`MarkRead::with_typing_indicator`] also shows "typing…" until the
/// reply or 25 seconds (Meta: only when you are about to reply; it shows
/// for every message, commands or not). When that call fails, a plain read
/// receipt is tried instead. A failed receipt is logged (kind only) and
/// never stops the event.
#[derive(Debug, Clone, Copy, Default)]
pub struct MarkRead {
    typing_indicator: bool,
}

impl MarkRead {
    /// Read receipts only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read receipts with a typing indicator.
    pub fn with_typing_indicator() -> Self {
        Self {
            typing_indicator: true,
        }
    }
}

#[async_trait]
impl Middleware for MarkRead {
    async fn handle(&self, ctx: Ctx, next: Next<'_>) -> Result<()> {
        if let (Some(from), Some(message)) = (ctx.phone_number_id(), ctx.message()) {
            let outbound = ctx.outbound();
            let mut result = outbound
                .mark_read(from, &message.id, self.typing_indicator)
                .await;
            if result.is_err() && self.typing_indicator {
                result = outbound.mark_read(from, &message.id, false).await;
            }
            if let Err(e) = result {
                tracing::warn!(error_kind = ?e.kind(), "bot could not mark a message read");
            }
        }
        next.run(ctx).await
    }
}
