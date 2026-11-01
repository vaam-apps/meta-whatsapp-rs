//! [`ThroughputUpgrades`]: a number's pacer follows Meta's throughput
//! upgrade. Opt-in: nothing in this crate follows one unless you set it up.
//!
//! Meta upgrades a business number from 80 to 1,000 messages a second by
//! itself (`throughput`, "Higher throughput") and then sends a
//! `phone_number_quality_update` webhook whose `event` is
//! `THROUGHPUT_UPGRADE` (`PhoneNumberQualityEvent::ThroughputUpgrade`).
//! [`ThroughputUpgrades`] hears that webhook and raises the number's rate
//! in the pacer's [`RateLimiter`] to [`Rate::HIGHER_THROUGHPUT`]
//! ([`RateLimiter::raise_rate`]: in a [`TokenBucket`], in this process; in
//! a [`KvRateLimiter`], for every replica), so its next sends go 1 ms apart
//! instead of 12.5 ms. Wire it in with `BotBuilder::follow_throughput_upgrades` (a
//! listener of the bot), or deliver events to it yourself: it is an
//! `EventSink<WebhookEvent>` too, for a process with no bot (next to your
//! own sink in a `FanoutSink`).
//!
//! **Only `THROUGHPUT_UPGRADE` changes a rate.** `ONBOARDING`, `FLAGGED`
//! and any value Meta adds later (`PhoneNumberQualityEvent::Other`) leave
//! every rate as it is (logged at debug level). Meta documents no
//! downgrade event and no mapping from a throughput level to messages a
//! second, so none is assumed. An upgrade never lowers a rate: a number
//! already at or above the upgrade's rate keeps its own, and an upgrade
//! heard twice changes nothing the second time.
//!
//! **Which number.** The webhook names the number by its display phone
//! number and its WhatsApp Business Account (the entry's `id`), never by
//! its phone number id, which is what a pacer keys on. A [`NumberDirectory`]
//! says which id that is; the default, [`KnownNumbers`], holds the numbers
//! you register, each by its WABA *and* its display number (compared by
//! their digits: `+1 555-078-3881` is `15550783881`). An upgrade for a
//! number the directory does not know changes nothing (logged as a
//! warning). Only the upgraded number's rate changes: every other number,
//! of the same WABA or not, keeps its own.
//!
//! **Where the rate is kept.** With a [`TokenBucket`] the rate lives in
//! the bucket: a restart (or another replica, which did not get the
//! webhook) is back at the bucket's configured rate, and Meta sends the
//! webhook once per upgrade, not again after a restart. With a
//! [`KvRateLimiter`] it is in the number's record, for every replica on the
//! store, and survives restarts until the record expires
//! ([`KvRateLimiter::idle_expiry`]: 24 hours after the number's last slot
//! by default). To keep it longer, record the number
//! [`ThroughputUpgrades::observe`] returns in your own store and set it
//! when you build the limiter ([`TokenBucket::rate_for`],
//! [`KvRateLimiter::rate_for`]).
//!
//! **What else loses an upgrade**, since Meta does not send it again:
//!
//! - As a bot's listener it runs after the middleware, like any listener:
//!   a middleware that stops the event (one ignoring everything outside
//!   business hours, or everything but messages) stops the upgrade with
//!   it. Let events without a message through (`ctx.message()` is
//!   `None`), or deliver to a `ThroughputUpgrades` as a sink next to the
//!   bot instead.
//! - A [`NumberDirectory`] that fails changes no rate, nor does a limiter
//!   that fails to raise it (a [`KvRateLimiter`] whose store is down;
//!   nothing is raised then). As a sink, that is
//!   `SinkError::Delivery`: the webhook answers `500` and Meta redelivers,
//!   for up to 7 days (`throughput`, "Webhooks"). As a bot's listener the
//!   error goes to the bot's `ErrorHandler`, and the default, `LogErrors`,
//!   logs and acknowledges it: the upgrade is lost. `PropagateErrors` has
//!   it redelivered ([`ThroughputUpgrades::observe`] is safe to repeat).
//!   A directory answers `Ok(None)`, not an error, for a number that is
//!   not yours, or Meta redelivers it all that time.
//!
//! [`TokenBucket`]: crate::TokenBucket
//! [`TokenBucket::rate_for`]: crate::TokenBucket::rate_for
//! [`KvRateLimiter`]: crate::KvRateLimiter
//! [`KvRateLimiter::idle_expiry`]: crate::KvRateLimiter::idle_expiry
//! [`KvRateLimiter::rate_for`]: crate::KvRateLimiter::rate_for

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use meta_whatsapp_core::error::{ConfigError, SinkError};
use meta_whatsapp_core::ids::{PhoneNumberId, WabaId};
use meta_whatsapp_core::sink::EventSink;
use meta_whatsapp_core::{Error, Result};
use meta_whatsapp_webhooks::WebhookEvent;
use meta_whatsapp_webhooks::fields::PhoneNumberQualityEvent;

use crate::command::CommandHandler;
use crate::ctx::Ctx;
use crate::pacer::{Rate, RateLimiter};

/// The event kind a [`ThroughputUpgrades`] listens to
/// (`WebhookEvent::kind` of a `phone_number_quality_update`).
pub const QUALITY_UPDATE_KIND: &str = "phone_number_quality_updated";

/// Which business number a `phone_number_quality_update` webhook names.
///
/// The webhook carries the number's WhatsApp Business Account id and its
/// display phone number (`display_phone_number`, digits only in Meta's
/// example), not its phone number id. The default is [`KnownNumbers`], the
/// numbers you register; implement this to look them up in your own store
/// (or with the client's phone numbers of a WABA, whose
/// `PhoneNumberInfo::display_phone_number` may be formatted:
/// `+1 631-555-5555`).
#[async_trait]
pub trait NumberDirectory: Send + Sync + fmt::Debug + 'static {
    /// The phone number id of `waba_id`'s number displayed as
    /// `display_phone_number`, or `None` when it is not one of yours. An
    /// error is for a lookup that could not be made (a store down): the
    /// event fails, see the [module docs](self).
    async fn phone_number_id(
        &self,
        waba_id: &WabaId,
        display_phone_number: &str,
    ) -> Result<Option<PhoneNumberId>>;
}

#[async_trait]
impl<T: NumberDirectory + ?Sized> NumberDirectory for Arc<T> {
    async fn phone_number_id(
        &self,
        waba_id: &WabaId,
        display_phone_number: &str,
    ) -> Result<Option<PhoneNumberId>> {
        (**self)
            .phone_number_id(waba_id, display_phone_number)
            .await
    }
}

/// The default [`NumberDirectory`]: the numbers you register, each by its
/// WABA and its display phone number, compared by their ASCII digits only
/// (`+1 555-078-3881`, `1 (555) 078-3881` and `15550783881` are one
/// number). Register it with its country code, as Meta's webhook shows
/// it: digits are compared whole, so `555-078-3881` is not `15550783881`.
/// A display number is matched only under the WABA it was registered
/// with. Its `Debug` shows how many numbers it holds, not which.
///
/// Share it behind an `Arc` to add a number while running
/// ([`KnownNumbers::insert`], a merchant onboarded meanwhile).
#[derive(Default)]
#[must_use]
pub struct KnownNumbers {
    numbers: RwLock<HashMap<(WabaId, String), PhoneNumberId>>,
}

impl fmt::Debug for KnownNumbers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self
            .numbers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        f.debug_struct("KnownNumbers")
            .field("numbers", &count)
            .finish()
    }
}

/// The digits of a display phone number, which is how two of them compare.
fn digits(display_phone_number: &str) -> String {
    display_phone_number
        .chars()
        .filter(char::is_ascii_digit)
        .collect()
}

impl KnownNumbers {
    /// No numbers.
    pub fn new() -> Self {
        Self::default()
    }

    /// With `phone_number_id`, displayed as `display_phone_number` in
    /// `waba_id`. A display number without a digit is a `ConfigError`.
    pub fn number(
        self,
        waba_id: WabaId,
        display_phone_number: &str,
        phone_number_id: PhoneNumberId,
    ) -> Result<Self> {
        self.insert(waba_id, display_phone_number, phone_number_id)?;
        Ok(self)
    }

    /// Add (or replace) a number while in use: [`Self::number`] on a
    /// shared directory.
    pub fn insert(
        &self,
        waba_id: WabaId,
        display_phone_number: &str,
        phone_number_id: PhoneNumberId,
    ) -> Result<()> {
        let digits = digits(display_phone_number);
        if digits.is_empty() {
            return Err(ConfigError::new("a display phone number without a digit").into());
        }
        self.numbers
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((waba_id, digits), phone_number_id);
        Ok(())
    }
}

#[async_trait]
impl NumberDirectory for KnownNumbers {
    async fn phone_number_id(
        &self,
        waba_id: &WabaId,
        display_phone_number: &str,
    ) -> Result<Option<PhoneNumberId>> {
        let key = (waba_id.clone(), digits(display_phone_number));
        Ok(self
            .numbers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned())
    }
}

/// Raises a number's rate in a [`RateLimiter`] ([`RateLimiter::raise_rate`])
/// when Meta's `phone_number_quality_update` says it was upgraded to higher
/// throughput. See the [module docs](self).
///
/// Give it the limiter your [`crate::Pacer`] paces with (a [`TokenBucket`]
/// or a [`KvRateLimiter`] kept in an `Arc`, the pacer built with
/// `Pacer::shared`): an upgrade of a limiter no pacer uses changes nothing
/// anyone sends. A limiter without rates of its own (the trait's default
/// `raise_rate`) raises nothing.
///
/// [`TokenBucket`]: crate::TokenBucket
/// [`KvRateLimiter`]: crate::KvRateLimiter
#[derive(Debug, Clone)]
pub struct ThroughputUpgrades {
    limiter: Arc<dyn RateLimiter>,
    numbers: Arc<dyn NumberDirectory>,
    rate: Rate,
}

impl ThroughputUpgrades {
    /// Follow upgrades of the numbers `numbers` knows, into `limiter` (an
    /// `Arc<TokenBucket>` or an `Arc<KvRateLimiter>`), at
    /// [`Rate::HIGHER_THROUGHPUT`].
    pub fn new<L: RateLimiter>(limiter: Arc<L>, numbers: impl NumberDirectory) -> Self {
        Self::shared(limiter, Arc::new(numbers))
    }

    /// [`Self::new`] with a limiter and a directory shared with other code
    /// (an `Arc<KnownNumbers>` you keep, to [`KnownNumbers::insert`] while
    /// running).
    pub fn shared(limiter: Arc<dyn RateLimiter>, numbers: Arc<dyn NumberDirectory>) -> Self {
        Self {
            limiter,
            numbers,
            rate: Rate::HIGHER_THROUGHPUT,
        }
    }

    /// Raise an upgraded number to `rate` instead of
    /// [`Rate::HIGHER_THROUGHPUT`]: below it, to leave headroom for the
    /// inbound messages Meta counts in the same throughput
    /// (`Rate::per_second(800)`).
    #[must_use]
    pub fn rate(mut self, rate: Rate) -> Self {
        self.rate = rate;
        self
    }

    /// Hear one event. For a `phone_number_quality_update` whose `event`
    /// is `THROUGHPUT_UPGRADE`, of a number the directory knows and whose
    /// rate is below this one's: raise that number's rate
    /// ([`RateLimiter::raise_rate`]) and return its id. Anything else
    /// returns `None` and changes nothing. Fails only when the directory or
    /// the limiter does, and then changes nothing.
    pub async fn observe(&self, event: &WebhookEvent) -> Result<Option<PhoneNumberId>> {
        let WebhookEvent::PhoneNumberQualityUpdated {
            waba_id, update, ..
        } = event
        else {
            return Ok(None);
        };
        if update.event != PhoneNumberQualityEvent::ThroughputUpgrade {
            tracing::debug!(
                event = update.event.as_str(),
                "quality update is not a throughput upgrade: no rate changed"
            );
            return Ok(None);
        }
        let Some(number) = self
            .numbers
            .phone_number_id(waba_id, &update.display_phone_number)
            .await?
        else {
            tracing::warn!(
                "throughput upgrade of a number the directory does not know: no rate changed"
            );
            return Ok(None);
        };
        // Checked and set in one step: a rate raised higher between a
        // separate check and set would be lowered.
        if !self.limiter.raise_rate(&number, self.rate).await? {
            tracing::debug!(
                "throughput upgrade of a number already at its rate (or a limiter without rates)"
            );
            return Ok(None);
        }
        tracing::info!(rate = self.rate.get(), "pacer follows a throughput upgrade");
        Ok(Some(number))
    }
}

/// As a bot's listener (`BotBuilder::follow_throughput_upgrades`): a
/// failed directory or limiter goes to the bot's `ErrorHandler`.
#[async_trait]
impl CommandHandler for ThroughputUpgrades {
    async fn call(&self, ctx: Ctx) -> Result<()> {
        self.observe(ctx.event()).await.map(drop)
    }
}

/// Without a bot: [`ThroughputUpgrades::observe`]; a failed directory or
/// limiter is `SinkError::Delivery` (the webhook answers `500`, Meta
/// redelivers).
#[async_trait]
impl EventSink<WebhookEvent> for ThroughputUpgrades {
    async fn deliver(&self, event: WebhookEvent) -> std::result::Result<(), SinkError> {
        self.observe(&event)
            .await
            .map(drop)
            .map_err(|e: Error| SinkError::Delivery(anyhow::Error::new(e)))
    }
}
