//! Rate limits (docs/design/server.md, section 6): a token bucket per
//! tenant and route class, per replica, charged after the key, the tenant
//! and the scope (steps 1 to 3 of the authorization order) and before
//! ownership, the vault and the body: a limited request costs nothing
//! else. Past the limit: `429 too_many_requests`, `retryable`, with
//! `Retry-After` ([`Limited::error`]).
//!
//! A request is charged **by its operation** ([`RateLimiter::charge`],
//! with an [`Operation`]), never by its HTTP method: the class is the
//! operation's ([`RouteClass::of`]), so a read lands in the read budget
//! through any API adapter, whatever method that adapter serves it with
//! (roadmap S6). Nothing outside this crate charges a class directly.
//!
//! The bucket is the **tenant's**: a tenant key and a platform key acting
//! for the same tenant draw from one bucket, and one tenant exhausting
//! its budget leaves the others' untouched.
//!
//! | Class | Operations | Default (per tenant and replica) |
//! | --- | --- | --- |
//! | `send` | every write that is not template management: `send_message`, `mark_read`, `upload_media`, `delete_media`, `update_profile`, `disconnect_waba` | 20/s, burst 40 (the design's "sends") |
//! | `read` | every read but templates': `list_wabas`, `list_numbers`, `get_number`, `get_profile`, `download_media`, `list_events` | 50/s, burst 50 (the design's "reads"; it states no burst) |
//! | `templates` | every template operation: `list_templates`, `get_template`, `create_template`, `delete_templates` | 2/s, burst 2 (the design's "template management"; it states no burst) |
//!
//! The design names classes for sends, reads and template management; it
//! does not say where read receipts, media and profile writes belong: they
//! count as `send`, the writes that reach Meta. Its limits are "per tenant
//! and replica" (a deployment of N replicas allows N times as much): each
//! is `WA_SERVER_RATE_<CLASS>` and `…_BURST` (see the service's
//! `meta_whatsapp_server::config`), the same for every tenant. Per-tenant
//! overrides by the admin API are not implemented.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Duration, Instant};

use crate::error::ServiceError;
use crate::model::TenantId;
use crate::operation::Operation;

/// The classes of routes a tenant's budget is split into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteClass {
    /// Writes that reach Meta (sends, read receipts, media, profile).
    Send,
    /// Reads.
    Read,
    /// Template management.
    Templates,
}

impl RouteClass {
    /// Every class.
    pub const ALL: [Self; 3] = [Self::Send, Self::Read, Self::Templates];

    /// The class of `operation` (the module's table): its own, whatever
    /// HTTP method, or none, an adapter serves it with.
    pub fn of(operation: Operation) -> Self {
        match operation {
            Operation::ListWabas
            | Operation::ListNumbers
            | Operation::GetNumber
            | Operation::GetProfile
            | Operation::DownloadMedia
            | Operation::ListEvents => Self::Read,
            Operation::UpdateProfile
            | Operation::DisconnectWaba
            | Operation::SendMessage
            | Operation::MarkRead
            | Operation::UploadMedia
            | Operation::DeleteMedia => Self::Send,
            Operation::ListTemplates
            | Operation::CreateTemplate
            | Operation::DeleteTemplates
            | Operation::GetTemplate => Self::Templates,
        }
    }

    /// Its name, for logs and metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Read => "read",
            Self::Templates => "templates",
        }
    }
}

/// One class's limit: `per_second` tokens a second, at most `burst` saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    /// Sustained requests a second (at least 1).
    pub per_second: u32,
    /// Requests allowed at once after a quiet period (at least 1).
    pub burst: u32,
}

/// Every class's limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimits {
    /// [`RouteClass::Send`].
    pub send: Rate,
    /// [`RouteClass::Read`].
    pub read: Rate,
    /// [`RouteClass::Templates`].
    pub templates: Rate,
}

impl Default for RateLimits {
    /// The design's numbers (section 6), per tenant and replica: sends
    /// 20/s with a burst of 40, reads 50/s, template management 2/s; the
    /// design states no burst for the last two, which get one second's
    /// worth.
    fn default() -> Self {
        Self {
            send: Rate {
                per_second: 20,
                burst: 40,
            },
            read: Rate {
                per_second: 50,
                burst: 50,
            },
            templates: Rate {
                per_second: 2,
                burst: 2,
            },
        }
    }
}

impl RateLimits {
    /// The limit of `class`.
    pub fn of(&self, class: RouteClass) -> Rate {
        match class {
            RouteClass::Send => self.send,
            RouteClass::Read => self.read,
            RouteClass::Templates => self.templates,
        }
    }
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

/// The buckets of one replica. Memory: one bucket per tenant and class
/// used, for tenants that passed the authorization order (existing ones).
#[derive(Debug)]
pub struct RateLimiter {
    limits: RateLimits,
    buckets: Mutex<HashMap<(TenantId, RouteClass), Bucket>>,
}

impl RateLimiter {
    /// Buckets with `limits`.
    pub fn new(limits: RateLimits) -> Self {
        Self {
            limits,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// The limits.
    pub fn limits(&self) -> RateLimits {
        self.limits
    }

    /// Charge `tenant` one request of `operation`: a token from the
    /// bucket of its class ([`RouteClass::of`]), else [`Limited`] (`429
    /// too_many_requests` with `Retry-After`, [`Limited::error`]). The
    /// class charged is returned, for the adapter's metrics.
    ///
    /// # Errors
    ///
    /// The bucket is empty: [`Limited`], with the class and the wait.
    pub fn charge(&self, tenant: &TenantId, operation: Operation) -> Result<RouteClass, Limited> {
        let class = RouteClass::of(operation);
        match self.check(tenant, class) {
            Ok(()) => Ok(class),
            Err(wait) => Err(Limited { class, wait }),
        }
    }

    /// Take a token from `tenant`'s `class` bucket, or say how long until
    /// one is there. Crate-private: an adapter charges an operation
    /// ([`Self::charge`]), never a class of its choosing.
    pub(crate) fn check(&self, tenant: &TenantId, class: RouteClass) -> Result<(), Duration> {
        let rate = self.limits.of(class);
        let per_second = f64::from(rate.per_second.max(1));
        let burst = f64::from(rate.burst.max(1));
        let now = Instant::now();
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        let bucket = buckets.entry((tenant.clone(), class)).or_insert(Bucket {
            tokens: burst,
            updated: now,
        });
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * per_second).min(burst);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.tokens) / per_second))
        }
    }
}

/// A request past its tenant's budget ([`RateLimiter::charge`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limited {
    /// The class whose bucket is empty.
    pub class: RouteClass,
    /// How long until it holds a token again.
    pub wait: Duration,
}

impl Limited {
    /// `429 too_many_requests`, retryable, with `Retry-After` the wait in
    /// whole seconds ([`retry_after_secs`]).
    pub fn error(&self) -> ServiceError {
        ServiceError::too_many_requests(retry_after_secs(self.wait))
    }
}

/// Transfers a replica runs at once (media held in memory, streamed
/// downloads), and each tenant's share of them: a tenant holds at most
/// half of a pool (at least one), so one tenant's slow or stuck transfers
/// never take every slot of the replica (docs/design/server.md, section 6:
/// "bounded media concurrency"; the share is this service's default, the
/// design states none).
#[derive(Debug)]
pub struct Slots {
    total: Arc<Semaphore>,
    per_tenant: usize,
    tenants: Mutex<HashMap<TenantId, Arc<Semaphore>>>,
}

/// A slot of [`Slots`], its tenant's and the replica's, released when
/// dropped.
#[derive(Debug)]
pub struct Slot {
    _tenant: OwnedSemaphorePermit,
    _total: OwnedSemaphorePermit,
}

impl Slots {
    /// A pool of `total` slots (at least one), each tenant holding at most
    /// half of them (at least one).
    pub fn new(total: usize) -> Self {
        let total = total.max(1);
        Self {
            total: Arc::new(Semaphore::new(total)),
            per_tenant: (total / 2).max(1),
            tenants: Mutex::new(HashMap::new()),
        }
    }

    /// How many slots one tenant may hold.
    pub fn per_tenant(&self) -> usize {
        self.per_tenant
    }

    /// A slot for `tenant`, or `None` when its share or the pool is taken.
    pub fn try_acquire(&self, tenant: &TenantId) -> Option<Slot> {
        let share = self
            .tenants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(tenant.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(self.per_tenant)))
            .clone();
        let tenant = share.try_acquire_owned().ok()?;
        let total = self.total.clone().try_acquire_owned().ok()?;
        Some(Slot {
            _tenant: tenant,
            _total: total,
        })
    }
}

/// `Retry-After` for a wait: whole seconds, rounded up, at least 1.
pub fn retry_after_secs(wait: Duration) -> u64 {
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    secs.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(id: &str) -> TenantId {
        TenantId::parse(id).unwrap()
    }

    /// The design's numbers (section 6), whatever the constants say.
    #[test]
    fn the_defaults_are_the_designs() {
        let limits = RateLimits::default();
        assert_eq!(
            (limits.send.per_second, limits.send.burst),
            (20, 40),
            "sends 20/s, burst 40"
        );
        assert_eq!(limits.read.per_second, 50, "reads 50/s");
        assert_eq!(limits.templates.per_second, 2, "template management 2/s");
    }

    /// A burst, then one token per `1 / per_second`; each tenant and
    /// class has its own bucket.
    #[tokio::test(start_paused = true)]
    async fn buckets_refill_at_their_rate_per_tenant_and_class() {
        let limiter = RateLimiter::new(RateLimits {
            send: Rate {
                per_second: 2,
                burst: 3,
            },
            ..RateLimits::default()
        });
        let a = tenant("a");
        for _ in 0..3 {
            limiter.check(&a, RouteClass::Send).unwrap();
        }
        let wait = limiter.check(&a, RouteClass::Send).unwrap_err();
        assert_eq!(wait, Duration::from_millis(500));
        assert_eq!(retry_after_secs(wait), 1);
        // Another tenant, another class: untouched.
        limiter.check(&tenant("b"), RouteClass::Send).unwrap();
        limiter.check(&a, RouteClass::Read).unwrap();
        tokio::time::advance(Duration::from_millis(500)).await;
        limiter.check(&a, RouteClass::Send).unwrap();
        assert!(limiter.check(&a, RouteClass::Send).is_err());
        // Never more than the burst saved.
        tokio::time::advance(Duration::from_secs(60)).await;
        for _ in 0..3 {
            limiter.check(&a, RouteClass::Send).unwrap();
        }
        assert!(limiter.check(&a, RouteClass::Send).is_err());
    }

    /// A tenant holds half of a pool at most: another tenant still gets a
    /// slot. Decisive: the tenant's share.
    #[test]
    fn a_tenant_holds_its_share_of_the_slots() {
        let slots = Slots::new(4);
        assert_eq!(slots.per_tenant(), 2);
        let a = tenant("a");
        let held: Vec<Slot> = (0..2).map(|_| slots.try_acquire(&a).unwrap()).collect();
        assert!(slots.try_acquire(&a).is_none(), "a's share is taken");
        let b = slots.try_acquire(&tenant("b")).unwrap();
        let c = slots.try_acquire(&tenant("c")).unwrap();
        assert!(
            slots.try_acquire(&tenant("d")).is_none(),
            "the pool is taken"
        );
        drop(held);
        assert!(slots.try_acquire(&a).is_some(), "released on drop");
        drop((b, c));
        assert_eq!(Slots::new(1).per_tenant(), 1);
        assert_eq!(Slots::new(0).per_tenant(), 1);
    }

    /// Each operation's class, the module's table, written out: reads
    /// apart from writes, template management apart from both (its scope's
    /// operations, and only they). Decisive: each arm of `RouteClass::of`.
    #[test]
    fn an_operations_class_is_the_tables() {
        use crate::model::Scope;
        let table = [
            ("list_wabas", RouteClass::Read),
            ("list_numbers", RouteClass::Read),
            ("get_number", RouteClass::Read),
            ("get_profile", RouteClass::Read),
            ("update_profile", RouteClass::Send),
            ("disconnect_waba", RouteClass::Send),
            ("send_message", RouteClass::Send),
            ("mark_read", RouteClass::Send),
            ("upload_media", RouteClass::Send),
            ("download_media", RouteClass::Read),
            ("delete_media", RouteClass::Send),
            ("list_templates", RouteClass::Templates),
            ("create_template", RouteClass::Templates),
            ("delete_templates", RouteClass::Templates),
            ("get_template", RouteClass::Templates),
            ("list_events", RouteClass::Read),
        ];
        assert_eq!(table.len(), Operation::ALL.len(), "every operation");
        for (id, class) in table {
            let operation = Operation::from_id(id).unwrap();
            assert_eq!(RouteClass::of(operation), class, "{id}");
            assert_eq!(
                class == RouteClass::Templates,
                operation.scope() == Scope::Templates,
                "{id}"
            );
        }
    }

    /// The decisive case of roadmap S6: a read, charged by its operation's
    /// id, lands in the read budget whatever HTTP method an adapter serves
    /// it with (axum's `GET`, and the `HEAD` it answers on every `GET`
    /// route; an RPC-style adapter's `POST`): the method is no input of
    /// [`RateLimiter::charge`], so it cannot move the request to another
    /// budget. Spending the read budget leaves the send and template
    /// budgets whole, and a write does not spend the read budget.
    #[tokio::test(start_paused = true)]
    async fn a_read_charged_by_operation_id_lands_in_the_read_budget_whatever_its_method() {
        let reads: Vec<Operation> = Operation::ALL
            .into_iter()
            .filter(|operation| RouteClass::of(*operation) == RouteClass::Read)
            .collect();
        let served = ["GET", "HEAD", "POST"];
        let burst = u32::try_from(reads.len() * served.len()).unwrap();
        let one = Rate {
            per_second: 1,
            burst: 1,
        };
        let limiter = RateLimiter::new(RateLimits {
            send: one,
            read: Rate {
                per_second: 1,
                burst,
            },
            templates: one,
        });
        let a = tenant("a");
        // What an adapter hands the core: the operation's id, never the
        // method it served the request with.
        let charge = |_method: &str, id: &str| limiter.charge(&a, Operation::from_id(id).unwrap());
        for operation in &reads {
            for method in served {
                assert_eq!(
                    charge(method, operation.id()),
                    Ok(RouteClass::Read),
                    "{method} {operation}"
                );
            }
        }
        for operation in &reads {
            for method in served {
                let refused = charge(method, operation.id()).unwrap_err();
                assert_eq!(refused.class, RouteClass::Read, "{method} {operation}");
                assert_eq!(refused.wait, Duration::from_secs(1));
                let error = refused.error();
                assert_eq!((error.status(), error.code()), (429, "too_many_requests"));
                assert!(error.is_retryable());
                assert_eq!(error.retry_after(), Some(1));
            }
        }
        // The other budgets are whole: a write and a template operation
        // each take their own one token.
        assert_eq!(charge("GET", "send_message"), Ok(RouteClass::Send));
        assert_eq!(charge("GET", "list_templates"), Ok(RouteClass::Templates));
        assert_eq!(
            charge("POST", "send_message").unwrap_err().class,
            RouteClass::Send
        );
        // And a write spends none of the read budget once it refills.
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(charge("POST", "list_events"), Ok(RouteClass::Read));
        assert_eq!(charge("POST", "update_profile"), Ok(RouteClass::Send));
        assert_eq!(
            charge("GET", "list_events").unwrap_err().class,
            RouteClass::Read
        );
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        assert_eq!(retry_after_secs(Duration::ZERO), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_after_secs(Duration::from_secs(2)), 2);
        assert_eq!(retry_after_secs(Duration::from_millis(2001)), 3);
    }
}
