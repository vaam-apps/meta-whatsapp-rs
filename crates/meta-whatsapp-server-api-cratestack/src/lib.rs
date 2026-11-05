//! `meta-whatsapp-server-api-cratestack`: the meta-whatsapp-rs service's
//! API on CrateStack (docs/design/server.md, section 8.3; roadmap S10).
//!
//! The contract is a CrateStack schema, [`SCHEMA`] (`schema/api.cstack`,
//! design §8.4): procedures only (`datasource { provider = "none" }`,
//! compiled with CrateStack's `cratestack-api`), so it names no database
//! and runs over any backend. Its transport is REST (design D19): each
//! procedure is `POST /$procs/<name>`, JSON on the wire.
//!
//! As built in S10 it has one procedure, `version`, which answers what
//! `GET /v1/version` answers on the axum adapter, field for field, with no
//! key. It is not the default adapter, and nothing serves it yet: the
//! binary, `meta-whatsapp-server`, compiles it in with its non-default
//! `api-cratestack` feature, beside `meta-whatsapp-server-api-axum`, which
//! stays the default and keeps `openapi/v1.json`. Roadmap S13 brings the
//! rest of the routes, one resource group at a time, and the configuration
//! that picks this adapter; S16 flips the default.
//!
//! CrateStack's types stay inside this crate (design §8.5): [`router`]
//! returns an axum 0.8 router, the axum the facade re-exports.

use cratestack::axum::Router;
use cratestack::axum::http::HeaderMap;
use cratestack::{CratestackContext, CratestackError, JsonCodec};
use meta_whatsapp_rs::core::config::ApiVersion;
use meta_whatsapp_server_http::api::ops::API_VERSION;

/// The contract: the CrateStack schema this crate compiles.
pub const SCHEMA: &str = include_str!("../schema/api.cstack");

/// The largest request body a procedure reads. `version` takes no
/// arguments, so its body is `{}` or `{"args":{}}`; the bound is the
/// adapter's, not CrateStack's default (design §8.6: the per-route body
/// limits stay ours).
pub const BODY_LIMIT_BYTES: usize = 1024;

// The generated code: the types, the procedures' registry and the router.
// Not this crate's API, and not documented by it.
#[allow(missing_docs, clippy::all, clippy::pedantic)]
mod generated {
    cratestack::include_server_schema!("schema/api.cstack", db = None);
}

use generated::cratestack_schema;

/// The adapter's routes: `POST /$procs/version`, answering as
/// `GET /v1/version` does, with `graph_api_version` the version the
/// service's Graph client calls (`AppState::graph_api_version`).
///
/// Every caller is anonymous here: `version` needs no key. The routes that
/// do (S13) take their caller from the HTTP layer's authentication, which
/// runs before the body is read (design §8.6).
pub fn router(graph_api_version: ApiVersion) -> Router {
    let db = cratestack_schema::Cratestack::builder().build();
    let procedures = Procedures {
        graph_api_version: graph_api_version.to_string(),
    };
    cratestack_schema::axum::router(db, procedures, (), JsonCodec, anonymous, BODY_LIMIT_BYTES)
}

/// Every request's caller: anonymous, whatever its headers say.
#[allow(clippy::unnecessary_wraps)] // CrateStack's `AuthProvider` for a closure
fn anonymous(_: &HeaderMap) -> Result<CratestackContext, CratestackError> {
    Ok(CratestackContext::anonymous())
}

/// The procedures, with what they answer from.
#[derive(Debug, Clone)]
struct Procedures {
    /// `AppState::graph_api_version`, as text.
    graph_api_version: String,
}

impl cratestack_schema::procedures::ProcedureRegistry for Procedures {
    fn version(
        &self,
        _db: &cratestack_schema::Cratestack,
        _ctx: &CratestackContext,
        _args: cratestack_schema::procedures::version::Args,
        _authorized: cratestack_schema::procedures::version::Authorized,
    ) -> impl Future<
        Output = Result<cratestack_schema::procedures::version::Output, CratestackError>,
    > + Send {
        // As the HTTP layer's `GET /v1/version` (`api::ops::version`).
        std::future::ready(Ok(cratestack_schema::Version {
            server: env!("CARGO_PKG_VERSION").to_owned(),
            meta_whatsapp_rs_revision: option_env!("META_WHATSAPP_RS_REVISION")
                .unwrap_or("unknown")
                .to_owned(),
            graph_api_version: self.graph_api_version.clone(),
            api_version: API_VERSION.to_owned(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema declares what the crate documents: no model, one
    /// procedure, REST.
    #[test]
    fn the_schema_has_one_procedure_and_no_model_over_rest() {
        assert_eq!(cratestack_schema::MODEL_COUNT, 0);
        assert_eq!(cratestack_schema::PROCEDURE_COUNT, 1);
        assert_eq!(cratestack_schema::TRANSPORT_STYLE, "rest");
    }
}
