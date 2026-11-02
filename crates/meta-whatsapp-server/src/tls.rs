//! rustls' process-default crypto provider (design D20 (b), roadmap S10).
//!
//! A crate that builds a rustls configuration either names its provider
//! (`ClientConfig::builder_with_provider`), as sqlx and the library's
//! `rediss://` and MongoDB adapters do with aws-lc-rs, or takes the
//! process default: `ClientConfig::builder()`, and reqwest, which uses the
//! default when one is installed and aws-lc-rs otherwise. With one
//! provider compiled into rustls the default is that one. A build with
//! CrateStack (the `api-cratestack` feature) compiles in two: CrateStack's
//! HTTP client brings `ring` beside aws-lc-rs, so rustls cannot choose,
//! `ClientConfig::builder()` panics, and CrateStack's client, when it is
//! constructed, installs `ring` as the default if nothing did first (which
//! the Graph client's reqwest would then use).
//!
//! sqlx names its provider itself (aws-lc-rs here; `ring` in a build with
//! CrateStack's sqlx crate, until upstream U2), except in one place: with
//! `sslmode=verify-ca` (the server's certificate checked against the CA,
//! not its host name) it builds the certificate verifier from the process
//! default, so in a build with both providers and none installed, every
//! connection to the database panics, the migrations' included.
//!
//! So the command line installs aws-lc-rs as the process default before
//! anything else ([`crate::cli::run_with`], for every command), and every
//! crate that takes the default gets the provider the rest of the tree
//! uses. An integrator who starts the service without the command line
//! ([`crate::serve::serve`] and its siblings, or `serve::connect`)
//! calls [`install_default_provider`] first.
//!
//! To swap the provider, install another one before the command line runs:
//! [`install_default_provider`] keeps a default that is already installed.

/// Installs aws-lc-rs as rustls' process-default crypto provider, unless a
/// provider is installed already, which it keeps (an integrator's choice).
/// Returns whether this call installed it.
///
/// [`crate::cli::run_with`] calls it first; an integrator who starts the
/// service without the command line (`serve::serve*`) calls it before.
pub fn install_default_provider() -> bool {
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        return false;
    }
    // Another thread may install one between the check and this call: the
    // first install wins, and a provider is the default either way.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .is_ok()
}
