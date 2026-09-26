//! The guards a command passes before its handler runs, each behind a trait:
//!
//! 1. banned senders ([`AccessPolicy::is_banned`]): nothing runs for them,
//!    no command and no listener;
//! 2. the command's [`Scope`](crate::Scope) (private or group only);
//! 3. owner-only commands ([`AccessPolicy::is_owner`]);
//! 4. the per-user cooldown ([`Cooldowns`]), checked last so a refused
//!    attempt does not start one.
//!
//! A refused command goes to [`Refusals`], which decides what (if anything)
//! the user is told.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::clock::Clock;
use meta_whatsapp_core::ids::{PhoneNumberId, UserId};
use meta_whatsapp_core::store::{Expiry, KvStore, StoreKey};
use sha2::{Digest, Sha256};

use crate::ctx::{Ctx, Sender};

/// Who owns the bot and who is banned from it.
///
/// Async so an integrator can read it from a database. The default is
/// [`AccessList`] (a fixed list; empty: no owners, nobody banned).
#[async_trait]
pub trait AccessPolicy: Send + Sync + fmt::Debug + 'static {
    /// Whether `sender` may run owner-only commands.
    async fn is_owner(&self, sender: &Sender) -> Result<bool>;
    /// Whether the bot ignores `sender` altogether.
    async fn is_banned(&self, sender: &Sender) -> Result<bool>;
}

/// A set of users, by BSUID or by phone number.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Identities {
    users: HashSet<UserId>,
    /// Digits only.
    phones: HashSet<String>,
}

impl Identities {
    fn contains(&self, sender: &Sender) -> bool {
        let user = |id: &Option<UserId>| id.as_ref().is_some_and(|u| self.users.contains(u));
        user(&sender.user_id)
            || user(&sender.parent_user_id)
            || sender
                .wa_id
                .as_ref()
                .is_some_and(|wa| self.phones.contains(&digits(wa.as_str())))
    }
}

fn digits(phone: &str) -> String {
    phone.chars().filter(char::is_ascii_digit).collect()
}

/// [`AccessPolicy`] from configuration: owners and banned users listed by
/// BSUID (matched against the sender's `user_id` and `parent_user_id`) or
/// by phone number (matched against `wa_id`, digits compared).
///
/// **List BSUIDs.** A user who adopted a username may arrive without a
/// phone number, and then a phone-only entry does not match them: a ban by
/// phone number alone can be walked around.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use]
pub struct AccessList {
    owners: Identities,
    banned: Identities,
}

impl AccessList {
    /// Nobody is an owner, nobody is banned.
    pub fn new() -> Self {
        Self::default()
    }

    /// An owner, by BSUID.
    pub fn owner(mut self, user_id: impl Into<UserId>) -> Self {
        self.owners.users.insert(user_id.into());
        self
    }

    /// An owner, by phone number (E.164; only its digits are compared).
    pub fn owner_phone(mut self, phone: &str) -> Self {
        self.owners.phones.insert(digits(phone));
        self
    }

    /// A banned user, by BSUID.
    pub fn ban(mut self, user_id: impl Into<UserId>) -> Self {
        self.banned.users.insert(user_id.into());
        self
    }

    /// A banned user, by phone number (see the type docs for why a BSUID
    /// is better).
    pub fn ban_phone(mut self, phone: &str) -> Self {
        self.banned.phones.insert(digits(phone));
        self
    }
}

#[async_trait]
impl AccessPolicy for AccessList {
    async fn is_owner(&self, sender: &Sender) -> Result<bool> {
        Ok(self.owners.contains(sender))
    }

    async fn is_banned(&self, sender: &Sender) -> Result<bool> {
        Ok(self.banned.contains(sender))
    }
}

/// Store namespace of [`KvCooldowns`]. Changing it forgets the running
/// cooldowns (they last one period at most), nothing else.
pub const COOLDOWN_NAMESPACE: &str = "bot.cooldown";

/// Whose cooldown, for which command, on which business number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CooldownKey<'a> {
    /// The business phone number the command arrived on.
    pub phone_number_id: &'a PhoneNumberId,
    /// The command's name.
    pub command: &'a str,
    /// The user's key ([`Sender::key`]: BSUID first).
    pub user: &'a str,
}

impl<'a> CooldownKey<'a> {
    /// A key.
    pub fn new(phone_number_id: &'a PhoneNumberId, command: &'a str, user: &'a str) -> Self {
        Self {
            phone_number_id,
            command,
            user,
        }
    }

    /// The store key: SHA-256 (hex) of the three parts, each
    /// length-prefixed so none can be shifted into another. Hashed so no
    /// phone number is written to the store.
    pub fn store_key(&self) -> StoreKey {
        let mut hasher = Sha256::new();
        for part in [self.phone_number_id.as_str(), self.command, self.user] {
            hasher.update(format!("{}:", part.len()));
            hasher.update(part);
            hasher.update(",");
        }
        StoreKey::new(COOLDOWN_NAMESPACE, hex::encode(hasher.finalize()))
    }
}

/// The answer of [`Cooldowns::try_start`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CooldownOutcome {
    /// No cooldown was running; one has started.
    Started,
    /// A cooldown is running; try again after this long.
    CoolingDown {
        /// Time left.
        retry_after: Duration,
    },
}

/// Per-user, per-command cooldowns.
///
/// The default is [`KvCooldowns`], a typed store on the `KvStore` port.
#[async_trait]
pub trait Cooldowns: Send + Sync + fmt::Debug + 'static {
    /// Start a cooldown of `period` for `key` unless one is running,
    /// atomically (two concurrent calls never both start one).
    async fn try_start(&self, key: &CooldownKey<'_>, period: Duration) -> Result<CooldownOutcome>;
}

/// [`Cooldowns`] on a [`KvStore`]: one record per key in
/// [`COOLDOWN_NAMESPACE`], created with `put_if_absent` and left to expire
/// after the period. Shared by every instance using the same store.
#[derive(Debug, Clone)]
pub struct KvCooldowns {
    kv: Arc<dyn KvStore>,
    clock: Arc<dyn Clock>,
}

impl KvCooldowns {
    /// Cooldowns in `kv`; `clock` only computes the time left.
    pub fn new(kv: Arc<dyn KvStore>, clock: Arc<dyn Clock>) -> Self {
        Self { kv, clock }
    }
}

#[async_trait]
impl Cooldowns for KvCooldowns {
    async fn try_start(&self, key: &CooldownKey<'_>, period: Duration) -> Result<CooldownOutcome> {
        let store_key = key.store_key();
        // Twice at most: a record that expired between the refused create
        // and the read is gone, and the second create takes its place.
        for _ in 0..2 {
            if self
                .kv
                .put_if_absent(&store_key, b"1".to_vec(), Expiry::After(period))
                .await?
                .is_some()
            {
                return Ok(CooldownOutcome::Started);
            }
            if let Some(record) = self.kv.get(&store_key).await? {
                let retry_after = record.expires_at.map_or(period, |at| {
                    Duration::try_from(at - self.clock.now())
                        .unwrap_or(Duration::ZERO)
                        .min(period)
                });
                return Ok(CooldownOutcome::CoolingDown { retry_after });
            }
        }
        Ok(CooldownOutcome::CoolingDown {
            retry_after: period,
        })
    }
}

/// Why a command did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The sender is banned ([`AccessPolicy::is_banned`]).
    Banned,
    /// An owner-only command, and the sender is not an owner.
    NotOwner,
    /// A group-only command in a private chat.
    GroupOnly,
    /// A private-only command in a group.
    PrivateOnly,
    /// The command's cooldown is running for this user.
    CoolingDown {
        /// Time left.
        retry_after: Duration,
    },
}

/// What the user is told when a command is refused.
#[async_trait]
pub trait Refusals: Send + Sync + fmt::Debug + 'static {
    /// `refusal` happened for `ctx` (its invocation is set, except for
    /// [`Refusal::Banned`], which is checked before any match).
    async fn refused(&self, ctx: &Ctx, refusal: &Refusal) -> Result<()>;
}

/// The default [`Refusals`]: a short English reply for a wrong chat or a
/// running cooldown; nothing for banned users or non-owners (the reply
/// would tell them the bot noticed, or that an owner command exists).
#[derive(Debug, Clone, Copy, Default)]
pub struct ReplyRefusals;

#[async_trait]
impl Refusals for ReplyRefusals {
    async fn refused(&self, ctx: &Ctx, refusal: &Refusal) -> Result<()> {
        let text = match refusal {
            Refusal::GroupOnly => "This command only works in a group.".to_owned(),
            Refusal::PrivateOnly => "This command only works in a private chat.".to_owned(),
            Refusal::CoolingDown { retry_after } => {
                // Whole seconds, rounded up: "0 s" would read as "now".
                let secs = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
                format!(
                    "Please wait {} s before using this command again.",
                    secs.max(1)
                )
            }
            Refusal::Banned | Refusal::NotOwner => return Ok(()),
        };
        ctx.reply(text).await.map(drop)
    }
}

/// [`Refusals`] that never answer.
#[derive(Debug, Clone, Copy, Default)]
pub struct SilentRefusals;

#[async_trait]
impl Refusals for SilentRefusals {
    async fn refused(&self, _: &Ctx, _: &Refusal) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sender(user: Option<&str>, parent: Option<&str>, wa: Option<&str>) -> Sender {
        Sender {
            user_id: user.map(UserId::new),
            parent_user_id: parent.map(UserId::new),
            wa_id: wa.map(Into::into),
            ..Sender::default()
        }
    }

    #[tokio::test]
    async fn the_access_list_matches_bsuids_and_phone_digits() {
        let list = AccessList::new()
            .owner("US.1")
            .owner_phone("+1 650 555 1234")
            .ban("US.ENT.9")
            .ban_phone("+44 20 7946 0000");
        assert!(
            list.is_owner(&sender(Some("US.1"), None, None))
                .await
                .unwrap()
        );
        assert!(
            list.is_owner(&sender(None, None, Some("16505551234")))
                .await
                .unwrap()
        );
        assert!(
            !list
                .is_owner(&sender(Some("US.2"), None, Some("1")))
                .await
                .unwrap()
        );
        // A parent BSUID matches too.
        assert!(
            list.is_banned(&sender(Some("US.3"), Some("US.ENT.9"), None))
                .await
                .unwrap()
        );
        assert!(
            list.is_banned(&sender(Some("US.4"), None, Some("442079460000")))
                .await
                .unwrap()
        );
        // A phone-only ban cannot match a sender without a phone number.
        assert!(
            !list
                .is_banned(&sender(Some("US.4"), None, None))
                .await
                .unwrap()
        );
        assert!(
            !AccessList::new()
                .is_owner(&sender(Some("US.1"), None, None))
                .await
                .unwrap()
        );
    }

    #[test]
    fn the_cooldown_key_is_pinned_and_hashed() {
        let pn = PhoneNumberId::new("106540352242922");
        let key = CooldownKey::new(&pn, "imagine", "16505551234").store_key();
        assert_eq!(key.namespace(), "bot.cooldown");
        assert_eq!(key.key().len(), 64);
        assert!(!key.key().contains("16505551234"));
        // Length prefixes: moving a character between parts changes the key.
        let shifted = CooldownKey::new(&pn, "imagin", "e16505551234").store_key();
        assert_ne!(key, shifted);
        assert_eq!(
            key.key(),
            hex::encode(Sha256::digest(
                "15:106540352242922,7:imagine,11:16505551234,"
            ))
        );
    }
}
