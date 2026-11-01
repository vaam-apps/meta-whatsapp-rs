//! The names this spike stores under: its collections, their fields and
//! their indexes (roadmap S9). Once a database holds events under them,
//! renaming one strands that data, so they are stable identifiers
//! (docs/architecture.md, § Stable identifiers), **pending the owner's
//! confirmation before the first release**. The collections and the
//! fields the Postgres backend has a column for keep that column's name.
//!
//! - [`EVENTS`]: one document per event, `_id` its public id (`evt_…`).
//! - [`STREAMS`]: one document per stream, `_id` the tenant's id, or `""`
//!   for the operator-only rows: its last sequence and how far it was
//!   purged.
//! - [`WABAS`]: one document per bound WABA, `_id` its id: the projection
//!   of the bindings the insert checks (the crate's docs), and the counter
//!   each insert it admits increments ([`WABA_INSERT_CHECKS`]).

/// The events.
pub const EVENTS: &str = "wa_server_events";
/// The streams: each one's last sequence and purge mark.
pub const STREAMS: &str = "wa_server_event_streams";
/// The bindings of WABAs (and their numbers) to tenants, as the insert
/// reads them.
pub const WABAS: &str = "wa_server_wabas";

/// An event's stream: its tenant's id, or `""` (operator-only).
pub const EVENT_STREAM: &str = "stream";
/// Its sequence in its stream.
pub const EVENT_SEQUENCE: &str = "sequence";
/// Its tenant's id; absent for an operator-only row.
pub const EVENT_TENANT: &str = "tenant_id";
/// The dedup key it holds; absent when it has none or gave it up.
pub const EVENT_DEDUP_KEY: &str = "dedup_key";
/// Until when it holds its dedup key; absent: for as long as it is stored.
pub const EVENT_DEDUP_UNTIL: &str = "dedup_until";
/// The business number it names, when it names one.
pub const EVENT_PHONE_NUMBER: &str = "phone_number_id";
/// Its WABA, when it names one.
pub const EVENT_WABA: &str = "waba_id";
/// Its type.
pub const EVENT_TYPE: &str = "event_type";
/// Its JSON, as written (a string, so that it comes back byte for byte).
pub const EVENT_DATA: &str = "data";
/// The size of [`EVENT_DATA`] in bytes: a page is cut to its budget from
/// these before any data is read.
pub const EVENT_DATA_BYTES: &str = "data_bytes";
/// When the database received it (the server's clock).
pub const EVENT_CREATED_AT: &str = "created_at";

/// A stream's highest sequence, stored or purged.
pub const STREAM_LAST: &str = "last_sequence";
/// Every event of the stream up to it was purged.
pub const STREAM_PURGED: &str = "purged_through";

/// A WABA's tenant.
pub const WABA_TENANT: &str = "tenant_id";
/// When its binding began.
pub const WABA_ATTACHED_AT: &str = "attached_at";
/// When it was last unbound from another tenant; absent: never.
pub const WABA_PREVIOUS_TENANT_UNBOUND_AT: &str = "previous_tenant_unbound_at";
/// Its numbers' ids.
pub const WABA_NUMBERS: &str = "numbers";
/// How many inserts its binding admitted: the write that makes an
/// unbinding conflict with an insert in flight.
pub const WABA_INSERT_CHECKS: &str = "insert_checks";

/// Unique: one event per sequence of a stream, and the page's order.
pub const INDEX_STREAM_SEQUENCE: &str = "wa_server_events_stream_sequence";
/// Unique over the events holding a dedup key (a partial index).
pub const INDEX_DEDUP_KEY: &str = "wa_server_events_dedup_key";
/// Retention's cutoff (the Postgres index's name).
pub const INDEX_CREATED_AT: &str = "wa_server_events_created_idx";

#[cfg(test)]
mod tests {
    /// The stored names, byte for byte (the `w` spelled `\x77`, as every
    /// pin of docs/architecture.md § Stable identifiers is, so that a
    /// search-and-replace cannot rewrite them along with the code).
    /// Decisive: any of them renamed.
    #[test]
    fn the_stored_names_are_pinned() {
        use super::*;
        let names = [
            (EVENTS, "\x77a_server_events"),
            (STREAMS, "\x77a_server_event_streams"),
            (WABAS, "\x77a_server_\x77abas"),
            (EVENT_STREAM, "stream"),
            (EVENT_SEQUENCE, "sequence"),
            (EVENT_TENANT, "tenant_id"),
            (EVENT_DEDUP_KEY, "dedup_key"),
            (EVENT_DEDUP_UNTIL, "dedup_until"),
            (EVENT_PHONE_NUMBER, "phone_number_id"),
            (EVENT_WABA, "\x77aba_id"),
            (EVENT_TYPE, "event_type"),
            (EVENT_DATA, "data"),
            (EVENT_DATA_BYTES, "data_bytes"),
            (EVENT_CREATED_AT, "created_at"),
            (STREAM_LAST, "last_sequence"),
            (STREAM_PURGED, "purged_through"),
            (WABA_TENANT, "tenant_id"),
            (WABA_ATTACHED_AT, "attached_at"),
            (
                WABA_PREVIOUS_TENANT_UNBOUND_AT,
                "previous_tenant_unbound_at",
            ),
            (WABA_NUMBERS, "numbers"),
            (WABA_INSERT_CHECKS, "insert_checks"),
            (INDEX_STREAM_SEQUENCE, "\x77a_server_events_stream_sequence"),
            (INDEX_DEDUP_KEY, "\x77a_server_events_dedup_key"),
            (INDEX_CREATED_AT, "\x77a_server_events_created_idx"),
        ];
        for (name, pinned) in names {
            assert_eq!(name, pinned);
        }
    }
}
