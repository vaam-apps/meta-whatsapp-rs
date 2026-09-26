//! What must not compile outside this crate, each with the compiler's
//! error code and text (`tests/visibility/fail/*.stderr`): `AppState`'s
//! records and authorization order are crate-private. The core's own
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
