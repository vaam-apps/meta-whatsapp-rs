//! What must not compile outside this crate, each with the compiler's
//! error code and text (`tests/visibility/fail/*.stderr`): the handlers,
//! which act on the capability they are given without asking the
//! authorizer again (`api_*.rs`: the security review of 3237040 called two
//! of them with a forged one). Another crate reaches them only through
//! `api::internal_router`, behind its guards, which is served with the
//! state's grant (the HTTP crate's pins:
//! `crates/meta-whatsapp-server-http/tests/visibility.rs`).
//! `tests/visibility/pass/` is the control: it names every path those
//! cases use, and compiles. (The service's `tests/visibility.rs` until
//! roadmap S7 split the crates.)
//!
//! The expected errors are the pinned toolchain's (`rust-toolchain.toml`).
//! After changing it or the cases, regenerate them with
//! `TRYBUILD=overwrite cargo test -p meta-whatsapp-server-api-axum --all-features --test visibility`
//! and review the diff: a case must still fail for its own reason.

#[test]
fn what_other_crates_cannot_reach() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/visibility/pass/*.rs");
    cases.compile_fail("tests/visibility/fail/*.rs");
}
