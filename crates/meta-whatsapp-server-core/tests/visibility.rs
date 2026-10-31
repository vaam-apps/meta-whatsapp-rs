//! What must not compile outside this crate, each with the compiler's
//! error code and text (`tests/visibility/fail/*.stderr`): the vault's
//! methods, the vault itself, the `Authorizer`'s admin writes, which only
//! the admin services reach (`authorizer_*`, roadmap S6), and a
//! capability of one's own (`src/authz.rs`, [`Tokens`]'s docs); a media
//! upload, or a download's limits, not made by their checks
//! (`src/media.rs`); the webhook
//! pipeline's sink, so that an event reaches the stores only through
//! `Pipeline::deliver`, after its signature (`src/inbound.rs`, roadmap
//! S5e); a rate class charged directly, rather than a request's
//! operation (`RateLimiter::check`: `src/ratelimit.rs`, roadmap S6).
//! `tests/visibility/pass/` is the control: it names every path those
//! cases use, and compiles.
//!
//! The expected errors are the pinned toolchain's (`rust-toolchain.toml`).
//! After changing it or the cases, regenerate them with
//! `TRYBUILD=overwrite cargo test -p meta-whatsapp-server-core --test visibility`
//! and review the diff: a case must still fail for its own reason.
//!
//! [`Tokens`]: meta_whatsapp_server_core::authz::Tokens

#[test]
fn what_other_crates_cannot_reach() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/visibility/pass/*.rs");
    cases.compile_fail("tests/visibility/fail/*.rs");
}
