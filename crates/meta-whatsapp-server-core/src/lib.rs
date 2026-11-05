//! `meta-whatsapp-server-core`: the meta-whatsapp-rs service's domain,
//! without an HTTP framework or a database driver (docs/design/server.md;
//! docs/architecture.md, § Service).
//!
//! The service (`meta-whatsapp-server`) is this crate plus adapters on
//! each side: an API adapter (today the axum one, with the OpenAPI
//! document) that turns requests into calls here and [`ServiceError`]s
//! into answers, and a backend (memory, Postgres) that implements the
//! [ports](#ports). Either side can be swapped without touching the rest.
//!
//! | Module | What |
//! | --- | --- |
//! | [`model`] | tenants, API keys, bindings, idempotency records: the validated domain types |
//! | [`admin`] | the admin operations as services: tenants, keys, platform keys, attaching a WABA (decision D4), reading and unbinding a binding, the vault's rotation |
//! | [`messages`] | the message operations as services: the send request (free of any API toolkit), the recipient and E.164 rules, sending (the accepted message) and marking read |
//! | [`numbers`] | the number operations as services: a number's live details, its business profile read and changed, disconnecting a WABA |
//! | [`media`] | the media operations as services: an upload checked (type, size, file name) before any request, a download verified against Meta's SHA-256 (whole, or as a framework-free byte stream that ends before its last chunk on a mismatch), a deletion of the number's own media only |
//! | [`inbox`] | the inbox operations as services: a number's conversations and one conversation's history, from its binding's start on (decision D10: a moved number's history hidden from its new tenant, never purged), and a reply through the library's inbox, checked against the 24-hour window and the thread's owner first ([`inbox::ReplyChecksByNumber`]: the checks per number); since roadmap M2g, a conversation's window as a reply would meet it, in the list too, marking a conversation read (a read receipt on request) and a message by id, each from the binding's start on |
//! | [`erasure`] | erasing a customer on a number (roadmap M2f): every identity the inbox connects to the ones named, journaled (an HMAC of each, never in clear) before the outbox's rows naming them and their inbox records are deleted, audited; only when a caller with the scope `erasure` asks |
//! | [`payments`] | the payment operations as services (roadmap P2): a WABA's payment configurations (list, get, create, a new OAuth link, delete), a payment's lookup and its refund on a number, each configuration named looked for in the number's own WABA first |
//! | [`groups`] | a number's groups as services (roadmap M5f), over the library's `client::groups` and group analytics: create, list, read, change, delete, the invite link, the participants, the join requests; **a group is the number's or it does not exist**, decided by [`groups::GroupRecords`] (written by Meta's lifecycle events and the listing) before the token is opened ([`groups::GroupRecords::owned`]); no participant in any log, error or audit event |
//! | [`templates`] | the template operations as services: the list (cached per WABA, [`templates::TemplateCache`]), the lookup of an id in a WABA, creation (the definition checked first), authentication templates in one or more languages, deletion |
//! | [`flows`] | WhatsApp Flows' management as services (roadmap M5d): a WABA's Flows created, listed, read, changed, filled with their Flow JSON, previewed, published, deprecated and deleted, each Flow id read for its WABA first |
//! | [`flows_endpoint`] | the Flows data endpoint (roadmap M5d), with no HTTP framework: Meta's signed request verified in constant time before anything is read, decrypted with the Flow private key ([`flows_endpoint::FlowKeys`]), a health check and an error notification answered by the service, a data exchange by the integrating app ([`flows_endpoint::FlowHandler`]), the answer sealed |
//! | [`otp`] | one-time passcodes as services, through the library's `OtpService`: a code sent from the tenant's sending number (its own, or the deployment's default OTP tenant's: decision D8), under the tenant's namespace, and a typed code checked |
//! | [`signup`] | Embedded Signup as services, over the library's `EmbeddedSignup`: an attempt bound to the tenant, its completion (decision D4 on the claimed WABA, then in the library's approval, before anything is stored, subscribed or shared), the kept attempts and their resumption, in both partner modes; offboarding (a tenant's disconnection and `account_update` webhooks, the credit line revoked first), and the operator's explicit re-funding of a revoked business |
//! | [`request`] | what the requests share: a Meta object read into the library's type, no key dropped; a Meta object id checked before it reaches a Graph path |
//! | [`keys`] | the API key format, digests, constant-time comparison |
//! | [`error`] | the error model as data: codes, statuses as numbers, `retryable`, `may_have_been_sent`, Meta's `details` |
//! | [`authz`] | the authorization order: key, tenant, scope, ownership, vault |
//! | [`audit`], [`observe`] | the audit events (target `audit`); what an operation reports to the adapter's metrics |
//! | [`events`] | routing Meta's webhook events to tenants, their outbox keys and ids, polling, one event by id |
//! | [`number_events`] | the service's own events about a tenant's numbers (roadmap M2d): `number_connected`, `number_disconnected`, `number_reconnect_required`, recorded in the outbox for the number's tenant |
//! | [`live`] | live event streams (SSE in the HTTP adapter): a tenant's events after a cursor, read from the outbox when its notifier wakes them or a re-poll is due, bounded per tenant and replica, a slow client dropped |
//! | [`inbound`] | the webhook pipeline: a signed body verified before anything is stored, parsed, each event under its dedup lease, routed by the records and recorded (inbox, then outbox), and what to answer Meta |
//! | [`idempotency`] | `Idempotency-Key`: claim, settle, replay, over a fingerprint of the operation and its canonical input; running an operation under a key (`run`: a status, a body, and whether it replayed) |
//! | [`operation`] | the tenant operations by id (the HTTP adapter's `operationId`): what a rate class and an idempotency fingerprint follow, whatever the adapter |
//! | [`ratelimit`] | per-tenant token buckets by route class, charged by operation; transfer slots |
//! | [`store`], [`outbox`], [`notify`], [`backend`] | the ports, and the bundle a backend gives |
//! | [`housekeeping`] | the periodic purges, over a [`backend::Backend`], one replica at a time |
//! | [`webhooks_out`] | webhooks out (roadmap M2c): endpoints and their secrets (Standard Webhooks, sealed under the data key), the destination allow-list checked at creation and at every attempt on a fresh, pinned lookup, the retry schedule, and the dispatcher: the outbox fanned out to each endpoint, deliveries claimed under a lease, bounded per endpoint, tenant and replica |
//! | `conformance` (feature `conformance`) | the ports' contracts as executable suites, run over a [`backend::Backend`]: every backend's tests run them |
//!
//! # Ports
//!
//! | Port | Methods | Implemented by the service for |
//! | --- | --- | --- |
//! | [`store::RecordStore`] | tenants, keys, bindings (`ping` … `set_waba_status`) | memory, Postgres |
//! | [`store::IdempotencyRecords`] | `claim_idempotency_key`, `complete_…`, `release_…`, `purge_…` | memory, Postgres |
//! | [`outbox::Outbox`] | `insert`, `page`, `get`, `purge`, `erase_contacts` | memory, Postgres |
//! | [`notify::EventNotifier`] | `notify`, `subscribe`: wake-ups of a tenant's live streams | memory (a broadcast in the process, [`notify::LocalNotifier`]), Postgres (`NOTIFY` on the deployment's channel, one `LISTEN` connection per replica) |
//! | [`store::LeaderLock`] | `try_exclusive(name, lease)`: a lease | memory (the process), Postgres (an advisory lock, the session ended past the lease) |
//! | [`store::Janitor`] | `purge_expired` | memory (nothing to do), Postgres (the library's key/value rows) |
//! | [`store::SchemaMigrator`] | `migrate` | memory (nothing to do), Postgres |
//! | [`webhooks_out::store::WebhookStore`] | endpoints, `fan_out`, `claim`, `settle`, deliveries (roadmap M2c) | memory, Postgres |
//! | [`backend::Backend`] | all of the above, and the library's `KvStore` and `ConversationStore`, over one database | memory, Postgres |
//!
//! No port names a type of a database driver, an HTTP framework or an
//! API toolkit: failures are the library's `StorageError`, times are
//! `time`'s, and no HTTP method or path enters the core: a request is its
//! [`operation::Operation`] and its input (but for the fingerprint of
//! before roadmap S6, `idempotency::Fingerprint::legacy_v1_json` and
//! `legacy_v1_upload`, which an HTTP adapter matches for one TTL). What every backend
//! guarantees, whatever its database (the routing checked again with each
//! outbox insert, the referential rules, one clock, typed contention,
//! purges without locks, byte order), is [`store`]'s module
//! documentation; the suites of feature `conformance`
//! (`meta_whatsapp_server_core::conformance::run`) check it, over a
//! backend's bundle.

pub mod admin;
pub mod audit;
pub mod authz;
pub mod backend;
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod erasure;
pub mod error;
pub mod events;
pub mod flows;
pub mod flows_endpoint;
pub mod groups;
pub mod housekeeping;
pub mod idempotency;
pub mod inbound;
pub mod inbox;
pub mod keys;
pub mod live;
pub mod media;
pub mod messages;
pub mod model;
pub mod notify;
pub mod number_events;
pub mod numbers;
pub mod observe;
pub mod operation;
pub mod otp;
pub mod outbox;
pub mod payments;
pub mod ratelimit;
pub mod registration;
pub mod request;
pub mod signup;
pub mod store;
pub mod templates;
pub mod uploads;
pub mod webhooks_out;

pub use error::ServiceError;
