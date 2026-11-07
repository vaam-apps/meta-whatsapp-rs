//! The records of the groups each number holds
//! ([`crate::groups::GroupRecords`], roadmap M5f and M5f3) over a backend's
//! bundle: the typed store on its `KvStore`, with the per-number index kept
//! beside it. Purging a number's records takes its records and its index and
//! nothing of another number's (`a_purge_takes_a_numbers_records_and_index_and_no_others`),
//! and the three things that purge are the core's own: an operator's unbind of
//! a WABA (with a token that opens and with none), a tenant's deletion, and
//! the operator's request for one number, each leaving no key of the
//! namespaces that names the number (`unbinding_and_deleting_purge_the_numbers_records`).
//! The index is not a number's or a group's id in clear, and a deletion
//! prunes it. Against a backend such as Postgres this is the case the
//! decisive test of roadmap M5f3 runs live.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{GroupId, PhoneNumberId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_rs::core::testing::ScriptedTransport;
use serde_json::json;

use super::unique_digits;
use crate::admin::{Admin, mint};
use crate::authz::{AdminCaller, Authorizer};
use crate::backend::Backend;
use crate::groups::{GroupRecords, INDEX_NAMESPACE, NAMESPACE};
use crate::model::{KeyOwner, TenantId};
use crate::store::RecordStore;

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    a_purge_takes_a_numbers_records_and_index_and_no_others(backend).await;
    unbinding_and_deleting_purge_the_numbers_records(backend).await;
}

/// A key/value store that remembers every key of the groups' two
/// namespaces written through it (never forgetting one).
struct Tracked {
    inner: Arc<dyn KvStore>,
    keys: Mutex<BTreeSet<(String, String)>>,
}

impl Tracked {
    fn new(inner: Arc<dyn KvStore>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            keys: Mutex::new(BTreeSet::new()),
        })
    }

    fn note(&self, key: &StoreKey) {
        if key.namespace() == NAMESPACE || key.namespace() == INDEX_NAMESPACE {
            self.keys
                .lock()
                .unwrap()
                .insert((key.namespace().to_owned(), key.key().to_owned()));
        }
    }

    /// Every key of the namespaces written so far.
    fn written(&self) -> BTreeSet<(String, String)> {
        self.keys.lock().unwrap().clone()
    }

    /// Those that hold a record in the store now.
    async fn live(&self) -> BTreeSet<(String, String)> {
        let mut live = BTreeSet::new();
        for (namespace, key) in self.written() {
            let held = self
                .inner
                .get(&StoreKey::new(namespace.clone(), key.clone()))
                .await
                .unwrap();
            if held.is_some() {
                live.insert((namespace, key));
            }
        }
        live
    }
}

impl std::fmt::Debug for Tracked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracked").finish_non_exhaustive()
    }
}

#[async_trait]
impl KvStore for Tracked {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.inner.get(key).await
    }
    async fn put(&self, key: &StoreKey, v: Vec<u8>, e: Expiry) -> Result<u64, StorageError> {
        self.note(key);
        self.inner.put(key, v, e).await
    }
    async fn put_if_absent(
        &self,
        key: &StoreKey,
        v: Vec<u8>,
        e: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.note(key);
        self.inner.put_if_absent(key, v, e).await
    }
    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        e: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.note(key);
        self.inner.compare_and_swap(key, expected, new, e).await
    }
    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

/// The groups each number holds in the first case.
const GROUPS: usize = 300;

fn tenant(id: &str) -> TenantId {
    TenantId::parse(id).unwrap()
}

fn waba(id: &str) -> WabaId {
    WabaId::new(id).unwrap()
}

fn pn(id: &str) -> PhoneNumberId {
    PhoneNumberId::new(id).unwrap()
}

/// The groups of a case: ids as Meta's look (opaque, base64-like).
fn group(n: usize) -> GroupId {
    GroupId::new(format!("Y2FwaV9ncm91cDoxOTUwNTU1MDA3OT{n:04}"))
}

/// Which of `keys` name `number` (a record's key is `<number>.<hash>`).
fn naming(keys: &BTreeSet<(String, String)>, number: &PhoneNumberId) -> Vec<(String, String)> {
    keys.iter()
        .filter(|(_, key)| {
            key.starts_with(&format!("{}.", number.as_str())) || key == number.as_str()
        })
        .cloned()
        .collect()
}

/// A number's records (many, so every kind of shard is exercised), then a
/// purge of one number: its records and its index are gone, no key of the
/// namespaces names it, the index never named the number or a group, a
/// number's records of another number are untouched (still found, their
/// index entries too), a repeat deletes nothing, and a group forgotten
/// leaves neither record nor index entry.
///
/// Decisive: a purge that skips the index, deletes another number's keys,
/// or leaves the shard fails the live-key comparisons.
pub async fn a_purge_takes_a_numbers_records_and_index_and_no_others(backend: &dyn Backend) {
    let kv = Tracked::new(backend.kv());
    let records = GroupRecords::new(kv.clone());
    let who = tenant("grp-suite");
    let (mine, other) = (pn(&unique_digits()), pn(&unique_digits()));
    for n in 0..GROUPS {
        records.record(&who, &mine, &group(n)).await.unwrap();
    }
    let mine_keys = kv.written();
    for n in 0..GROUPS {
        records.record(&who, &other, &group(n)).await.unwrap();
    }
    let other_keys: BTreeSet<_> = kv.written().difference(&mine_keys).cloned().collect();
    assert!(!mine_keys.is_empty() && !other_keys.is_empty());
    assert_eq!(
        kv.live().await,
        kv.written(),
        "every key written is live before the purge"
    );
    // The index is hashes: it names neither number nor group.
    for (namespace, key) in kv.written() {
        if namespace == INDEX_NAMESPACE {
            assert!(
                !key.contains(mine.as_str()) && !key.contains(other.as_str()),
                "an index key names a number"
            );
            let value = backend
                .kv()
                .get(&StoreKey::new(namespace.clone(), key.clone()))
                .await
                .unwrap()
                .unwrap()
                .value;
            let text = String::from_utf8(value).unwrap();
            assert!(
                !text.contains(mine.as_str()) && !text.contains("Y2FwaV9ncm91"),
                "an index record names a number or a group: {text}"
            );
        }
    }

    // A group forgotten: its record and its index entry.
    records.forget(&mine, &group(0)).await.unwrap();
    assert!(
        !records
            .indexed(&mine, &GroupRecords::hash(&mine, &group(0)))
            .await
            .unwrap(),
        "a forgotten group stays in the index"
    );

    assert_eq!(records.purge(&mine).await.unwrap(), (GROUPS - 1) as u64);
    let live = kv.live().await;
    assert_eq!(
        naming(&live, &mine),
        Vec::new(),
        "a key of the namespaces still names the purged number"
    );
    assert!(
        live.is_disjoint(&mine_keys),
        "keys of the purged number are live: {:?}",
        live.intersection(&mine_keys).collect::<Vec<_>>()
    );
    assert_eq!(
        live, other_keys,
        "another number's records or index were touched"
    );
    assert_eq!(
        records.purge(&mine).await.unwrap(),
        0,
        "a repeat deletes nothing"
    );
    // The other number's records still answer, and a purge of it empties it.
    for n in [0, 1, GROUPS - 1] {
        let held = backend
            .kv()
            .get(&StoreKey::new(
                NAMESPACE,
                GroupRecords::key(&other, &group(n)),
            ))
            .await
            .unwrap();
        assert!(held.is_some(), "another number's record {n} is gone");
    }
    assert_eq!(records.purge(&other).await.unwrap(), GROUPS as u64);
    assert_eq!(kv.live().await, BTreeSet::new(), "nothing is left");
    // Not a number's id: nothing is touched.
    assert_eq!(records.purge(&pn("1")).await.unwrap(), 0);
}

/// The Graph rig and the service's authorizer over `backend`, group
/// records included.
struct Rig {
    authz: Authorizer,
    graph: ScriptedTransport,
    vault: TokenVault,
    records: Arc<dyn RecordStore>,
    groups: GroupRecords,
}

fn rig(backend: &dyn Backend, kv: Arc<dyn KvStore>) -> Rig {
    let graph = ScriptedTransport::new();
    let client = Client::builder()
        .transport(graph.clone())
        .retry(meta_whatsapp_rs::RetryPolicy::NONE)
        .build()
        .unwrap();
    let vault = TokenVault::new(
        backend.kv(),
        VaultKeys::new(VaultKey::generate("groups-suite").unwrap()),
    )
    .unwrap();
    let groups = GroupRecords::new(kv);
    let records = backend.records();
    let authz = Authorizer::new(records.clone(), vault.clone(), client)
        .unwrap()
        .with_group_records(groups.clone());
    Rig {
        authz,
        graph,
        vault,
        records,
        groups,
    }
}

async fn admin(rig: &Rig) -> AdminCaller {
    let (key, _) = mint(
        rig.records.as_ref(),
        KeyOwner::Admin,
        Vec::new(),
        String::new(),
        None,
    )
    .await
    .unwrap();
    rig.authz
        .admin_caller(Some(&format!("Bearer {}", key.expose_key())))
        .await
        .unwrap()
}

/// Bind `waba_id` to `tenant_id` with `numbers`, and store `token` for it
/// (when it has one), as an attach would.
async fn connect(
    rig: &Rig,
    tenant_id: &TenantId,
    waba_id: &str,
    numbers: &[&PhoneNumberId],
    token: bool,
) {
    let ids: Vec<PhoneNumberId> = numbers.iter().map(|n| (*n).clone()).collect();
    rig.records
        .bind_waba(tenant_id, &waba(waba_id), &ids)
        .await
        .unwrap();
    if token {
        rig.vault
            .store(
                &StoredBusinessToken::new(waba(waba_id), AccessToken::new("EAAG-groups-suite"))
                    .phone_number_ids(ids),
            )
            .await
            .unwrap();
    }
}

/// Decisive case of roadmap M5f3: after an operator's unbind (a WABA whose
/// token opens, one with none), after a tenant's deletion, and after the
/// operator's request for one number, no key of the namespaces names the
/// number, and the records of the numbers that stayed bound (another
/// WABA's, another tenant's) are untouched.
///
/// 1. Tenant A holds WABA 1 (numbers a1, a2, a token), WABA 2 (a3, a token)
///    and WABA 3 (a4, no token); tenant B holds WABA 4 (b1). Each number
///    holds ten groups.
/// 2. The operator unbinds WABA 1: a1 and a2 are purged; a3, a4 and b1 are
///    untouched.
/// 3. The operator unbinds WABA 3, whose token no longer opens (the path
///    that deletes without opening it): a4 purged.
/// 4. The operator asks for b1's records: purged, with the count; a repeat
///    finds none; a3 stays.
/// 5. Tenant A is deleted (WABA 2 is disconnected on the way): a3 purged.
///
/// Decisive: any one of the unbinding paths not purging fails its step.
pub async fn unbinding_and_deleting_purge_the_numbers_records(backend: &dyn Backend) {
    let kv = Tracked::new(backend.kv());
    let rig = rig(backend, kv.clone());
    let (a, b) = (
        tenant(&format!("grp-a-{}", &unique_digits()[20..])),
        tenant(&format!("grp-b-{}", &unique_digits()[20..])),
    );
    for t in [&a, &b] {
        rig.records.create_tenant(t, "").await.unwrap();
    }
    let ids: Vec<String> = (0..5).map(|_| unique_digits()).collect();
    let (w1, w2, w3, w4) = (
        ids[0].as_str(),
        ids[1].as_str(),
        ids[2].as_str(),
        ids[3].as_str(),
    );
    let (a1, a2, a3, a4, b1) = (
        pn(&unique_digits()),
        pn(&unique_digits()),
        pn(&unique_digits()),
        pn(&unique_digits()),
        pn(&unique_digits()),
    );
    connect(&rig, &a, w1, &[&a1, &a2], true).await;
    connect(&rig, &a, w2, &[&a3], true).await;
    connect(&rig, &a, w3, &[&a4], false).await;
    connect(&rig, &b, w4, &[&b1], true).await;
    for (who, number) in [(&a, &a1), (&a, &a2), (&a, &a3), (&a, &a4), (&b, &b1)] {
        for n in 0..10 {
            rig.groups.record(who, number, &group(n)).await.unwrap();
        }
    }
    let admin = admin(&rig).await;
    let service = Admin::new(&rig.authz, &admin).unwrap();
    let live = || kv.live();
    let named = |live: &BTreeSet<(String, String)>, number: &PhoneNumberId| naming(live, number);
    assert_eq!(named(&live().await, &a1).len(), 10);

    // 2. An operator's unbind with a token that opens.
    rig.graph.push_json(200, json!({"success": true}));
    service.unbind_waba(w1).await.unwrap();
    assert_eq!(rig.graph.remaining(), 0);
    let now = live().await;
    assert_eq!(named(&now, &a1), Vec::new(), "WABA 1's number a1");
    assert_eq!(named(&now, &a2), Vec::new(), "WABA 1's number a2");
    for kept in [&a3, &a4, &b1] {
        assert_eq!(named(&now, kept).len(), 10, "a bound number lost records");
    }

    // 3. No token that opens: the unbind that deletes without opening it.
    service.unbind_waba(w3).await.unwrap();
    let now = live().await;
    assert_eq!(named(&now, &a4), Vec::new(), "WABA 3's number a4");
    assert_eq!(named(&now, &a3).len(), 10);
    assert_eq!(named(&now, &b1).len(), 10);

    // 4. The operator's request for one number.
    assert_eq!(service.purge_group_records(b1.as_str()).await.unwrap(), 10);
    assert_eq!(service.purge_group_records(b1.as_str()).await.unwrap(), 0);
    let now = live().await;
    assert_eq!(named(&now, &b1), Vec::new(), "b1 after the request");
    assert_eq!(named(&now, &a3).len(), 10);
    let refused = service
        .purge_group_records("not-a-number")
        .await
        .unwrap_err();
    assert_eq!((refused.status(), refused.code()), (404, "not_found"));

    // 5. A tenant's deletion.
    rig.graph.push_json(200, json!({"success": true}));
    service.delete_tenant(a.as_str()).await.unwrap();
    assert_eq!(rig.graph.remaining(), 0);
    let now = live().await;
    assert_eq!(named(&now, &a3), Vec::new(), "the deleted tenant's number");
    // Everything that was written under the namespaces is gone: the index
    // shards too (none of them was left behind by any of the paths).
    assert_eq!(now, BTreeSet::new(), "a key of the namespaces is left");
}
