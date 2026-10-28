//! Roadmap S5c: the core's template services ([`crate::templates`]) over
//! a backend's bundle, with no HTTP layer: the core's [`Authorizer`] over
//! the backend's records and a vault on its key/value store (every read
//! of which is recorded, `VaultReads`), and a Graph client on a
//! [`ScriptedTransport`]. Another tenant's template id is not found, with
//! zero vault reads: tenant B's caller reaches neither tenant A's WABA
//! (step 4, before the vault) nor A's capability (the services open only
//! on a WABA of the caller's own tenant), and through its own WABA an id
//! of A's is `404 not_found`, without Meta's code or text, nothing of it
//! answered or deleted, A's vault record never read. The services open
//! only on capabilities their own [`Authorizer`] made. And the
//! operations send Meta's documented requests, with the WABA's token: the
//! list (cached per WABA, dropped by a creation or deletion), the lookup,
//! the creation (the definition checked before any request), the
//! deletions.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use meta_whatsapp_rs::Client;
use meta_whatsapp_rs::client::embedded_signup::{
    StoredBusinessToken, TokenVault, VaultKey, VaultKeys,
};
use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, TemplateId, WabaId};
use meta_whatsapp_rs::core::secret::AccessToken;
use meta_whatsapp_rs::core::store::{Expiry, KvStore, StoreKey, Versioned};
use meta_whatsapp_rs::core::testing::{RecordedRequest, ScriptedTransport};
use serde_json::{Value, json};

use super::messages::Told;
use crate::admin::mint;
use crate::authz::{Authorizer, Caller, OwnedWaba};
use crate::backend::Backend;
use crate::model::{BindOutcome, KeyOwner, Scope, TenantId};
use crate::templates::{
    self, TEMPLATE_FIELDS, Template, TemplateCache, TemplateCreated, TemplateFilter, Templates,
};

/// The tenant holding the template, its WABA, its number and its token.
const HOLDER: Tenant = Tenant {
    id: "tpl-holder",
    waba: "102290129340398",
    pn: "106540352242922",
    token: "EAAG-the-holders-token",
};
/// The other tenant, whose caller asks for the holder's template.
const OTHER: Tenant = Tenant {
    id: "tpl-other",
    waba: "102290129340399",
    pn: "106540352242923",
    token: "EAAG-the-other-tenants-token",
};
/// The tenant of the second case (the cases share a backend: [`run`]).
const MANAGER: Tenant = Tenant {
    id: "tpl-manager",
    waba: "102290129340400",
    pn: "106540352242924",
    token: "EAAG-the-managers-token",
};
/// templates/template-management, "Get all templates": its first
/// template's id and name.
const TEMPLATE_ID: &str = "1304694804498707";
const TEMPLATE_NAME: &str = "coupon_expiration_reminder_number_vars";

/// Every case, on one backend.
pub async fn run(backend: &dyn Backend) {
    another_tenants_template_id_is_not_found_with_zero_vault_reads(backend).await;
    the_template_services_send_metas_documented_requests(backend).await;
}

/// A tenant of a case: its id, its WABA and number, and its token.
#[derive(Clone, Copy)]
struct Tenant {
    id: &'static str,
    waba: &'static str,
    pn: &'static str,
    token: &'static str,
}

/// A key/value store recording every key read through it (the vault's
/// `get`s), over the backend's own.
#[derive(Debug)]
pub(crate) struct VaultReads {
    inner: Arc<dyn KvStore>,
    reads: Mutex<Vec<String>>,
}

impl VaultReads {
    pub(crate) fn new(inner: Arc<dyn KvStore>) -> Self {
        Self {
            inner,
            reads: Mutex::new(Vec::new()),
        }
    }

    /// The keys read so far, `namespace/key`.
    pub(crate) fn reads(&self) -> Vec<String> {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl KvStore for VaultReads {
    async fn get(&self, key: &StoreKey) -> Result<Option<Versioned>, StorageError> {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{}/{}", key.namespace(), key.key()));
        self.inner.get(key).await
    }

    async fn put(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<u64, StorageError> {
        self.inner.put(key, value, expiry).await
    }

    async fn put_if_absent(
        &self,
        key: &StoreKey,
        value: Vec<u8>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.inner.put_if_absent(key, value, expiry).await
    }

    async fn compare_and_swap(
        &self,
        key: &StoreKey,
        expected: u64,
        new: Option<Vec<u8>>,
        expiry: Expiry,
    ) -> Result<Option<u64>, StorageError> {
        self.inner
            .compare_and_swap(key, expected, new, expiry)
            .await
    }

    async fn delete(&self, key: &StoreKey) -> Result<bool, StorageError> {
        self.inner.delete(key).await
    }
}

/// A request's query, as its pairs in order.
pub(crate) fn query(request: &RecordedRequest) -> Vec<(String, String)> {
    request
        .url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// `pairs` as owned strings, for comparing with [`query`].
pub(crate) fn pairs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// templates/template-management, "Get all templates": its first
/// template, and a next page.
fn documented_list() -> Value {
    json!({
        "data": [{
            "name": TEMPLATE_NAME,
            "parameter_format": "POSITIONAL",
            "components": [
                {"type": "HEADER", "format": "TEXT", "text": "Act fast, {{1}}!", "example": {"header_text": ["Pablo"]}},
                {"type": "BODY", "text": "Just a quick reminder—your exclusive coupon code, {{1}}, *expires in only {{2}} days!*",
                 "example": {"body_text": [["SUMMER20", "10"]]}},
                {"type": "FOOTER", "text": "Lucky Shrub Succulents"},
                {"type": "BUTTONS", "buttons": [
                    {"type": "URL", "text": "See deals", "url": "https://www.luckyshrub.com/deals"},
                    {"type": "QUICK_REPLY", "text": "Unsubscribe"}
                ]}
            ],
            "language": "en",
            "status": "APPROVED",
            "category": "MARKETING",
            "sub_category": "CUSTOM",
            "id": TEMPLATE_ID
        }],
        "paging": {"cursors": {"before": "QVFIU...", "after": "QVFIUafter"},
                   "next": "https://graph.facebook.com/v23.0/10229..."}
    })
}

/// The documented template, as the services answer it.
fn documented_template() -> Template {
    Template {
        id: TemplateId::new(TEMPLATE_ID).unwrap(),
        name: Some(TEMPLATE_NAME.to_owned()),
        language: Some("en".to_owned()),
        status: Some("APPROVED".to_owned()),
        category: Some("MARKETING".to_owned()),
        components: documented_list()["data"][0]["components"]
            .as_array()
            .unwrap()
            .clone(),
    }
}

/// The service as the cases see it: the order over the backend's records
/// and a vault whose reads are recorded, its Graph script, the cache, and
/// what it told the metrics.
struct Service {
    authz: Authorizer,
    vault: TokenVault,
    reads: Arc<VaultReads>,
    script: ScriptedTransport,
    cache: TemplateCache,
    told: Told,
}

impl Service {
    /// The template services on `waba` for `caller`, observed.
    fn templates<'a>(&'a self, caller: &'a Caller, waba: &'a OwnedWaba) -> Templates<'a> {
        Templates::new(&self.authz, caller, waba, &self.cache)
            .unwrap()
            .observed_by(&self.told)
    }

    /// Another [`Authorizer`] over the same records and vault, with a
    /// Graph client of its own.
    fn other(&self, backend: &dyn Backend) -> (Authorizer, ScriptedTransport) {
        let script = ScriptedTransport::new();
        let client = Client::builder().transport(script.clone()).build().unwrap();
        let other = Authorizer::new(backend.records(), self.vault.clone(), client).unwrap();
        (other, script)
    }
}

/// The order over the backend's records and a vault on its key/value
/// store, every read recorded; tokens stored by [`tenant`].
fn service(backend: &dyn Backend) -> Service {
    let reads = Arc::new(VaultReads::new(backend.kv()));
    let vault = TokenVault::new(
        reads.clone(),
        VaultKeys::new(VaultKey::generate("templates-suite").unwrap()),
    )
    .unwrap();
    let script = ScriptedTransport::new();
    let client = Client::builder().transport(script.clone()).build().unwrap();
    let authz = Authorizer::new(backend.records(), vault.clone(), client).unwrap();
    Service {
        authz,
        vault,
        reads,
        script,
        cache: TemplateCache::new(Duration::from_secs(60)),
        told: Told::default(),
    }
}

/// `tenant` with a `templates` key, its WABA and number bound, and its
/// token in the vault; the caller `authz` admits for that key.
async fn tenant(backend: &dyn Backend, service: &Service, tenant: Tenant) -> Caller {
    let records = backend.records();
    let id = TenantId::parse(tenant.id).unwrap();
    records.create_tenant(&id, "").await.unwrap();
    let waba = WabaId::new(tenant.waba).unwrap();
    let pn = PhoneNumberId::new(tenant.pn).unwrap();
    let bound = records
        .bind_waba(&id, &waba, std::slice::from_ref(&pn))
        .await
        .unwrap();
    assert_eq!(bound, BindOutcome::Bound);
    service
        .vault
        .store(
            &StoredBusinessToken::new(waba, AccessToken::new(tenant.token)).phone_number_ids([pn]),
        )
        .await
        .unwrap();
    caller_of(backend, &service.authz, tenant.id).await
}

/// A `templates` key for `tenant` that `authz` admits.
async fn caller_of(backend: &dyn Backend, authz: &Authorizer, tenant: &str) -> Caller {
    let key = mint(
        backend.records().as_ref(),
        KeyOwner::Tenant(TenantId::parse(tenant).unwrap()),
        vec![Scope::Templates],
        String::new(),
        None,
    )
    .await
    .unwrap()
    .0;
    authz
        .tenant_caller(
            Some(&format!("Bearer {}", key.expose_key())),
            None,
            Scope::Templates,
        )
        .await
        .unwrap()
}

/// Whether a vault key names `tenant`'s WABA or number.
fn names(key: &str, tenant: Tenant) -> bool {
    key.ends_with(&format!("/{}", tenant.waba)) || key.ends_with(&format!("/{}", tenant.pn))
}

/// The decisive case of roadmap S5c, with no HTTP layer: tenant B's
/// caller on tenant A's template id is not found, with zero vault reads.
///
/// 1. The control: A's caller, on A's WABA, finds A's template, through
///    Meta's documented requests with A's token (the bare id asked for
///    its name only; the WABA's own list of that name).
/// 2. B's caller naming A's WABA is `404 not_found` (step 4 of the
///    order), with zero vault reads and no request.
/// 3. B's caller handed A's capability: the template services refuse to
///    open, `404 not_found`, with zero vault reads and no request.
/// 4. B's caller, on B's own WABA, asking for A's template id: `404
///    not_found`, without Meta's code or text, its template never
///    answered. Meta was asked with B's token only (the id's name, then
///    B's WABA's list of that name, which lacks it), and the lookup read
///    the vault zero times; Meta refusing the bare id is `404` too, its
///    refusal told to the [`Observer`](crate::observe::Observer).
/// 5. B deleting A's template by name and id through B's WABA: `404
///    not_found`, nothing deleted (no `DELETE` sent), zero vault reads.
/// 6. Another [`Authorizer`]'s caller, or its capability, for B's own
///    WABA: `403 forbidden`, with zero vault reads and no request on
///    either order's client.
///
/// Through 2 to 6, A's vault record and phone index are never read.
///
/// Decisive: the ownership filter of `Authorizer::owned_waba` (step 4)
/// dropped fails step 2 (a capability made, the vault read); the
/// tenant's check of `Templates::new` dropped fails step 3 (A's template
/// answered to B, with A's token); the lookup through the WABA's own list
/// dropped (the bare id trusted) fails step 4; the lookup before a
/// deletion by id dropped fails step 5 (a `DELETE` sent, which nothing
/// answers); the admission of the caller or of the WABA dropped fails
/// step 6.
pub async fn another_tenants_template_id_is_not_found_with_zero_vault_reads(backend: &dyn Backend) {
    let service = service(backend);
    let script = &service.script;
    let holder = tenant(backend, &service, HOLDER).await;
    let other = tenant(backend, &service, OTHER).await;
    let holders_waba = service
        .authz
        .owned_waba(&holder, WabaId::new(HOLDER.waba).unwrap())
        .await
        .unwrap();
    let others_waba = service
        .authz
        .owned_waba(&other, WabaId::new(OTHER.waba).unwrap())
        .await
        .unwrap();

    // 1. The control: the holder's template, the holder's token.
    script.push_json(200, json!({"name": TEMPLATE_NAME, "id": TEMPLATE_ID}));
    script.push_json(200, documented_list());
    let found = service
        .templates(&holder, &holders_waba)
        .get(TEMPLATE_ID)
        .await
        .unwrap();
    assert_eq!(found, documented_template());
    let requests = script.requests();
    let [named, listed] = &requests[..] else {
        panic!("two requests: {requests:?}")
    };
    assert_eq!(
        (named.method.as_str(), named.path()),
        ("GET", format!("/v25.0/{TEMPLATE_ID}").as_str())
    );
    assert_eq!(query(named), pairs(&[("fields", "name")]));
    assert_eq!(named.bearer(), Some(HOLDER.token));
    assert_eq!(
        (listed.method.as_str(), listed.path()),
        (
            "GET",
            format!("/v25.0/{}/message_templates", HOLDER.waba).as_str()
        )
    );
    assert_eq!(
        query(listed),
        pairs(&[
            ("fields", &TEMPLATE_FIELDS.join(",")),
            ("limit", "100"),
            ("name", TEMPLATE_NAME),
        ])
    );
    assert_eq!(listed.bearer(), Some(HOLDER.token));
    assert_eq!(script.remaining(), 0);

    // From here, everything is B's: A's records are never read again.
    let before = service.reads.reads().len();
    let sent = || script.requests().len();
    let reads_since = |from: usize| service.reads.reads()[from..].to_vec();

    // 2. B's caller names A's WABA: not found, before the vault.
    let refused = service
        .authz
        .owned_waba(&other, WabaId::new(HOLDER.waba).unwrap())
        .await
        .unwrap_err();
    assert_eq!((refused.status(), refused.code()), (404, "not_found"));
    assert_eq!(reads_since(before), Vec::<String>::new(), "a vault read");
    assert_eq!(sent(), 2, "a request was sent");

    // 3. B's caller handed A's capability: the services do not open.
    let refused = Templates::new(&service.authz, &other, &holders_waba, &service.cache)
        .map(drop)
        .unwrap_err();
    assert_eq!((refused.status(), refused.code()), (404, "not_found"));
    assert_eq!(reads_since(before), Vec::<String>::new(), "a vault read");
    assert_eq!(sent(), 2, "a request was sent");

    // 4. B's own WABA, A's template id: not B's WABA's, not found.
    let theirs = service.templates(&other, &others_waba);
    script.push_json(200, json!({"name": TEMPLATE_NAME, "id": TEMPLATE_ID}));
    script.push_json(
        200,
        json!({"data": [{"id": "1304694804498708", "name": TEMPLATE_NAME, "language": "de"}]}),
    );
    let missing = theirs.get(TEMPLATE_ID).await.unwrap_err();
    assert_eq!((missing.status(), missing.code()), (404, "not_found"));
    assert!(missing.graph().is_none(), "Meta's error: {missing:?}");
    let requests = script.requests();
    let [.., named, listed] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(named.path(), format!("/v25.0/{TEMPLATE_ID}"));
    assert_eq!(named.bearer(), Some(OTHER.token), "B's token");
    assert_eq!(
        listed.path(),
        format!("/v25.0/{}/message_templates", OTHER.waba),
        "B's WABA's own list"
    );
    assert_eq!(listed.query("name").as_deref(), Some(TEMPLATE_NAME));
    assert_eq!(listed.bearer(), Some(OTHER.token), "B's token");
    // Meta refusing the bare id (not one its token reaches): not found,
    // without Meta's code or text, and told.
    script.push_json(
        400,
        json!({"error": {"message": "Unsupported get request. Object with ID '1304694804498707' does not exist, cannot be loaded due to missing permissions, or does not support this operation.",
                         "type": "GraphMethodException", "code": 100, "error_subcode": 33,
                         "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn"}}),
    );
    let refused = theirs.get(TEMPLATE_ID).await.unwrap_err();
    assert_eq!((refused.status(), refused.code()), (404, "not_found"));
    assert!(refused.graph().is_none(), "Meta's error: {refused:?}");
    assert_eq!(service.told.graph_errors(), ["invalid_parameter"]);
    assert_eq!(sent(), 5);
    assert_eq!(script.remaining(), 0);
    assert_eq!(reads_since(before), Vec::<String>::new(), "a vault read");

    // 5. B deleting A's template by id through B's WABA: nothing deleted.
    script.push_json(200, json!({"data": []}));
    let missing = theirs
        .delete(TEMPLATE_NAME, Some(TEMPLATE_ID))
        .await
        .unwrap_err();
    assert_eq!((missing.status(), missing.code()), (404, "not_found"));
    let looked_up = script.last_request().unwrap();
    assert_eq!(
        (looked_up.method.as_str(), looked_up.path()),
        (
            "GET",
            format!("/v25.0/{}/message_templates", OTHER.waba).as_str()
        )
    );
    assert_eq!(
        query(&looked_up),
        pairs(&[
            ("fields", "name"),
            ("limit", "100"),
            ("name", TEMPLATE_NAME)
        ])
    );
    assert_eq!(sent(), 6, "a DELETE was sent");
    assert_eq!(script.remaining(), 0);
    assert_eq!(reads_since(before), Vec::<String>::new(), "a vault read");

    // 6. Another order's caller or capability: refused.
    let (rival, rival_script) = service.other(backend);
    let rivals_caller = caller_of(backend, &rival, OTHER.id).await;
    let rivals_waba = rival
        .owned_waba(&rivals_caller, WabaId::new(OTHER.waba).unwrap())
        .await
        .unwrap();
    let before_refusals = service.reads.reads().len();
    for (opened, which) in [
        (
            Templates::new(&service.authz, &rivals_caller, &others_waba, &service.cache),
            "their caller",
        ),
        (
            Templates::new(&service.authz, &other, &rivals_waba, &service.cache),
            "their capability",
        ),
        (
            Templates::new(&rival, &other, &others_waba, &service.cache),
            "through their order",
        ),
    ] {
        let refused = opened.map(drop).unwrap_err();
        assert_eq!(
            (refused.status(), refused.code()),
            (403, "forbidden"),
            "{which}"
        );
    }
    assert_eq!(
        reads_since(before_refusals),
        Vec::<String>::new(),
        "a vault read"
    );
    assert_eq!(sent(), 6, "a request was sent");
    assert!(rival_script.requests().is_empty());

    // Through 2 to 6, nothing of A's was read from the vault (the other
    // order's opening of B's WABA read B's records only).
    let read_of_a: Vec<String> = reads_since(before)
        .into_iter()
        .filter(|key| names(key, HOLDER))
        .collect();
    assert_eq!(read_of_a, Vec::<String>::new(), "A's vault record was read");
}

/// The template services send Meta's documented requests with the WABA's
/// token, and answer Meta's documented examples.
///
/// 1. A list: `GET /{waba}/message_templates` with the template fields
///    and the page size, the documented page answered field by field,
///    with Meta's cursor; the same query again comes from the cache (no
///    request); filters (the status upper-cased) and the cursor reach Meta
///    as its parameters; a malformed filter is refused before any
///    request.
/// 2. A definition breaking a documented limit is refused on its field
///    before any request; a creation posts the definition as given and
///    answers Meta's id, status and category, and drops the WABA's cached
///    pages (the list asks Meta again).
/// 3. A deletion by name sends Meta's `DELETE` with the name; by name and
///    id, the lookup first, then the `DELETE` with `hsm_id`; a malformed
///    name or id is refused before any request; a deletion drops the
///    WABA's cached pages.
///
/// Decisive: the cache's lookup dropped fails step 1's repeat (a second
/// request, which nothing answers); the invalidation after a creation or
/// a deletion dropped fails steps 2 and 3 (a page answered from the
/// cache, the script's answer left over); the definition's check dropped
/// fails step 2 (a request sent).
pub async fn the_template_services_send_metas_documented_requests(backend: &dyn Backend) {
    let service = service(backend);
    let script = &service.script;
    let manager = tenant(backend, &service, MANAGER).await;
    let waba = service
        .authz
        .owned_waba(&manager, WabaId::new(MANAGER.waba).unwrap())
        .await
        .unwrap();
    let templates = service.templates(&manager, &waba);
    let list_path = format!("/v25.0/{}/message_templates", MANAGER.waba);
    let fields = TEMPLATE_FIELDS.join(",");

    // 1. The list, then the cache.
    script.push_json(200, documented_list());
    let page = templates
        .list(TemplateFilter::default(), None, 50)
        .await
        .unwrap();
    assert_eq!(page.templates, [documented_template()]);
    assert_eq!(page.next_after.as_deref(), Some("QVFIUafter"));
    let request = script.last_request().unwrap();
    assert_eq!(
        (request.method.as_str(), request.path()),
        ("GET", list_path.as_str())
    );
    assert_eq!(
        query(&request),
        pairs(&[("fields", &fields), ("limit", "50")])
    );
    assert_eq!(request.bearer(), Some(MANAGER.token));
    let again = templates
        .list(TemplateFilter::default(), None, 50)
        .await
        .unwrap();
    assert_eq!(again, page, "the cached page");
    assert_eq!(script.requests().len(), 1, "the cache asked Meta");
    script.push_json(200, json!({"data": []}));
    let filtered = templates
        .list(
            TemplateFilter {
                status: Some("approved".to_owned()),
                name: Some("order_confirmation".to_owned()),
            },
            Some("QVFIUafter".to_owned()),
            5,
        )
        .await
        .unwrap();
    assert!(filtered.templates.is_empty());
    assert_eq!(filtered.next_after, None);
    assert_eq!(
        query(&script.last_request().unwrap()),
        pairs(&[
            ("fields", &fields),
            ("limit", "5"),
            ("name", "order_confirmation"),
            ("status", "APPROVED"),
            ("after", "QVFIUafter"),
        ])
    );
    for (filter, field) in [
        (
            TemplateFilter {
                status: Some("a&b".to_owned()),
                name: None,
            },
            "status",
        ),
        (
            TemplateFilter {
                status: None,
                name: Some("Order Confirmation".to_owned()),
            },
            "name",
        ),
    ] {
        let refused = templates.list(filter, None, 50).await.unwrap_err();
        assert_eq!(
            (refused.status(), refused.code(), refused.field()),
            (422, "invalid_request", Some(field))
        );
    }
    assert_eq!(script.requests().len(), 2, "a refused filter was sent");

    // 2. The creation: checked first, then posted; the cache dropped.
    let definition = json!({
        "name": "spring_sale", "language": "en_US", "category": "MARKETING",
        "components": [
            {"type": "HEADER", "format": "TEXT", "text": "Our {{1}} is on!",
             "example": {"header_text": ["Spring Sale"]}},
            {"type": "BODY",
             "text": "Shop now through {{1}} and use code {{2}} to get {{3}} off of all merchandise.",
             "example": {"body_text": [["the end of April", "25OFF", "25%"]]}},
            {"type": "FOOTER", "text": "Use the buttons below to manage your marketing subscriptions"},
            {"type": "BUTTONS", "buttons": [
                {"type": "QUICK_REPLY", "text": "Unsubscribe from Promos"},
                {"type": "QUICK_REPLY", "text": "Unsubscribe from All"}
            ]}
        ]
    });
    let mut bad_name = definition.clone();
    bad_name["name"] = json!("Spring Sale");
    let refused = templates::definition(&bad_name).unwrap_err();
    assert_eq!(
        (refused.status(), refused.code(), refused.field()),
        (422, "invalid_request", Some("name"))
    );
    // custom-marketing-templates, example response.
    script.push_json(
        200,
        json!({"id": "1627019861106475", "status": "PENDING", "category": "MARKETING"}),
    );
    let created = templates
        .create(&templates::definition(&definition).unwrap())
        .await
        .unwrap();
    assert_eq!(
        created,
        TemplateCreated {
            id: TemplateId::new("1627019861106475").unwrap(),
            status: Some("PENDING".to_owned()),
            category: Some("MARKETING".to_owned()),
        }
    );
    let request = script.last_request().unwrap();
    assert_eq!(
        (request.method.as_str(), request.path()),
        ("POST", list_path.as_str())
    );
    assert_eq!(request.bearer(), Some(MANAGER.token));
    assert_eq!(request.json().unwrap(), definition, "posted as given");
    script.push_json(200, documented_list());
    templates
        .list(TemplateFilter::default(), None, 50)
        .await
        .unwrap();
    assert_eq!(script.requests().len(), 4, "the creation kept the cache");

    // 3. The deletions: by name; by name and id, looked up first.
    script.push_json(200, json!({"success": true}));
    templates.delete("order_confirmation", None).await.unwrap();
    let request = script.last_request().unwrap();
    assert_eq!(
        (request.method.as_str(), request.path()),
        ("DELETE", list_path.as_str())
    );
    assert_eq!(query(&request), pairs(&[("name", "order_confirmation")]));
    assert_eq!(request.bearer(), Some(MANAGER.token));
    script.push_json(
        200,
        json!({"data": [{"id": "1407680676729941", "name": "order_confirmation"}]}),
    );
    script.push_json(200, json!({"success": true}));
    templates
        .delete("order_confirmation", Some("1407680676729941"))
        .await
        .unwrap();
    let requests = script.requests();
    let [.., looked_up, deleted] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(
        (looked_up.method.as_str(), looked_up.path()),
        ("GET", list_path.as_str())
    );
    assert_eq!(
        query(looked_up),
        pairs(&[
            ("fields", "name"),
            ("limit", "100"),
            ("name", "order_confirmation")
        ])
    );
    assert_eq!(
        (deleted.method.as_str(), deleted.path()),
        ("DELETE", list_path.as_str())
    );
    assert_eq!(
        query(deleted),
        pairs(&[
            ("hsm_id", "1407680676729941"),
            ("name", "order_confirmation")
        ])
    );
    assert_eq!(deleted.bearer(), Some(MANAGER.token));
    let before = script.requests().len();
    for (name, id, field) in [
        ("", None, "name"),
        ("Bad Name", None, "name"),
        ("a", Some("x1"), "id"),
        ("a", Some("0123"), "id"),
    ] {
        let refused = templates.delete(name, id).await.unwrap_err();
        assert_eq!(
            (refused.status(), refused.code(), refused.field()),
            (422, "invalid_request", Some(field)),
            "{name:?} {id:?}"
        );
    }
    assert_eq!(
        script.requests().len(),
        before,
        "a refused deletion was sent"
    );
    script.push_json(200, json!({"data": []}));
    templates
        .list(TemplateFilter::default(), None, 50)
        .await
        .unwrap();
    assert_eq!(
        script.requests().len(),
        before + 1,
        "the deletion kept the cache"
    );
    assert_eq!(script.remaining(), 0);
    assert!(service.told.graph_errors().is_empty());
}
