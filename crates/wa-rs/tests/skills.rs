//! The consumer skills in `skills/` instruct the coding agents of the
//! repositories that use wa-rs. Their example code, `skills/<name>/examples/*.rs`,
//! is compiled into this test crate (see `tests/skill_examples/mod.rs`) and
//! its tests run here, against this commit's API.

#![allow(clippy::unwrap_used, clippy::expect_used)]

// Every example needs one feature or another; `just check`, `just lint` and
// `just test` build with all of them.
#[cfg(all(
    feature = "reqwest",
    feature = "memory",
    feature = "sinks",
    feature = "postgres",
    feature = "redis",
    feature = "axum",
    feature = "typst",
    feature = "flows-endpoint"
))]
mod skill_examples;
