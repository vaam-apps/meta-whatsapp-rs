//! What must not compile outside this crate, each with the compiler's
//! error code and text (`tests/visibility/fail/*.stderr`): `AppState`'s
//! records, the backend it was composed from and its authorization order
//! are crate-private, and it is composed from a `Backend` alone (roadmap
//! S4: `app_state_from_loose_ports.rs`, `app_state_new.rs`, and
//! `inbound_stores.rs` for the webhook pipeline). So are the
//! handlers, which act on the capability they are given without asking
//! the authorizer again (`api_*.rs`: the security review of 3237040
//! called two of them with a forged one), and `idempotency::run`, which
//! runs over the service's own idempotency records (since roadmap S5b the
//! core's `idempotency::run`, which admits the `Caller` it is given). The
//! core's own
//! pins are `crates/meta-whatsapp-server-core/tests/visibility.rs`.
//! `tests/visibility/pass/` is the control: it names every path those
//! cases use, and compiles.
//!
//! The expected errors are the pinned toolchain's (`rust-toolchain.toml`).
//! After changing it or the cases, regenerate them with
//! `TRYBUILD=overwrite cargo test -p meta-whatsapp-server --all-features --test visibility`
//! and review the diff: a case must still fail for its own reason.

#[test]
fn what_other_crates_cannot_reach() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/visibility/pass/*.rs");
    cases.compile_fail("tests/visibility/fail/*.rs");
}
