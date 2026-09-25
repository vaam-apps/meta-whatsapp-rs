//! Reference code for the `meta-whatsapp-rs-storage` skill: the `KvStore` and
//! `ConversationStore` adapters (memory, Postgres, Redis), their upkeep,
//! and the conformance suites a store of your own must pass.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_rs::adapters::store::postgres::{self, PostgresKvStore, TablePrefix, sqlx};
use meta_whatsapp_rs::adapters::store::{PostgresConversationStore, RedisKvStore, redis};
use meta_whatsapp_rs::prelude::*;

/// Postgres for everything: one pool, migrated at startup.
pub async fn postgres_stores(
    database_url: &str,
) -> anyhow::Result<(Arc<dyn KvStore>, Arc<dyn ConversationStore>)> {
    let pool = sqlx::PgPool::connect(database_url).await?; // sqlx as meta-whatsapp-rs re-exports it
    postgres::migrate(&pool).await?; // idempotent, under a lock: any instance may run it
    let kv = PostgresKvStore::new(pool.clone());
    let purger = kv.clone();
    tokio::spawn(async move {
        // One row per webhook event (dedup) and per OTP: purge what expired.
        let mut every = tokio::time::interval(Duration::from_secs(600));
        loop {
            every.tick().await;
            if let Err(e) = purger.purge_expired().await {
                tracing::warn!(error = %e, "purging expired meta-whatsapp-rs rows failed");
            }
        }
    });
    Ok((Arc::new(kv), Arc::new(PostgresConversationStore::new(pool))))
}

/// Tables under another prefix than `wa_` (two deployments, one schema).
pub async fn prefixed(pool: sqlx::PgPool) -> anyhow::Result<PostgresKvStore> {
    let prefix = TablePrefix::new("shop_wa_")?;
    postgres::migrate_with_prefix(&pool, &prefix).await?;
    Ok(PostgresKvStore::with_prefix(pool, prefix))
}

/// Redis: persistence on, `maxmemory-policy noeviction`, an instance of its
/// own. `redis` is meta-whatsapp-rs's re-export: no redis dependency of your own.
pub async fn redis_kv(url: &str) -> anyhow::Result<Arc<dyn KvStore>> {
    let client = redis::Client::open(url)?; // `rediss://`: see the skill (your TLS feature and provider)
    let conn = client.get_connection_manager().await?;
    Ok(Arc::new(RedisKvStore::new(conn).with_prefix("shop:wa:")))
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;
    use meta_whatsapp_rs::adapters::store::conformance;
    use meta_whatsapp_rs::adapters::store::conversation_conformance;
    use meta_whatsapp_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
    use meta_whatsapp_rs::core::clock::ManualClock;

    // The contract every KvStore must keep: run it against yours, moving
    // the store's clock with the closure (a store that expires on a
    // server's clock uses `conformance::run_with_real_time` instead).
    #[tokio::test]
    async fn a_kv_store_passes_the_contract() {
        let clock = std::sync::Arc::new(ManualClock::new(datetime!(2026-09-24 12:00 UTC)));
        let store = MemoryKvStore::with_clock(clock.clone());
        conformance::run(&store, &|d| clock.advance(d)).await; // panics on a violation
    }

    #[tokio::test]
    async fn a_conversation_store_passes_the_contract() {
        conversation_conformance::run(&MemoryConversationStore::new()).await;
    }
}
