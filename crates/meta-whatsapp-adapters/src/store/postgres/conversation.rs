//! [`ConversationStore`] on Postgres.
//!
//! - Content keeps U+0000: `kind`, `text` and the summary's preview are
//!   `BYTEA` (their UTF-8 bytes), `payload` and `error` are `json` bound as
//!   JSON text (see the [module docs](super#content-keeps-u0000)). Ids,
//!   contacts and phone number ids are `TEXT COLLATE "C"`, and they alone
//!   (with the timestamps) order and match rows; the only comparison on
//!   content is `fill_media_placeholder`'s, of `kind_utf8` with the
//!   placeholder's bytes.
//! - `append` is a single statement: the message insert (`ON CONFLICT (id)
//!   DO NOTHING`) feeds the inbox-summary upsert through a data-modifying
//!   CTE, so a duplicate id changes nothing and the summary can never
//!   disagree with the history, without a client-side transaction.
//! - `append_synced` is one statement per batch: the messages travel as
//!   one array per column (`UNNEST`), duplicates within the batch are
//!   dropped (first wins), the insert is `ON CONFLICT (id) DO NOTHING`, and
//!   each touched conversation's summary is upserted once from its newest
//!   inserted row, without the inbound effects (`last_inbound_at`,
//!   `unread`). Nothing about a message's origin is stored: the summary is
//!   maintained when a row is written, so synced history needs no column.
//! - `revoke` first tries to store the tombstone (a message insert alone,
//!   `ON CONFLICT (id) DO NOTHING`: a tombstone is never part of the
//!   summary, and the primary key serializes it against a concurrent append
//!   of the message), and only when the id is taken runs the status
//!   compare-and-swap below, restricted to the revoke's direction. A
//!   separate statement, so it sees a row a concurrent append committed.
//! - `fill_media_placeholder` is one statement too: the content update of
//!   a row whose `kind` is still the placeholder and whose status is not
//!   `deleted` (a revoked placeholder never gets its content), and the
//!   preview of its conversation when it is the latest message. Two
//!   concurrent fills cannot both apply: the second finds the row no longer
//!   a placeholder, and a concurrent revoke either comes first (the fill
//!   finds it deleted) or marks the filled row deleted.
//! - `update_status` (and `revoke`) applies [`DeliveryStatus::supersedes`] — the rule lives
//!   in `meta-whatsapp-core`, not re-encoded in SQL — as a compare-and-swap on the
//!   stored status: read it, decide in Rust, then `UPDATE … WHERE status =
//!   <what we read>`. A concurrent update makes the swap miss and we re-read.
//!   Every successful update strictly raises the status rank, so the loop is
//!   bounded by the number of ranks. Both statements match `id` *and*
//!   `phone_number_id`, so an event of one business number never changes a
//!   row of another.
//! - `message` (the lookup) matches `id` *and* `phone_number_id` too, so it
//!   finds a message whether ids are unique per store (the primary key
//!   today) or per number (`OPEN_QUESTIONS.md` #33), and never another
//!   number's.
//! - Window events, thread owners and synced contacts live in tables of
//!   their own (migration `0004`), keyed by business number and contact
//!   (a window event by business number and id). `set_thread_owner` and
//!   `put_contact` are one upsert each whose update only applies when the
//!   stored record is not later (`ON CONFLICT … DO UPDATE … WHERE`), so
//!   the rule holds under concurrency without a read. `remove_contact` is
//!   the same upsert, of a row with `removed` set and nothing but the key
//!   and the removal's time (it replaces the contact, or takes its place
//!   when none is stored), so a late `add` older than it is refused;
//!   reads skip those rows. Identity links live in `wa_identity_links`,
//!   keyed by business number and their two identities (`link_identity`
//!   is an insert that ignores a conflict), and `identities` is one
//!   recursive query over the synced contacts and the links of one number.
//! - Every message insert also writes its sender
//!   ([`StoredMessage::sender`], computed in Rust: the payload is never
//!   read field by field in SQL, see the [module docs](super#content-keeps-u0000))
//!   to `wa_messages.sender`, indexed with the business number, which no
//!   read returns: it is what an erasure matches a person's group
//!   messages on.
//! - `erase_all` (and `erase`, its one-key case) and `purge_before` order
//!   themselves against the writers with
//!   two transaction-level advisory locks, in the two-key form (whose key
//!   space never meets the one-key locks of sqlx's `migrate` or of the
//!   service):
//!   - the **number lock**, `('wa_messages'::regclass::oid::int4,
//!     hashtext(<phone_number_id>))` under the default prefix (the class
//!     is the table's object id: each schema and prefix has its own): `append`
//!     and `append_synced` take it shared, first thing in their one
//!     statement (a batch takes each of its numbers', in order); `erase_all`
//!     takes it exclusive, then deletes in a second statement, whose
//!     snapshot so holds every append that took the lock before it, and
//!     no append can start meanwhile. Without it, an append that had
//!     written its message but not yet its conversation's summary when
//!     the erasure took its snapshot left the message behind, its summary
//!     deleted (the erasure waited for the summary row and deleted the
//!     appended version). An append that waits on the lock is recorded
//!     after the erasure, with a summary of its own. The cost: while an
//!     erasure deletes, appends to its business number wait (every
//!     contact's: the lock is per number, so that a history chunk takes one
//!     lock per number rather than one per contact).
//!   - the **purge lock**, `('wa_conversations'::regclass::oid::int4, 0)`:
//!     `purge_before` takes it exclusive, `erase_all` shared, before the number
//!     lock. Two purges, or a purge and an erasure, would otherwise take
//!     the same rows' locks in the orders of their plans (the `ts` index
//!     oldest first, the conversation's index newest first, a sequential
//!     scan by position), and deadlock. Purges run one at a time;
//!     erasures of different numbers beside each other.
//!
//!   The deletion itself is one statement over one snapshot: six `DELETE`s
//!   in data-modifying CTEs, the messages first (`contact = ANY($2)`; a
//!   synced contact on any of its four ids, a link on either side), then
//!   the person's messages in other conversations (`sender = ANY($2)` and
//!   a conversation not among the ids, so never a row the first `DELETE`
//!   takes), updated in place under [`ErasureMode::Redact`] or deleted
//!   under [`ErasureMode::Delete`], and last the summaries whose latest
//!   message that was: its preview cleared, or the summary rebuilt from
//!   the latest remaining row (never a tombstone), or deleted. Those
//!   messages are the number's, so its lock covers them too. The other
//!   writers take no advisory lock: `fill_media_placeholder` locks a
//!   message then its summary, the order the deletion's statements take
//!   them in, and the rest write one row. An instance of the revision
//!   before these locks does not take them: while one still appends, an
//!   erasure can leave a message without its summary, and its messages
//!   carry no sender (upgrade every instance). A message recorded while a
//!   purge runs is not waited for:
//!   one older than the cutoff (a late history chunk) may be kept, its
//!   summary purged, until the next purge.
//! - `purge_before`'s statement is built the same way, and locks the
//!   summaries it deletes in key order first: the order `append_synced`
//!   updates a chunk's summaries in, so the two never deadlock.
//!   `wa_messages(ts)` and `wa_conversations(last_message_at)` are indexed
//!   for it (migration `0004`).

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use meta_whatsapp_core::error::StorageError;
use meta_whatsapp_core::ids::{AppId, MessageId, PhoneNumberId, UserId};
use meta_whatsapp_core::store::{
    ConversationKey, ConversationStore, ConversationSummary, DeliveryStatus, Direction, Erased,
    ErasureMode, IdentityLink, Purged, Retention, StoredContact, StoredMessage, ThreadOwner,
    ThreadOwnership, WindowEvent, WindowEventKind,
};
use sqlx::postgres::PgRow;
use sqlx::{AssertSqlSafe, PgPool, Row};
use time::OffsetDateTime;

use super::{TablePrefix, backend, to_i64};

/// More compare-and-swap rounds than there are status ranks: reaching it
/// means something outside this adapter keeps rewriting the row.
const MAX_STATUS_ROUNDS: usize = 16;

/// `ConversationStore` over a Postgres pool. Cheap to clone.
///
/// Unread counts are by **arrival** (inbound messages appended since the
/// last `mark_read`; synced history never counts), like the in-memory
/// store. Timestamps are stored with microsecond precision.
///
/// Retention is kept by default; [`with_retention`](Self::with_retention)
/// sets what [`ConversationStore::apply_retention`] purges. Nothing purges
/// on its own: schedule `apply_retention`. Runs from several replicas at
/// once are safe: purges take turns (see the module docs), and an erasure
/// waits for a purge in progress. An erasure redacts an erased person's
/// messages in other conversations (a group's) by default;
/// [`with_erasure_mode`](Self::with_erasure_mode) deletes them instead.
#[derive(Clone)]
pub struct PostgresConversationStore {
    pool: PgPool,
    prefix: TablePrefix,
    sql: Arc<Sql>,
    retention: Retention,
    erasure_mode: ErasureMode,
}

impl fmt::Debug for PostgresConversationStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresConversationStore")
            .field("prefix", &self.prefix)
            .field("retention", &self.retention)
            .field("erasure_mode", &self.erasure_mode)
            .finish_non_exhaustive()
    }
}

struct Sql {
    append: Arc<str>,
    append_synced: Arc<str>,
    tombstone: Arc<str>,
    fill_placeholder: Arc<str>,
    status_get: Arc<str>,
    status_swap: Arc<str>,
    messages: Arc<str>,
    messages_before: Arc<str>,
    conversations: Arc<str>,
    conversations_before: Arc<str>,
    mark_read: Arc<str>,
    last_inbound_at: Arc<str>,
    message: Arc<str>,
    window_event: Arc<str>,
    window_events: Arc<str>,
    window_events_before: Arc<str>,
    set_owner: Arc<str>,
    owner: Arc<str>,
    put_contact: Arc<str>,
    remove_contact: Arc<str>,
    contact: Arc<str>,
    contacts: Arc<str>,
    contacts_after: Arc<str>,
    link_identity: Arc<str>,
    identity_links: Arc<str>,
    identities: Arc<str>,
    erase_locks: [Arc<str>; 2],
    erase_redacting: Arc<str>,
    erase_deleting: Arc<str>,
    purge_lock: Arc<str>,
    purge_all: Arc<str>,
    purge_number: Arc<str>,
}

const MESSAGE_COLUMNS: &str = "id, phone_number_id, contact, direction, kind_utf8, text_utf8, \
     payload_json, status, ts, status_at, error_json";

/// What a message insert writes: [`MESSAGE_COLUMNS`] and the sender
/// ([`StoredMessage::sender`]), which no read returns.
const INSERT_COLUMNS: &str = "id, phone_number_id, contact, direction, kind_utf8, text_utf8, \
     payload_json, status, ts, status_at, error_json, sender";

const SUMMARY_COLUMNS: &str =
    "phone_number_id, contact, last_message_at, last_inbound_at, last_text_utf8, unread";

const WINDOW_EVENT_COLUMNS: &str = "phone_number_id, id, contact, kind, ts";

const OWNER_COLUMNS: &str = "owner, role, app_id, since";

const CONTACT_COLUMNS: &str = "phone_number_id, contact, full_name_utf8, first_name_utf8, \
     phone_number, user_id, parent_user_id, username_utf8, synced_at";

/// The twelve values of a message row, in [`INSERT_COLUMNS`] order. The
/// content travels as bytes (`kind_utf8`, `text_utf8`: `BYTEA`) and as
/// JSON text cast to `json` (`payload_json`, `error_json`): neither Postgres
/// `text` nor `jsonb` can hold U+0000, and content keeps it (see the
/// [module docs](super#content-keeps-u0000)).
const MESSAGE_VALUES: &str = "$1, $2, $3, $4, $5, $6, $7::json, $8, $9, $10, $11::json, $12";

/// The summary's "newest message" is the max by `(ts, id)`, the same order
/// the history pages in.
const NEWER: &str = "(EXCLUDED.last_message_at, EXCLUDED.last_message_id) \
                     > (c.last_message_at, c.last_message_id)";

/// The arguments of the number lock on business number `number` (an SQL
/// expression): the class is the messages table's object id, so the
/// tables of each prefix, and of each schema, have locks of their own. A
/// stable identifier: every replica, of every revision, must take the
/// same lock (`docs/architecture.md`).
fn number_lock(messages: &str, number: &str) -> String {
    format!("'{messages}'::regclass::oid::int4, hashtext({number})")
}

/// The arguments of the purge lock: the conversations table's object id
/// (see [`number_lock`]).
fn purge_lock(conversations: &str) -> String {
    format!("'{conversations}'::regclass::oid::int4, 0")
}

/// The `append` statement. `inbound_at` and `unread` are the SQL
/// expressions an inserted row contributes to its conversation's
/// `last_inbound_at` and `unread`. The message is inserted from the row of
/// `lock`, so the number lock is held before anything is written.
fn append_sql(messages: &str, conversations: &str, inbound_at: &str, unread: &str) -> String {
    let newer = NEWER;
    let lock = number_lock(messages, "$2");
    format!(
        "WITH lock AS MATERIALIZED (SELECT pg_advisory_xact_lock_shared({lock})), \
         inserted AS ( \
           INSERT INTO {messages} ({INSERT_COLUMNS}) \
           SELECT {MESSAGE_VALUES} FROM lock \
           ON CONFLICT (id) DO NOTHING \
           RETURNING id, phone_number_id, contact, direction, text_utf8, ts \
         ) \
         INSERT INTO {conversations} AS c \
           (phone_number_id, contact, last_message_at, last_message_id, last_text_utf8, \
            last_inbound_at, unread) \
         SELECT phone_number_id, contact, ts, id, text_utf8, {inbound_at}, {unread} \
         FROM inserted \
         ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
           last_message_at = CASE WHEN {newer} THEN EXCLUDED.last_message_at ELSE c.last_message_at END, \
           last_message_id = CASE WHEN {newer} THEN EXCLUDED.last_message_id ELSE c.last_message_id END, \
           last_text_utf8 = CASE WHEN {newer} THEN EXCLUDED.last_text_utf8 ELSE c.last_text_utf8 END, \
           last_inbound_at = GREATEST(c.last_inbound_at, EXCLUDED.last_inbound_at), \
           unread = c.unread + EXCLUDED.unread \
         RETURNING 1 AS appended"
    )
}

/// The `purge_before` statement. `scope` restricts every `DELETE` to one
/// business number (`$2`), or is empty. The summaries are locked in key
/// order before they are deleted: the order `append_synced` updates them
/// in, so a purge and a history chunk never deadlock.
fn purge_sql(prefix: &TablePrefix, scope: &str) -> String {
    let messages = prefix.table("messages");
    let conversations = prefix.table("conversations");
    let window_events = prefix.table("window_events");
    let owners = prefix.table("thread_owners");
    let contacts = prefix.table("synced_contacts");
    format!(
        "WITH m AS (DELETE FROM {messages} WHERE ts < $1{scope} RETURNING 1), \
         c AS (DELETE FROM {conversations} WHERE (phone_number_id, contact) IN ( \
           SELECT phone_number_id, contact FROM {conversations} \
           WHERE last_message_at < $1{scope} \
           ORDER BY phone_number_id, contact FOR UPDATE \
         ) RETURNING 1), \
         w AS (DELETE FROM {window_events} WHERE ts < $1{scope} RETURNING 1), \
         o AS (DELETE FROM {owners} WHERE since < $1{scope} RETURNING 1), \
         r AS (DELETE FROM {contacts} WHERE removed AND synced_at < $1{scope} RETURNING 1) \
         SELECT (SELECT count(*) FROM m), (SELECT count(*) FROM c), \
           (SELECT count(*) FROM w), (SELECT count(*) FROM o), (SELECT count(*) FROM r)"
    )
}

/// The deletion of `erase_all`, in one statement over one snapshot: `$1`
/// the business number, `$2` the erased ids (`text[]`), `$3` the kind an
/// erased person's group message gets (`redact`) or the tombstone's kind,
/// which the recomputed summaries skip (not `redact`), `$4` the erased ids
/// that are not empty, which a synced contact's other ids and a link's
/// sides are matched on (an empty one names no one). The messages go
/// first (the order `fill_media_placeholder` takes a message and its
/// summary in). `g` is the person's messages in conversations keyed by
/// someone else: disjoint from `m`'s rows (their conversation is not one
/// of the ids), and so are the summaries it rewrites from `c`'s.
fn erase_sql(prefix: &TablePrefix, redact: bool) -> String {
    let messages = prefix.table("messages");
    let conversations = prefix.table("conversations");
    let window_events = prefix.table("window_events");
    let owners = prefix.table("thread_owners");
    let contacts = prefix.table("synced_contacts");
    let links = prefix.table("identity_links");
    let theirs = "phone_number_id = $1 AND sender = ANY($2) AND NOT (contact = ANY($2))";
    let group = if redact {
        // Each message keeps its place; the preview of a conversation whose
        // latest message it is goes with its text.
        format!(
            "g AS (UPDATE {messages} SET kind_utf8 = $3, text_utf8 = NULL, \
               payload_json = '{{}}'::json, error_json = NULL, sender = NULL \
               WHERE {theirs} RETURNING id, contact), \
             gs AS (UPDATE {conversations} AS s SET last_text_utf8 = NULL FROM g \
               WHERE s.phone_number_id = $1 AND s.contact = g.contact \
                 AND s.last_message_id = g.id)"
        )
    } else {
        // A conversation whose latest message went follows its latest
        // remaining one (never a tombstone), or loses its summary.
        format!(
            "g AS (DELETE FROM {messages} WHERE {theirs} RETURNING id, contact), \
             gc AS (SELECT DISTINCT s.contact FROM {conversations} AS s JOIN g \
               ON s.phone_number_id = $1 AND s.contact = g.contact AND s.last_message_id = g.id), \
             gl AS (SELECT DISTINCT ON (r.contact) r.contact, r.ts, r.id, r.text_utf8 \
               FROM {messages} AS r JOIN gc ON r.phone_number_id = $1 AND r.contact = gc.contact \
               WHERE r.kind_utf8 <> $3 AND NOT EXISTS (SELECT 1 FROM g WHERE g.id = r.id) \
               ORDER BY r.contact, r.ts DESC, r.id COLLATE \"C\" DESC), \
             su AS (UPDATE {conversations} AS s SET last_message_at = gl.ts, \
               last_message_id = gl.id, last_text_utf8 = gl.text_utf8 FROM gl \
               WHERE s.phone_number_id = $1 AND s.contact = gl.contact), \
             sd AS (DELETE FROM {conversations} AS s USING gc \
               WHERE s.phone_number_id = $1 AND s.contact = gc.contact \
                 AND NOT EXISTS (SELECT 1 FROM gl WHERE gl.contact = gc.contact))"
        )
    };
    format!(
        "WITH m AS (DELETE FROM {messages} \
           WHERE phone_number_id = $1 AND contact = ANY($2) RETURNING 1), \
         c AS (DELETE FROM {conversations} \
           WHERE phone_number_id = $1 AND contact = ANY($2) RETURNING 1), \
         w AS (DELETE FROM {window_events} \
           WHERE phone_number_id = $1 AND contact = ANY($2) RETURNING 1), \
         o AS (DELETE FROM {owners} \
           WHERE phone_number_id = $1 AND contact = ANY($2) RETURNING 1), \
         p AS (DELETE FROM {contacts} WHERE phone_number_id = $1 \
           AND (contact = ANY($2) OR user_id = ANY($4) OR parent_user_id = ANY($4) \
             OR phone_number = ANY($4)) RETURNING 1), \
         l AS (DELETE FROM {links} WHERE phone_number_id = $1 \
           AND (previous = ANY($4) OR current = ANY($4)) RETURNING 1), \
         {group} \
         SELECT (SELECT count(*) FROM m), (SELECT count(*) FROM c), \
           (SELECT count(*) FROM w), (SELECT count(*) FROM o), (SELECT count(*) FROM p), \
           (SELECT count(*) FROM l), (SELECT count(*) FROM g)"
    )
}

/// The `append_synced` statement: one row per element of the twelve
/// column arrays, the first of each id kept, no inbound effects. Its rows
/// are read under the number lock of each of the batch's numbers, taken
/// shared, in order, before the first is inserted.
fn append_synced_sql(messages: &str, conversations: &str) -> String {
    let newer = NEWER;
    let lock = number_lock(messages, "number");
    format!(
        "WITH lock AS MATERIALIZED ( \
           SELECT pg_advisory_xact_lock_shared({lock}) \
           FROM unnest($2::text[]) AS number GROUP BY number ORDER BY number \
         ), input AS ( \
           SELECT DISTINCT ON (id) * FROM UNNEST($1::text[], $2::text[], $3::text[], \
             $4::text[], $5::bytea[], $6::bytea[], $7::json[], $8::text[], \
             $9::timestamptz[], $10::timestamptz[], $11::json[], $12::text[]) \
             WITH ORDINALITY AS t({INSERT_COLUMNS}, n) \
           WHERE (SELECT count(*) FROM lock) >= 0 \
           ORDER BY id, n \
         ), inserted AS ( \
           INSERT INTO {messages} ({INSERT_COLUMNS}) \
           SELECT {INSERT_COLUMNS} FROM input \
           ON CONFLICT (id) DO NOTHING \
           RETURNING id, phone_number_id, contact, text_utf8, ts \
         ), latest AS ( \
           SELECT DISTINCT ON (phone_number_id, contact) phone_number_id, contact, ts, id, text_utf8 \
           FROM inserted \
           ORDER BY phone_number_id, contact, ts DESC, id COLLATE \"C\" DESC \
         ), summary AS ( \
           INSERT INTO {conversations} AS c \
             (phone_number_id, contact, last_message_at, last_message_id, last_text_utf8, \
              last_inbound_at, unread) \
           SELECT phone_number_id, contact, ts, id, text_utf8, NULL, 0 FROM latest \
           ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
             last_message_at = CASE WHEN {newer} THEN EXCLUDED.last_message_at ELSE c.last_message_at END, \
             last_message_id = CASE WHEN {newer} THEN EXCLUDED.last_message_id ELSE c.last_message_id END, \
             last_text_utf8 = CASE WHEN {newer} THEN EXCLUDED.last_text_utf8 ELSE c.last_text_utf8 END \
         ) \
         SELECT id FROM inserted"
    )
}

impl Sql {
    #[allow(clippy::too_many_lines)] // one statement per field
    fn new(prefix: &TablePrefix) -> Self {
        let messages = prefix.table("messages");
        let conversations = prefix.table("conversations");
        let window_events = prefix.table("window_events");
        let owners = prefix.table("thread_owners");
        let contacts = prefix.table("synced_contacts");
        let links = prefix.table("identity_links");
        let arc = |s: String| -> Arc<str> { Arc::from(s) };
        Self {
            append: arc(append_sql(
                &messages,
                &conversations,
                "CASE WHEN direction = 'inbound' THEN ts END",
                "CASE WHEN direction = 'inbound' THEN 1 ELSE 0 END",
            )),
            append_synced: arc(append_synced_sql(&messages, &conversations)),
            // The history row alone: a tombstone never touches (nor creates)
            // its conversation's summary.
            tombstone: arc(format!(
                "INSERT INTO {messages} ({INSERT_COLUMNS}) \
                 VALUES ({MESSAGE_VALUES}) \
                 ON CONFLICT (id) DO NOTHING \
                 RETURNING 1 AS appended"
            )),
            // Data-modifying CTEs always run to completion, whether or not
            // the final SELECT reads them. `$6`: the placeholder kind, as
            // bytes (`kind_utf8` is compared byte for byte). `$7`: the
            // `deleted` status, which a placeholder is never filled in.
            fill_placeholder: arc(format!(
                "WITH filled AS ( \
                   UPDATE {messages} SET kind_utf8 = $3, text_utf8 = $4, payload_json = $5::json \
                   WHERE id = $1 AND phone_number_id = $2 AND kind_utf8 = $6 AND status <> $7 \
                   RETURNING id, phone_number_id, contact, text_utf8 \
                 ), preview AS ( \
                   UPDATE {conversations} AS c SET last_text_utf8 = f.text_utf8 \
                   FROM filled f \
                   WHERE c.phone_number_id = f.phone_number_id AND c.contact = f.contact \
                     AND c.last_message_id = f.id \
                 ) \
                 SELECT count(*) FROM filled"
            )),
            // `$3` (`$7`): the direction a revoke is restricted to, NULL for
            // a status update.
            status_get: arc(format!(
                "SELECT status FROM {messages} \
                 WHERE id = $1 AND phone_number_id = $2 AND ($3::text IS NULL OR direction = $3)"
            )),
            status_swap: arc(format!(
                "UPDATE {messages} SET status = $4, status_at = $5, \
                   error_json = COALESCE($6::json, error_json) \
                 WHERE id = $1 AND phone_number_id = $2 AND status = $3 \
                   AND ($7::text IS NULL OR direction = $7)"
            )),
            messages: arc(format!(
                "SELECT {MESSAGE_COLUMNS} FROM {messages} \
                 WHERE phone_number_id = $1 AND contact = $2 \
                 ORDER BY ts DESC, id DESC LIMIT $3"
            )),
            messages_before: arc(format!(
                "SELECT {MESSAGE_COLUMNS} FROM {messages} \
                 WHERE phone_number_id = $1 AND contact = $2 AND (ts, id) < ($4, $5) \
                 ORDER BY ts DESC, id DESC LIMIT $3"
            )),
            conversations: arc(format!(
                "SELECT {SUMMARY_COLUMNS} FROM {conversations} \
                 WHERE phone_number_id = $1 \
                 ORDER BY last_message_at DESC, contact DESC LIMIT $2"
            )),
            conversations_before: arc(format!(
                "SELECT {SUMMARY_COLUMNS} FROM {conversations} \
                 WHERE phone_number_id = $1 AND (last_message_at, contact) < ($3, $4) \
                 ORDER BY last_message_at DESC, contact DESC LIMIT $2"
            )),
            mark_read: arc(format!(
                "UPDATE {conversations} SET unread = 0 WHERE phone_number_id = $1 AND contact = $2"
            )),
            last_inbound_at: arc(format!(
                "SELECT last_inbound_at FROM {conversations} \
                 WHERE phone_number_id = $1 AND contact = $2"
            )),
            message: arc(format!(
                "SELECT {MESSAGE_COLUMNS} FROM {messages} WHERE id = $1 AND phone_number_id = $2"
            )),
            window_event: arc(format!(
                "INSERT INTO {window_events} ({WINDOW_EVENT_COLUMNS}) VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (phone_number_id, id) DO NOTHING \
                 RETURNING 1 AS recorded"
            )),
            window_events: arc(format!(
                "SELECT {WINDOW_EVENT_COLUMNS} FROM {window_events} \
                 WHERE phone_number_id = $1 AND contact = $2 \
                 ORDER BY ts DESC, id DESC LIMIT $3"
            )),
            window_events_before: arc(format!(
                "SELECT {WINDOW_EVENT_COLUMNS} FROM {window_events} \
                 WHERE phone_number_id = $1 AND contact = $2 AND (ts, id) < ($4, $5) \
                 ORDER BY ts DESC, id DESC LIMIT $3"
            )),
            // The update applies only when the stored record is not later.
            set_owner: arc(format!(
                "INSERT INTO {owners} AS o (phone_number_id, contact, {OWNER_COLUMNS}) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
                   owner = EXCLUDED.owner, role = EXCLUDED.role, app_id = EXCLUDED.app_id, \
                   since = EXCLUDED.since \
                 WHERE EXCLUDED.since >= o.since \
                 RETURNING 1 AS stored"
            )),
            owner: arc(format!(
                "SELECT {OWNER_COLUMNS} FROM {owners} WHERE phone_number_id = $1 AND contact = $2"
            )),
            put_contact: arc(format!(
                "INSERT INTO {contacts} AS p ({CONTACT_COLUMNS}) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
                 ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
                   full_name_utf8 = EXCLUDED.full_name_utf8, \
                   first_name_utf8 = EXCLUDED.first_name_utf8, \
                   phone_number = EXCLUDED.phone_number, user_id = EXCLUDED.user_id, \
                   parent_user_id = EXCLUDED.parent_user_id, \
                   username_utf8 = EXCLUDED.username_utf8, synced_at = EXCLUDED.synced_at, \
                   removed = false \
                 WHERE EXCLUDED.synced_at >= p.synced_at \
                 RETURNING 1 AS stored"
            )),
            // The removal replaces the row (or takes the key's place) with
            // its time alone, unless the stored row is later. `prior`, read
            // (and locked) first, says whether a contact was there.
            remove_contact: arc(format!(
                "WITH prior AS ( \
                   SELECT removed FROM {contacts} \
                   WHERE phone_number_id = $1 AND contact = $2 AND synced_at <= $3 \
                   FOR UPDATE \
                 ), removal AS ( \
                   INSERT INTO {contacts} AS p (phone_number_id, contact, synced_at, removed) \
                   VALUES ($1, $2, $3, true) \
                   ON CONFLICT (phone_number_id, contact) DO UPDATE SET \
                     full_name_utf8 = NULL, first_name_utf8 = NULL, phone_number = NULL, \
                     user_id = NULL, parent_user_id = NULL, username_utf8 = NULL, \
                     synced_at = EXCLUDED.synced_at, removed = true \
                   WHERE p.synced_at <= EXCLUDED.synced_at \
                   RETURNING 1 \
                 ) \
                 SELECT EXISTS (SELECT 1 FROM prior WHERE NOT removed) \
                   AND EXISTS (SELECT 1 FROM removal)"
            )),
            contact: arc(format!(
                "SELECT {CONTACT_COLUMNS} FROM {contacts} \
                 WHERE phone_number_id = $1 AND contact = $2 AND NOT removed"
            )),
            contacts: arc(format!(
                "SELECT {CONTACT_COLUMNS} FROM {contacts} \
                 WHERE phone_number_id = $1 AND NOT removed ORDER BY contact LIMIT $2"
            )),
            contacts_after: arc(format!(
                "SELECT {CONTACT_COLUMNS} FROM {contacts} \
                 WHERE phone_number_id = $1 AND contact > $3 AND NOT removed \
                 ORDER BY contact LIMIT $2"
            )),
            link_identity: arc(format!(
                "INSERT INTO {links} (phone_number_id, previous, current, ts) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (phone_number_id, previous, current) DO NOTHING \
                 RETURNING 1 AS linked"
            )),
            identity_links: arc(format!(
                "SELECT phone_number_id, previous, current, ts FROM {links} \
                 WHERE phone_number_id = $1 AND (previous = $2 OR current = $2) \
                 ORDER BY ts, previous, current"
            )),
            // The closure from `$2`: a synced contact (not a kept removal)
            // naming an identity found adds its four, a link its other
            // side. An empty identity names no one: it is never followed
            // nor found. `UNION` drops what was found already, so it ends.
            identities: arc(format!(
                "WITH RECURSIVE ids(id) AS ( \
                   SELECT $2::text COLLATE \"C\" \
                   UNION \
                   SELECT n.id FROM ids CROSS JOIN LATERAL ( \
                     SELECT v.id FROM {contacts} AS p, LATERAL (VALUES (p.contact), \
                       (p.user_id), (p.parent_user_id), (p.phone_number)) AS v(id) \
                     WHERE p.phone_number_id = $1 AND NOT p.removed \
                       AND ids.id IN (p.contact, p.user_id, p.parent_user_id, p.phone_number) \
                     UNION ALL \
                     SELECT l.previous FROM {links} AS l \
                     WHERE l.phone_number_id = $1 AND l.current = ids.id \
                     UNION ALL \
                     SELECT l.current FROM {links} AS l \
                     WHERE l.phone_number_id = $1 AND l.previous = ids.id \
                   ) AS n(id) WHERE ids.id <> '' AND n.id <> '' \
                 ) \
                 SELECT id FROM ids"
            )),
            // The purge lock shared, then the number lock exclusive: see the
            // module docs. Each its own statement, so the deletion's
            // snapshot is taken after both are held.
            erase_locks: [
                arc(format!(
                    "SELECT pg_advisory_xact_lock_shared({})",
                    purge_lock(&conversations)
                )),
                arc(format!(
                    "SELECT pg_advisory_xact_lock({})",
                    number_lock(&messages, "$1")
                )),
            ],
            erase_redacting: arc(erase_sql(prefix, true)),
            erase_deleting: arc(erase_sql(prefix, false)),
            purge_lock: arc(format!(
                "SELECT pg_advisory_xact_lock({})",
                purge_lock(&conversations)
            )),
            purge_all: arc(purge_sql(prefix, "")),
            purge_number: arc(purge_sql(prefix, " AND phone_number_id = $2")),
        }
    }
}

impl PostgresConversationStore {
    /// Store on `pool` with the default `wa_` tables. Run
    /// [`migrate`](super::migrate) first.
    pub fn new(pool: PgPool) -> Self {
        Self::with_prefix(pool, TablePrefix::DEFAULT)
    }

    /// Store on `pool` using `prefix`'s tables. Run
    /// [`migrate_with_prefix`](super::migrate_with_prefix) with the same
    /// prefix first.
    pub fn with_prefix(pool: PgPool, prefix: TablePrefix) -> Self {
        let sql = Arc::new(Sql::new(&prefix));
        Self {
            pool,
            prefix,
            sql,
            retention: Retention::Keep,
            erasure_mode: ErasureMode::Redact,
        }
    }

    /// This store applying `retention` in
    /// [`ConversationStore::apply_retention`] (the default keeps
    /// everything).
    #[must_use]
    pub fn with_retention(mut self, retention: Retention) -> Self {
        self.retention = retention;
        self
    }

    /// This store redacting or deleting an erased person's messages in
    /// other conversations (a group's) as `mode` says, in
    /// [`ConversationStore::erase_all`] (the default redacts:
    /// [`ErasureMode::Redact`]).
    #[must_use]
    pub fn with_erasure_mode(mut self, mode: ErasureMode) -> Self {
        self.erasure_mode = mode;
        self
    }

    /// The table prefix in use.
    pub fn prefix(&self) -> &TablePrefix {
        &self.prefix
    }
}

fn direction_str(d: Direction) -> &'static str {
    match d {
        Direction::Inbound => "inbound",
        Direction::Outbound => "outbound",
    }
}

fn parse_direction(s: &str) -> Result<Direction, StorageError> {
    match s {
        "inbound" => Ok(Direction::Inbound),
        "outbound" => Ok(Direction::Outbound),
        other => Err(StorageError::Backend(anyhow::anyhow!(
            "unknown direction `{other}` in the messages table"
        ))),
    }
}

/// `DeliveryStatus` ↔ its serde name, so the column holds exactly what the
/// type serializes to (and a variant added to `meta-whatsapp-core` needs no change here).
fn status_str(status: DeliveryStatus) -> Result<String, StorageError> {
    match serde_json::to_value(status) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        Ok(other) => Err(StorageError::Backend(anyhow::anyhow!(
            "DeliveryStatus serialized to {other}, expected a string"
        ))),
        Err(source) => Err(StorageError::Corrupt {
            key: "status".to_owned(),
            source,
        }),
    }
}

/// A stored status. The error names no message id (see `set_status`).
fn parse_status(s: String) -> Result<DeliveryStatus, StorageError> {
    serde_json::from_value(serde_json::Value::String(s)).map_err(|source| StorageError::Corrupt {
        key: "message status".to_owned(),
        source,
    })
}

/// `ThreadOwner` ↔ its serde name, as for [`status_str`].
fn owner_str(owner: ThreadOwner) -> Result<String, StorageError> {
    match serde_json::to_value(owner) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        Ok(other) => Err(StorageError::Backend(anyhow::anyhow!(
            "ThreadOwner serialized to {other}, expected a string"
        ))),
        Err(source) => Err(StorageError::Corrupt {
            key: "thread owner".to_owned(),
            source,
        }),
    }
}

/// A stored thread owner. The error names no conversation.
fn parse_owner(s: String) -> Result<ThreadOwner, StorageError> {
    serde_json::from_value(serde_json::Value::String(s)).map_err(|source| StorageError::Corrupt {
        key: "thread owner".to_owned(),
        source,
    })
}

/// Content as the text a `json` column stores. `serde_json` writes U+0000
/// as the escape `\u0000`, which `json` keeps as written (`jsonb` refuses
/// it). `what` names the field in an error, never the message.
fn json_text(value: &serde_json::Value, what: &str) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|source| StorageError::Corrupt {
        key: what.to_owned(),
        source,
    })
}

/// A `BYTEA` content column back to its text. Our writes are always UTF-8;
/// bytes that are not (a row edited by hand) are
/// [`StorageError::Corrupt`], never replaced. The error names the column,
/// never the content.
fn utf8(column: &str, bytes: Vec<u8>) -> Result<String, StorageError> {
    String::from_utf8(bytes).map_err(|e| StorageError::Corrupt {
        key: column.to_owned(),
        source: serde::de::Error::custom(format_args!(
            "not UTF-8 (valid up to byte {})",
            e.utf8_error().valid_up_to()
        )),
    })
}

fn message_from_row(row: &PgRow) -> Result<StoredMessage, StorageError> {
    let id: String = row.try_get("id").map_err(backend)?;
    let direction: String = row.try_get("direction").map_err(backend)?;
    let status: String = row.try_get("status").map_err(backend)?;
    let status = parse_status(status)?;
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let kind: Vec<u8> = row.try_get("kind_utf8").map_err(backend)?;
    let text: Option<Vec<u8>> = row.try_get("text_utf8").map_err(backend)?;
    Ok(StoredMessage {
        conversation: ConversationKey::new(phone_number_id, contact),
        direction: parse_direction(&direction)?,
        kind: utf8("kind_utf8", kind)?,
        text: text.map(|t| utf8("text_utf8", t)).transpose()?,
        payload: row.try_get("payload_json").map_err(backend)?,
        status,
        timestamp: row.try_get("ts").map_err(backend)?,
        status_at: row.try_get("status_at").map_err(backend)?,
        error: row.try_get("error_json").map_err(backend)?,
        id: MessageId::new(id),
    })
}

fn window_event_from_row(row: &PgRow) -> Result<WindowEvent, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let kind: String = row.try_get("kind").map_err(backend)?;
    Ok(WindowEvent {
        conversation: ConversationKey::new(phone_number_id, contact),
        kind: WindowEventKind::from_name(&kind),
        id: row.try_get("id").map_err(backend)?,
        at: row.try_get("ts").map_err(backend)?,
    })
}

fn owner_from_row(row: &PgRow) -> Result<ThreadOwnership, StorageError> {
    let owner: String = row.try_get("owner").map_err(backend)?;
    let app_id: Option<String> = row.try_get("app_id").map_err(backend)?;
    Ok(ThreadOwnership {
        owner: parse_owner(owner)?,
        role: row.try_get("role").map_err(backend)?,
        app_id: app_id.map(AppId::new),
        since: row.try_get("since").map_err(backend)?,
    })
}

/// An optional content column (`*_utf8`) back to its text.
fn optional_utf8(row: &PgRow, column: &str) -> Result<Option<String>, StorageError> {
    let bytes: Option<Vec<u8>> = row.try_get(column).map_err(backend)?;
    bytes.map(|b| utf8(column, b)).transpose()
}

fn contact_from_row(row: &PgRow) -> Result<StoredContact, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let user_id: Option<String> = row.try_get("user_id").map_err(backend)?;
    let parent_user_id: Option<String> = row.try_get("parent_user_id").map_err(backend)?;
    Ok(StoredContact {
        key: ConversationKey::new(phone_number_id, contact),
        full_name: optional_utf8(row, "full_name_utf8")?,
        first_name: optional_utf8(row, "first_name_utf8")?,
        phone_number: row.try_get("phone_number").map_err(backend)?,
        user_id: user_id.map(UserId::new),
        parent_user_id: parent_user_id.map(UserId::new),
        username: optional_utf8(row, "username_utf8")?,
        synced_at: row.try_get("synced_at").map_err(backend)?,
    })
}

fn link_from_row(row: &PgRow) -> Result<IdentityLink, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    Ok(IdentityLink {
        phone_number_id: PhoneNumberId::new(phone_number_id),
        previous: row.try_get("previous").map_err(backend)?,
        current: row.try_get("current").map_err(backend)?,
        at: row.try_get("ts").map_err(backend)?,
    })
}

/// A `count(*)` column as a `u64`: never negative.
fn count(row: &PgRow, index: usize) -> Result<u64, StorageError> {
    let n: i64 = row.try_get(index).map_err(backend)?;
    Ok(u64::try_from(n).unwrap_or(0))
}

fn summary_from_row(row: &PgRow) -> Result<ConversationSummary, StorageError> {
    let phone_number_id: String = row.try_get("phone_number_id").map_err(backend)?;
    let contact: String = row.try_get("contact").map_err(backend)?;
    let unread: i64 = row.try_get("unread").map_err(backend)?;
    let last_text: Option<Vec<u8>> = row.try_get("last_text_utf8").map_err(backend)?;
    Ok(ConversationSummary {
        key: ConversationKey::new(phone_number_id, contact),
        last_message_at: row.try_get("last_message_at").map_err(backend)?,
        last_inbound_at: row.try_get("last_inbound_at").map_err(backend)?,
        last_text: last_text.map(|t| utf8("last_text_utf8", t)).transpose()?,
        // Never negative: it only grows by 1 and resets to 0.
        unread: u64::try_from(unread).unwrap_or(0),
    })
}

impl PostgresConversationStore {
    /// Run an append statement (`sql`) for `message`.
    async fn insert(&self, sql: &Arc<str>, message: &StoredMessage) -> Result<bool, StorageError> {
        let status = status_str(message.status)?;
        let payload = json_text(&message.payload, "message payload")?;
        let error = message
            .error
            .as_ref()
            .map(|e| json_text(e, "message error"))
            .transpose()?;
        let row = sqlx::query(AssertSqlSafe(Arc::clone(sql)))
            .bind(message.id.as_str())
            .bind(message.conversation.phone_number_id.as_str())
            .bind(message.conversation.contact.as_str())
            .bind(direction_str(message.direction))
            .bind(message.kind.as_bytes())
            .bind(message.text.as_deref().map(str::as_bytes))
            .bind(payload)
            .bind(status)
            .bind(message.timestamp)
            .bind(message.status_at)
            .bind(error)
            .bind(message.sender())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        Ok(row.is_some())
    }

    /// Move message `id` of `phone_number_id` (in `direction` only, when
    /// given) to `status` if that supersedes the stored one: see the module
    /// docs.
    async fn set_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        direction: Option<Direction>,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<&serde_json::Value>,
    ) -> Result<bool, StorageError> {
        let new = status_str(status)?;
        let error = error.map(|e| json_text(e, "message error")).transpose()?;
        let direction = direction.map(direction_str);
        for _ in 0..MAX_STATUS_ROUNDS {
            let current: Option<String> =
                sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.status_get)))
                    .bind(id.as_str())
                    .bind(phone_number_id.as_str())
                    .bind(direction)
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(backend)?;
            let Some(current) = current else {
                // A status for a message sent elsewhere, for another
                // business number's message, or a revoke of the other
                // direction.
                return Ok(false);
            };
            if !status.supersedes(parse_status(current.clone())?) {
                return Ok(false);
            }
            let done = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.status_swap)))
                .bind(id.as_str())
                .bind(phone_number_id.as_str())
                .bind(&current)
                .bind(&new)
                .bind(at)
                .bind(error.as_deref())
                .bind(direction)
                .execute(&self.pool)
                .await
                .map_err(backend)?;
            if done.rows_affected() == 1 {
                return Ok(true);
            }
            // Someone else moved the status between our read and our swap;
            // decide again against what they wrote.
        }
        // No message id: it is a Meta id derived from the customer's phone
        // number, and this error is logged.
        Err(StorageError::Backend(anyhow::anyhow!(
            "a message status kept changing; gave up after {MAX_STATUS_ROUNDS} rounds"
        )))
    }
}

#[async_trait]
impl ConversationStore for PostgresConversationStore {
    async fn append(&self, message: StoredMessage) -> Result<bool, StorageError> {
        self.insert(&self.sql.append, &message).await
    }

    async fn append_synced(&self, messages: Vec<StoredMessage>) -> Result<Vec<bool>, StorageError> {
        if messages.is_empty() {
            return Ok(Vec::new());
        }
        let n = messages.len();
        let mut ids = Vec::with_capacity(n);
        let mut numbers = Vec::with_capacity(n);
        let mut contacts = Vec::with_capacity(n);
        let mut directions = Vec::with_capacity(n);
        let mut kinds = Vec::with_capacity(n);
        let mut texts = Vec::with_capacity(n);
        let mut payloads = Vec::with_capacity(n);
        let mut statuses = Vec::with_capacity(n);
        let mut timestamps = Vec::with_capacity(n);
        let mut status_ats = Vec::with_capacity(n);
        let mut errors = Vec::with_capacity(n);
        let mut senders = Vec::with_capacity(n);
        for m in &messages {
            ids.push(m.id.as_str());
            numbers.push(m.conversation.phone_number_id.as_str());
            contacts.push(m.conversation.contact.as_str());
            directions.push(direction_str(m.direction));
            kinds.push(m.kind.as_bytes());
            texts.push(m.text.as_deref().map(str::as_bytes));
            payloads.push(json_text(&m.payload, "message payload")?);
            statuses.push(status_str(m.status)?);
            timestamps.push(m.timestamp);
            status_ats.push(m.status_at);
            errors.push(
                m.error
                    .as_ref()
                    .map(|e| json_text(e, "message error"))
                    .transpose()?,
            );
            senders.push(m.sender());
        }
        let inserted: Vec<String> =
            sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.append_synced)))
                .bind(&ids)
                .bind(&numbers)
                .bind(&contacts)
                .bind(&directions)
                .bind(&kinds)
                .bind(&texts)
                .bind(&payloads)
                .bind(&statuses)
                .bind(&timestamps)
                .bind(&status_ats)
                .bind(&errors)
                .bind(&senders)
                .fetch_all(&self.pool)
                .await
                .map_err(backend)?;
        // The first occurrence of each inserted id is the one inserted.
        let mut inserted: std::collections::HashSet<String> = inserted.into_iter().collect();
        Ok(messages
            .iter()
            .map(|m| inserted.remove(m.id.as_str()))
            .collect())
    }

    async fn revoke(
        &self,
        key: &ConversationKey,
        id: &MessageId,
        direction: Direction,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        let tombstone = StoredMessage::tombstone(key, id, direction, at);
        if self.insert(&self.sql.tombstone, &tombstone).await? {
            return Ok(true);
        }
        self.set_status(
            &key.phone_number_id,
            id,
            Some(direction),
            DeliveryStatus::Deleted,
            at,
            None,
        )
        .await
    }

    async fn fill_media_placeholder(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        kind: String,
        text: Option<String>,
        payload: serde_json::Value,
    ) -> Result<bool, StorageError> {
        let filled: i64 = sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.fill_placeholder)))
            .bind(id.as_str())
            .bind(phone_number_id.as_str())
            .bind(kind.into_bytes())
            .bind(text.map(String::into_bytes))
            .bind(json_text(&payload, "message payload")?)
            .bind(StoredMessage::MEDIA_PLACEHOLDER.as_bytes())
            .bind(status_str(DeliveryStatus::Deleted)?)
            .fetch_one(&self.pool)
            .await
            .map_err(backend)?;
        Ok(filled > 0)
    }

    async fn update_status(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
        status: DeliveryStatus,
        at: OffsetDateTime,
        error: Option<serde_json::Value>,
    ) -> Result<bool, StorageError> {
        self.set_status(phone_number_id, id, None, status, at, error.as_ref())
            .await
    }

    async fn messages(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, MessageId)>,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = match &before {
            None => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.messages))),
            Some(_) => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.messages_before))),
        }
        .bind(key.phone_number_id.as_str())
        .bind(key.contact.as_str())
        .bind(to_i64(limit));
        let query = match &before {
            None => query,
            Some((at, id)) => query.bind(*at).bind(id.as_str()),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(message_from_row).collect()
    }

    async fn conversations(
        &self,
        phone_number_id: &PhoneNumberId,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<ConversationSummary>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = match &before {
            None => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.conversations))),
            Some(_) => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.conversations_before))),
        }
        .bind(phone_number_id.as_str())
        .bind(to_i64(limit));
        let query = match &before {
            None => query,
            Some((at, contact)) => query.bind(*at).bind(contact.as_str()),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(summary_from_row).collect()
    }

    async fn mark_read(&self, key: &ConversationKey) -> Result<(), StorageError> {
        sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.mark_read)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }

    async fn last_inbound_at(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<OffsetDateTime>, StorageError> {
        let at: Option<Option<OffsetDateTime>> =
            sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.last_inbound_at)))
                .bind(key.phone_number_id.as_str())
                .bind(key.contact.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(backend)?;
        Ok(at.flatten())
    }

    async fn message(
        &self,
        phone_number_id: &PhoneNumberId,
        id: &MessageId,
    ) -> Result<Option<StoredMessage>, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.message)))
            .bind(id.as_str())
            .bind(phone_number_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(message_from_row).transpose()
    }

    async fn record_window_event(&self, event: WindowEvent) -> Result<bool, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.window_event)))
            .bind(event.conversation.phone_number_id.as_str())
            .bind(event.id.as_str())
            .bind(event.conversation.contact.as_str())
            .bind(event.kind.as_str())
            .bind(event.at)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        Ok(row.is_some())
    }

    async fn window_events(
        &self,
        key: &ConversationKey,
        before: Option<(OffsetDateTime, String)>,
        limit: usize,
    ) -> Result<Vec<WindowEvent>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = match &before {
            None => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.window_events))),
            Some(_) => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.window_events_before))),
        }
        .bind(key.phone_number_id.as_str())
        .bind(key.contact.as_str())
        .bind(to_i64(limit));
        let query = match &before {
            None => query,
            Some((at, id)) => query.bind(*at).bind(id.as_str()),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(window_event_from_row).collect()
    }

    async fn set_thread_owner(
        &self,
        key: &ConversationKey,
        ownership: ThreadOwnership,
    ) -> Result<bool, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.set_owner)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .bind(owner_str(ownership.owner)?)
            .bind(ownership.role.as_deref())
            .bind(ownership.app_id.as_ref().map(AppId::as_str))
            .bind(ownership.since)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        Ok(row.is_some())
    }

    async fn thread_owner(
        &self,
        key: &ConversationKey,
    ) -> Result<Option<ThreadOwnership>, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.owner)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(owner_from_row).transpose()
    }

    async fn put_contact(&self, contact: StoredContact) -> Result<bool, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.put_contact)))
            .bind(contact.key.phone_number_id.as_str())
            .bind(contact.key.contact.as_str())
            .bind(contact.full_name.as_deref().map(str::as_bytes))
            .bind(contact.first_name.as_deref().map(str::as_bytes))
            .bind(contact.phone_number.as_deref())
            .bind(contact.user_id.as_ref().map(UserId::as_str))
            .bind(contact.parent_user_id.as_ref().map(UserId::as_str))
            .bind(contact.username.as_deref().map(str::as_bytes))
            .bind(contact.synced_at)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        Ok(row.is_some())
    }

    async fn remove_contact(
        &self,
        key: &ConversationKey,
        at: OffsetDateTime,
    ) -> Result<bool, StorageError> {
        sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.remove_contact)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .bind(at)
            .fetch_one(&self.pool)
            .await
            .map_err(backend)
    }

    async fn contact(&self, key: &ConversationKey) -> Result<Option<StoredContact>, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.contact)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        row.as_ref().map(contact_from_row).transpose()
    }

    async fn contacts(
        &self,
        phone_number_id: &PhoneNumberId,
        after: Option<String>,
        limit: usize,
    ) -> Result<Vec<StoredContact>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let query = match &after {
            None => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.contacts))),
            Some(_) => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.contacts_after))),
        }
        .bind(phone_number_id.as_str())
        .bind(to_i64(limit));
        let query = match &after {
            None => query,
            Some(contact) => query.bind(contact.as_str()),
        };
        let rows = query.fetch_all(&self.pool).await.map_err(backend)?;
        rows.iter().map(contact_from_row).collect()
    }

    async fn link_identity(&self, link: IdentityLink) -> Result<bool, StorageError> {
        let row = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.link_identity)))
            .bind(link.phone_number_id.as_str())
            .bind(link.previous.as_str())
            .bind(link.current.as_str())
            .bind(link.at)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend)?;
        Ok(row.is_some())
    }

    async fn identity_links(
        &self,
        key: &ConversationKey,
    ) -> Result<Vec<IdentityLink>, StorageError> {
        let rows = sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.identity_links)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        rows.iter().map(link_from_row).collect()
    }

    async fn identities(&self, key: &ConversationKey) -> Result<BTreeSet<String>, StorageError> {
        let ids: Vec<String> = sqlx::query_scalar(AssertSqlSafe(Arc::clone(&self.sql.identities)))
            .bind(key.phone_number_id.as_str())
            .bind(key.contact.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(backend)?;
        Ok(ids.into_iter().collect())
    }

    async fn erase_all(
        &self,
        phone_number_id: &PhoneNumberId,
        contacts: &[String],
    ) -> Result<Erased, StorageError> {
        if contacts.is_empty() {
            return Ok(Erased::default());
        }
        let (sql, kind) = if self.erasure_mode == ErasureMode::Delete {
            (&self.sql.erase_deleting, StoredMessage::REVOKED)
        } else {
            // `Redact`, and any mode added later: never keep the content.
            (&self.sql.erase_redacting, StoredMessage::ERASED)
        };
        let mut tx = self.pool.begin().await.map_err(backend)?;
        let [purge, number] = &self.sql.erase_locks;
        sqlx::query(AssertSqlSafe(Arc::clone(purge)))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        sqlx::query(AssertSqlSafe(Arc::clone(number)))
            .bind(phone_number_id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        // An empty id names no one: it matches what is keyed by it, never a
        // contact's other ids nor a link's sides.
        let named: Vec<&str> = contacts
            .iter()
            .map(String::as_str)
            .filter(|id| !id.is_empty())
            .collect();
        let row = sqlx::query(AssertSqlSafe(Arc::clone(sql)))
            .bind(phone_number_id.as_str())
            .bind(contacts)
            .bind(kind.as_bytes())
            .bind(&named)
            .fetch_one(&mut *tx)
            .await
            .map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(Erased {
            messages: count(&row, 0)?,
            conversations: count(&row, 1)?,
            window_events: count(&row, 2)?,
            thread_owners: count(&row, 3)?,
            contacts: count(&row, 4)?,
            identity_links: count(&row, 5)?,
            group_messages: count(&row, 6)?,
        })
    }

    fn erasure_mode(&self) -> ErasureMode {
        self.erasure_mode
    }

    async fn purge_before(
        &self,
        phone_number_id: Option<&PhoneNumberId>,
        cutoff: OffsetDateTime,
    ) -> Result<Purged, StorageError> {
        let mut tx = self.pool.begin().await.map_err(backend)?;
        sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.purge_lock)))
            .execute(&mut *tx)
            .await
            .map_err(backend)?;
        let query = match phone_number_id {
            None => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.purge_all))).bind(cutoff),
            Some(number) => sqlx::query(AssertSqlSafe(Arc::clone(&self.sql.purge_number)))
                .bind(cutoff)
                .bind(number.as_str()),
        };
        let row = query.fetch_one(&mut *tx).await.map_err(backend)?;
        tx.commit().await.map_err(backend)?;
        Ok(Purged {
            messages: count(&row, 0)?,
            conversations: count(&row, 1)?,
            window_events: count(&row, 2)?,
            thread_owners: count(&row, 3)?,
            contact_removals: count(&row, 4)?,
        })
    }

    fn retention(&self) -> Retention {
        self.retention
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_round_trip_through_serde() {
        use DeliveryStatus as S;
        for s in [
            S::Received,
            S::Accepted,
            S::Sent,
            S::Delivered,
            S::Read,
            S::Played,
            S::Failed,
            S::Deleted,
        ] {
            let name = status_str(s).unwrap();
            assert_eq!(parse_status(name).unwrap(), s);
        }
        assert_eq!(status_str(S::Delivered).unwrap(), "delivered");
        assert!(matches!(
            parse_status("warning".to_owned()),
            Err(StorageError::Corrupt { .. })
        ));
    }

    #[test]
    fn owner_names_round_trip_through_serde() {
        for owner in [
            ThreadOwner::ThisApp,
            ThreadOwner::AnotherApp,
            ThreadOwner::Idle,
        ] {
            assert_eq!(parse_owner(owner_str(owner).unwrap()).unwrap(), owner);
        }
        assert!(matches!(
            parse_owner("somebody".to_owned()),
            Err(StorageError::Corrupt { .. })
        ));
    }

    /// The advisory locks are stable identifiers: replicas of two
    /// revisions must take the same ones (`docs/architecture.md`). `\x77`
    /// is `w`, so a search-and-replace of the prefix cannot rewrite this
    /// pin along with the code.
    #[test]
    fn the_locks_are_pinned() {
        let sql = Sql::new(&TablePrefix::DEFAULT);
        let number = "'\x77a_messages'::regclass::oid::int4, hashtext(";
        assert_eq!(
            sql.purge_lock.as_ref(),
            "SELECT pg_advisory_xact_lock('\x77a_conversations'::regclass::oid::int4, 0)"
        );
        assert_eq!(
            sql.erase_locks[0].as_ref(),
            "SELECT pg_advisory_xact_lock_shared('\x77a_conversations'::regclass::oid::int4, 0)"
        );
        assert_eq!(
            sql.erase_locks[1].as_ref(),
            format!("SELECT pg_advisory_xact_lock({number}$1))")
        );
        assert!(
            sql.append.starts_with(&format!(
                "WITH lock AS MATERIALIZED (SELECT pg_advisory_xact_lock_shared({number}$2))), "
            )),
            "{}",
            sql.append
        );
        assert!(
            sql.append_synced.contains(&format!(
                "SELECT pg_advisory_xact_lock_shared({number}number)) \
                 FROM unnest($2::text[]) AS number GROUP BY number ORDER BY number"
            )),
            "{}",
            sql.append_synced
        );
    }

    #[test]
    fn directions_round_trip() {
        for d in [Direction::Inbound, Direction::Outbound] {
            assert_eq!(parse_direction(direction_str(d)).unwrap(), d);
        }
        assert!(parse_direction("sideways").is_err());
    }
}
