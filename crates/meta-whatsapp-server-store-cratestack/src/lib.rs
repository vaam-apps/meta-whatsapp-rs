//! `meta-whatsapp-server-store-cratestack`: the meta-whatsapp-rs service's
//! CrateStack models on Postgres, a spike (docs/design/server.md, section
//! 8; roadmap S11).
//!
//! The contract is a CrateStack schema, [`SCHEMA`] (`schema/store.cstack`),
//! with one model, `WaProbe`. CrateStack applies a model's policies to
//! in-process calls too (its ADR 0018), so the store does not bypass them:
//! it calls every model as the system principal ([`system_context`],
//! CrateStack's `SystemContext`), and every model's `@@allow` names
//! `auth().isSystem()`. A tenant's context, which no header can turn into
//! the system principal, sees none of it.
//!
//! A spike: the binary does not compose it, and it implements none of the
//! core's ports. Roadmap S14 builds the hybrid store on what it shows. The
//! model's name and its table's (`wa_probes`) are **pending the owner's
//! confirmation before the first release**, like every stored name.
//!
//! CrateStack's types stay inside this crate (design §8.5) but the
//! contexts, which are CrateStack's `CratestackContext`: a spike's
//! shortcut S14 closes.

use cratestack::sqlx::PgPool;
use cratestack::{CratestackContext, SystemContext};

/// The contract: the CrateStack schema this crate compiles.
pub const SCHEMA: &str = include_str!("../schema/store.cstack");

/// The service's name as the system principal's audit actor
/// (`system:<service>`). Pending the owner's confirmation before the first
/// release, as it would be stored in an audit trail.
pub const SYSTEM_SERVICE: &str = "meta-whatsapp-server";

// The generated code: the model, its inputs, and the builder. Not this
// crate's API, and not documented by it.
#[allow(missing_docs, clippy::all, clippy::pedantic, dead_code)]
mod generated {
    cratestack::include_server_schema!("schema/store.cstack", db = Postgres);
}

use generated::cratestack_schema;

/// A probe row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The row's key.
    pub id: i64,
    /// The tenant it names.
    pub tenant_id: String,
    /// Free text.
    pub label: String,
}

/// The context the store calls models with: the system principal, named
/// after [`SYSTEM_SERVICE`]. Only server code can make one: nothing in a
/// request reaches it.
#[must_use]
pub fn system_context() -> CratestackContext {
    SystemContext::for_service(SYSTEM_SERVICE).into_context()
}

/// A context for a tenant, as the HTTP layer would build one from its
/// authenticated key: it carries the tenant's id and is not the system
/// principal.
#[must_use]
pub fn tenant_context(tenant_id: &str) -> CratestackContext {
    CratestackContext::authenticated([(
        "tenantId".to_owned(),
        cratestack::Value::String(tenant_id.to_owned()),
    )])
}

/// The `WaProbe` model over a pool.
#[derive(Clone)]
pub struct Probes {
    db: cratestack_schema::Cratestack,
}

impl std::fmt::Debug for Probes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Probes").finish_non_exhaustive()
    }
}

impl Probes {
    /// The model over `pool`.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            db: cratestack_schema::Cratestack::builder(pool).build(),
        }
    }

    /// Insert a row, as `ctx`.
    ///
    /// # Errors
    ///
    /// CrateStack's refusal (the policy denies `ctx`) or the database's
    /// failure, as its text.
    pub async fn create(&self, ctx: &CratestackContext, probe: Probe) -> Result<Probe, String> {
        self.db
            .wa_probe()
            .create(cratestack_schema::CreateWaProbeInput {
                id: probe.id,
                tenantId: probe.tenant_id,
                label: probe.label,
            })
            .run(ctx)
            .await
            .map(into_probe)
            .map_err(|e| e.to_string())
    }

    /// Every row `ctx` may list: `find_many`, the policy applied.
    ///
    /// # Errors
    ///
    /// CrateStack's refusal or the database's failure, as its text.
    pub async fn find_many(&self, ctx: &CratestackContext) -> Result<Vec<Probe>, String> {
        self.db
            .wa_probe()
            .find_many()
            .run(ctx)
            .await
            .map(|rows| rows.into_iter().map(into_probe).collect())
            .map_err(|e| e.to_string())
    }
}

fn into_probe(row: cratestack_schema::WaProbe) -> Probe {
    Probe {
        id: row.id,
        tenant_id: row.tenantId,
        label: row.label,
    }
}
