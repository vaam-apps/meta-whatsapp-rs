//! What must not compile outside this crate, each with the compiler's
//! error code and text (`tests/visibility/fail/*.stderr`): `AppState`'s
//! records and its authorization order are crate-private, it no longer
//! holds the backend it was composed from, and it is composed from a
//! `Backend` alone (roadmap S4: `app_state_from_loose_ports.rs`,
//! `app_state_new.rs`, and `inbound_stores.rs` for the webhook pipeline).
//! So is `idempotency::run`, which runs over the service's own idempotency
//! records (since roadmap S5b the core's `idempotency::run`, which admits
//! the `Caller` it is given). An API adapter reaches them through the
//! state's `Grant`, which only composing the state makes
//! (`grant_from_state.rs`; no `From` or `FromRef` conversion of a state
//! makes one either, `grant_from_conversions.rs`: roadmap S7). The API
//! adapter's handlers are pinned in their own crate
//! (`crates/meta-whatsapp-server-api-axum/tests/visibility.rs`), the
//! core's in `crates/meta-whatsapp-server-core/tests/visibility.rs`.
//! `tests/visibility/pass/` is the control: it names every path those
//! cases use, and compiles. (The service's `tests/visibility.rs` until
//! roadmap S7 split the crates.)
//!
//! The expected errors are the pinned toolchain's (`rust-toolchain.toml`).
//! After changing it or the cases, regenerate them with
//! `TRYBUILD=overwrite cargo test -p meta-whatsapp-server-http --all-features --test visibility`
//! and review the diff: a case must still fail for its own reason.

#[test]
fn what_other_crates_cannot_reach() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/visibility/pass/*.rs");
    cases.compile_fail("tests/visibility/fail/*.rs");
}
