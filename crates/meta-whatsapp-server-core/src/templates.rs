//! Templates (docs/design/server.md, section 4.2), as services an API
//! adapter calls, with the tenant's WABA token:
//!
//! | Operation | Graph calls |
//! | --- | --- |
//! | [`Templates::list`] | `GET /{waba_id}/message_templates?fields=name,language,status,category,components&limit=…[&status=…][&name=…][&after=…]`, cached per WABA ([`TemplateCache`]) |
//! | [`Templates::get`] | `GET /{id}?fields=name`, then `GET /{waba_id}/message_templates?fields=…&name=…` (the id must be among them) |
//! | [`Templates::create`] | `POST /{waba_id}/message_templates` |
//! | [`Templates::create_authentication`] | `POST /{waba_id}/upsert_message_templates` (`templates/authentication-templates/bulk-management`: one authentication template in one or more languages, created or updated; roadmap M3c) |
//! | [`Templates::delete`] | `DELETE /{waba_id}/message_templates?name=…`; with an id, `GET /{waba_id}/message_templates?fields=name&name=…` first (the id must be among them), then `…&hsm_id=…` |
//!
//! Pages: `templates/template-management`, `templates/overview`,
//! `reference/whatsapp-business-account/message-template-api`.
//!
//! **A template id is the WABA's, or it does not exist.** The WABA is the
//! tenant's (step 4 of the authorization order, [`crate::authz`]), and the
//! services open only on a WABA of the [`Caller`]'s own tenant
//! ([`Templates::new`]); a template id is Meta's, and one token may reach
//! several tenants' WABAs (the platform's system user token attached to
//! WABAs of different tenants). Meta's template object does not name its
//! WABA (`reference/whatsapp-business-account/message-template-api`), so
//! the id is looked for through the WABA's own edge, by the template's
//! name: a template of another WABA, or none, is `404 not_found`, the same
//! answer, without Meta's code or text, and nothing of it is answered or
//! deleted. The bare id is only asked for its name, never acted on. The
//! lookup reads at most 5 pages of 100 templates (a template of the WABA
//! past them is `404` too), through the list's `name` filter, which
//! Meta's reference for the list does not document; it costs 2 to 6
//! management calls (a get: the name, then the pages; a deletion by id:
//! the pages, then the deletion), never cached.
//!
//! Meta allows 200 management calls an hour per WABA: a list is cached
//! for a TTL the adapter sets (the service: 60 seconds) **per WABA** (and
//! per query and tenant), in the [`TemplateCache`] the adapter holds (the
//! service: one per replica), and a creation or deletion through it drops
//! the WABA's entries. A creation is Meta's own JSON (a
//! `TemplateDefinition`, read by [`definition`]), checked locally against
//! every limit the docs state before any request (`422 invalid_request`
//! with `field`); Meta refusing its content is `422 template_rejected`, a
//! WABA at its template limit `409 template_limit_reached`. An idempotent
//! creation runs under [`crate::idempotency::run`]. Review results arrive
//! as `template_status_updated` events. A deletion is a tenant's audit
//! event ([`crate::audit::tenant_audit`]: `template_deleted`, or
//! `templates_deleted` for every language of a name).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::authentication::{AuthenticationUpsert, OtpButton, SupportedApp};
use meta_whatsapp_rs::client::templates::{
    TemplateCreated as Submitted, TemplateDefinition, TemplateInfo, TemplateListQuery,
    TemplateStatus,
};
use meta_whatsapp_rs::core::ids::{TemplateId, WabaId};
use serde_json::Value;
use tokio::time::{Duration, Instant};

use crate::audit::{Subject, tenant_audit};
use crate::authz::{Authorizer, Caller, OwnedWaba, object_failed};
use crate::error::ServiceError;
use crate::observe::Observer;
use crate::request::{meta_object, typed_graph_id, wire};

/// The fields asked of Meta for each template.
pub const TEMPLATE_FIELDS: [&str; 5] = ["name", "language", "status", "category", "components"];

/// Most list pages cached in one [`TemplateCache`]; past it, the least
/// recently used page goes.
pub const TEMPLATE_CACHE_ENTRIES: usize = 512;

/// Most list pages cached for one tenant's WABA (its queries and cursors);
/// past it, that WABA's least recently used page goes, never another
/// WABA's.
pub const TEMPLATE_CACHE_PER_WABA: usize = 32;

/// Longest template name (`templates/template-management`: 512
/// characters of `a-z 0-9 _`).
const MAX_NAME_LEN: usize = 512;

/// Page size of the lookups that find a template id among a WABA's
/// templates of one name.
const LOOKUP_PAGE_SIZE: u32 = 100;

/// Most pages such a lookup reads: a name has one template per language
/// (Meta supports about 70), so one page is the rule; past these, the id
/// counts as not the WABA's.
const LOOKUP_PAGES: usize = 5;

/// Where the lookup's log line has always been written from: the target
/// the service's operators filter on (`RUST_LOG`), kept when the lookup
/// moved here (roadmap S5c).
const LOG_TARGET: &str = "meta_whatsapp_server::api::templates";

// ─── The answers ─────────────────────────────────────────────────────────

/// A template, as Meta describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    /// Template id.
    pub id: TemplateId,
    /// Name (several languages share one).
    pub name: Option<String>,
    /// Language code, e.g. `en_US`.
    pub language: Option<String>,
    /// Review status, as Meta names it (`APPROVED`, `PENDING`, `REJECTED`,
    /// `PAUSED`, …): only `APPROVED` templates can be sent.
    pub status: Option<String>,
    /// Category, as Meta names it (`MARKETING`, `UTILITY`,
    /// `AUTHENTICATION`).
    pub category: Option<String>,
    /// Components, in Meta's JSON (`templates/components`).
    pub components: Vec<Value>,
}

impl From<TemplateInfo> for Template {
    fn from(info: TemplateInfo) -> Self {
        Self {
            status: wire(info.status.as_ref()),
            category: wire(info.category.as_ref()),
            components: info
                .components
                .iter()
                .filter_map(|c| serde_json::to_value(c).ok())
                .collect(),
            id: info.id,
            name: info.name,
            language: info.language,
        }
    }
}

/// A page of a WABA's templates.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplatePage {
    /// The templates, in Meta's order.
    pub templates: Vec<Template>,
    /// Meta's cursor for the next page (its `paging.cursors.after`), when
    /// Meta says one follows: pass it back as the list's `after`. The
    /// adapter may wrap it (the service hex-encodes it as `next_cursor`).
    pub next_after: Option<String>,
}

/// A created template, as Meta answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateCreated {
    /// Template id.
    pub id: TemplateId,
    /// Initial status (usually `PENDING`).
    pub status: Option<String>,
    /// The category Meta assigned.
    pub category: Option<String>,
}

impl From<Submitted> for TemplateCreated {
    fn from(created: Submitted) -> Self {
        Self {
            status: wire(created.status.as_ref()),
            category: wire(created.category.as_ref()),
            id: created.id,
        }
    }
}

/// An authentication template's button (roadmap M3c): Meta's `otp_type`
/// values the service creates. Zero-tap is not among them: creating one
/// sets Meta's `zero_tap_terms_accepted`, an acceptance of the WhatsApp
/// Business terms for zero-tap, a legal act the service does not take for
/// a tenant (AGENTS.md, § Decisions: the owner's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtpButtonType {
    /// `copy_code`: the user copies the code.
    CopyCode,
    /// `one_tap`: Android apps (`supported_apps`) fill the code in.
    OneTap,
}

/// An authentication template to create or update, as the request carried
/// it (roadmap M3c): Meta's bulk management fields
/// (`templates/authentication-templates/bulk-management`), its components'
/// own fields flattened, each named as Meta names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationRequest {
    /// The template's name.
    pub name: String,
    /// Its languages: each is created, or updated when it exists.
    pub languages: Vec<String>,
    /// The button.
    pub otp_type: OtpButtonType,
    /// A one-tap button's Android apps, `(package_name, signature_hash)`,
    /// 1 to 5; none for copy code.
    pub supported_apps: Vec<(String, String)>,
    /// The body's "For your security, do not share this code."
    pub add_security_recommendation: Option<bool>,
    /// The footer's "This code expires in N minutes.", 1 to 90.
    pub code_expiration_minutes: Option<u32>,
    /// The message's time to live: 30 to 900 seconds, or -1.
    pub message_send_ttl_seconds: Option<i64>,
}

/// One language of an authentication template created or updated, as Meta
/// answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationTemplateCreated {
    /// Template id.
    pub id: TemplateId,
    /// Its status, as Meta wrote it.
    pub status: Option<String>,
    /// Its language.
    pub language: Option<String>,
}

/// The library's upsert of `request`, checked against every limit the
/// docs state before any request.
///
/// # Errors
///
/// `422 invalid_request` on `name`, `languages`, `supported_apps`,
/// `code_expiration_minutes` or `message_send_ttl_seconds` (the field the
/// library's check names, as this request names it; `body` otherwise).
pub fn authentication(
    request: AuthenticationRequest,
) -> Result<AuthenticationUpsert, ServiceError> {
    template_name("name", &request.name)?;
    if request.languages.is_empty() {
        return Err(ServiceError::invalid("languages"));
    }
    let otp = match request.otp_type {
        OtpButtonType::CopyCode if request.supported_apps.is_empty() => {
            OtpButton::CopyCode { text: None }
        }
        OtpButtonType::CopyCode => return Err(ServiceError::invalid("supported_apps")),
        OtpButtonType::OneTap => OtpButton::OneTap {
            text: None,
            autofill_text: None,
            supported_apps: request
                .supported_apps
                .into_iter()
                .map(|(package, hash)| SupportedApp::new(package, hash))
                .collect(),
        },
    };
    let upsert = AuthenticationUpsert {
        name: request.name,
        languages: request.languages,
        otp,
        add_security_recommendation: request.add_security_recommendation,
        code_expiration_minutes: request.code_expiration_minutes,
        message_send_ttl_seconds: request.message_send_ttl_seconds,
    };
    upsert.validate().map_err(|invalid| {
        let field = invalid.field.as_str();
        ServiceError::invalid(
            [
                "supported_apps",
                "code_expiration_minutes",
                "message_send_ttl_seconds",
                "languages",
            ]
            .into_iter()
            .find(|name| field.contains(name))
            .unwrap_or(if field == "name" { "name" } else { "body" }),
        )
    })?;
    Ok(upsert)
}

/// A list's filters, as the request carried them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateFilter {
    /// Only templates with this status, as Meta names it (`APPROVED`,
    /// `REJECTED`, …; any case).
    pub status: Option<String>,
    /// Only templates with this name.
    pub name: Option<String>,
}

// ─── The cache ───────────────────────────────────────────────────────────

/// What a cached page answers: one tenant's WABA, one query. The tenant
/// is in the key although a WABA has one tenant at a time: a page cached
/// before an unbind is never answered to the next tenant bound to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    tenant: String,
    waba_id: String,
    status: Option<String>,
    name: Option<String>,
    after: Option<String>,
    limit: usize,
}

#[derive(Debug)]
struct Cached {
    /// When Meta answered it.
    at: Instant,
    /// When it was last answered from here.
    used: Instant,
    page: Arc<TemplatePage>,
}

/// Template lists cached per WABA, in memory (the service keeps one per
/// replica): at most [`TEMPLATE_CACHE_PER_WABA`] pages for one tenant's
/// WABA and [`TEMPLATE_CACHE_ENTRIES`] in all, each for the TTL, on
/// tokio's clock.
#[derive(Debug)]
pub struct TemplateCache {
    ttl: Duration,
    entries: Mutex<HashMap<CacheKey, Cached>>,
}

impl TemplateCache {
    /// An empty cache keeping pages for `ttl`.
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, key: &CacheKey) -> Option<Arc<TemplatePage>> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let cached = entries.get_mut(key)?;
        if cached.at.elapsed() >= self.ttl {
            return None;
        }
        cached.used = Instant::now();
        Some(cached.page.clone())
    }

    /// Keep `page`: expired pages go first; then, past
    /// [`TEMPLATE_CACHE_PER_WABA`], the WABA's least recently used page,
    /// and past [`TEMPLATE_CACHE_ENTRIES`], the cache's.
    fn put(&self, key: CacheKey, page: Arc<TemplatePage>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let ttl = self.ttl;
        entries.retain(|_, cached| cached.at.elapsed() < ttl);
        entries.remove(&key);
        let same_waba = |k: &CacheKey| k.tenant == key.tenant && k.waba_id == key.waba_id;
        if entries.keys().filter(|k| same_waba(k)).count() >= TEMPLATE_CACHE_PER_WABA {
            let lru = entries
                .iter()
                .filter(|(k, _)| same_waba(k))
                .min_by_key(|(_, cached)| cached.used)
                .map(|(k, _)| k.clone());
            if let Some(lru) = lru {
                entries.remove(&lru);
            }
        }
        if entries.len() >= TEMPLATE_CACHE_ENTRIES {
            let lru = entries
                .iter()
                .min_by_key(|(_, cached)| cached.used)
                .map(|(k, _)| k.clone());
            if let Some(lru) = lru {
                entries.remove(&lru);
            }
        }
        let now = Instant::now();
        entries.insert(
            key,
            Cached {
                at: now,
                used: now,
                page,
            },
        );
    }

    /// Drop every page of `waba_id`: its templates changed.
    pub fn invalidate(&self, waba_id: &WabaId) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|key, _| key.waba_id != waba_id.as_str());
    }
}

// ─── The request's checks ────────────────────────────────────────────────

/// A template name: `a-z 0-9 _`, at most 512 characters, else `422
/// invalid_request` on `field`.
pub(crate) fn template_name(field: &'static str, name: &str) -> Result<(), ServiceError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if valid {
        Ok(())
    } else {
        Err(ServiceError::invalid(field))
    }
}

/// A status filter: letters and `_`, upper-cased as Meta writes it, else
/// `422 invalid_request` on `status`.
fn status_filter(status: &str) -> Result<String, ServiceError> {
    let valid = !status.is_empty()
        && status.len() <= 64
        && status.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_');
    if valid {
        Ok(status.to_ascii_uppercase())
    } else {
        Err(ServiceError::invalid("status"))
    }
}

/// The key of a zero-tap authentication button by which the template's
/// creator accepts Meta's zero-tap terms
/// (`templates/authentication-templates/zero-tap-authentication-templates`:
/// "If set to false, the template will not be created"). Setting it is a
/// legal act, which the service never takes for a tenant (AGENTS.md,
/// § Decisions, "Legal acts"; design section 10): a definition holding it
/// `true` is refused on its path, before anything is sent. Zero-tap stays
/// the owner's (roadmap M3g); `false` is the library's own refusal.
pub const ZERO_TAP_TERMS: &str = "zero_tap_terms_accepted";

/// The path of the first [`ZERO_TAP_TERMS`] set `true` in `value`, at
/// `path`, wherever it is (a button of the `BUTTONS` component, a
/// carousel card's, any other). Below a key that is not a plain name the
/// path stops growing (the deepest plain one, `body` at the root), so the
/// answer never echoes arbitrary input ([`meta_object`]'s rule).
fn zero_tap_terms(value: &Value, path: &str) -> Option<String> {
    walk_zero_tap_terms(value, path, false)
}

fn walk_zero_tap_terms(value: &Value, path: &str, opaque: bool) -> Option<String> {
    let root = || {
        if path.is_empty() {
            "body".to_owned()
        } else {
            path.to_owned()
        }
    };
    let below = |key: &str| {
        let plain = !key.is_empty()
            && key.len() <= 64
            && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        match (opaque || !plain, path.is_empty()) {
            (true, _) => (root(), true),
            (false, true) => (key.to_owned(), false),
            (false, false) => (format!("{path}.{key}"), false),
        }
    };
    match value {
        Value::Object(object) => object.iter().find_map(|(key, value)| {
            let (at, opaque) = below(key);
            if key == ZERO_TAP_TERMS && value.as_bool() == Some(true) {
                Some(at)
            } else {
                walk_zero_tap_terms(value, &at, opaque)
            }
        }),
        Value::Array(items) => items.iter().enumerate().find_map(|(i, item)| {
            let at = if opaque {
                root()
            } else {
                format!("{}[{i}]", root())
            };
            walk_zero_tap_terms(item, &at, opaque)
        }),
        _ => None,
    }
}

/// The template definition a request carries, in Meta's JSON (`name`,
/// `language`, `category`, `parameter_format`, `components`, as
/// `templates/overview` writes it), read with no key dropped
/// ([`meta_object`]) and checked against every limit the docs state:
/// nothing is sent when this fails.
///
/// # Errors
///
/// `422 invalid_request`: on `body` (not a definition), on the path of a
/// key the library would not send to Meta, on the path of a
/// `zero_tap_terms_accepted: true` ([`ZERO_TAP_TERMS`]), or on the field
/// breaking a documented limit (`name`,
/// `components[2].buttons[0].package_name`, …).
pub fn definition(body: &Value) -> Result<TemplateDefinition, ServiceError> {
    let definition: TemplateDefinition = meta_object("", body)?;
    if let Some(path) = zero_tap_terms(body, "") {
        tracing::warn!(
            field = path.as_str(),
            "refused a template accepting Meta's zero-tap terms: a legal act the service does not take for a tenant"
        );
        return Err(ServiceError::invalid(path));
    }
    definition
        .validate()
        .map_err(|invalid| ServiceError::invalid(invalid.field))?;
    Ok(definition)
}

// ─── The operations ──────────────────────────────────────────────────────

/// The template operations on a WABA of the caller's tenant: an
/// [`OwnedWaba`] its [`Authorizer`] made from that tenant's binding, for
/// a [`Caller`] it made, both admitted ([`Self::new`]), with the WABA's
/// token; the lists cached in a [`TemplateCache`]. A failed Graph call is
/// told to the [`Observer`] (the adapter's metrics), and a `190` marks the
/// WABA's numbers `reconnect_required` ([`OwnedWaba::failed`]). Cheap: a
/// few references.
pub struct Templates<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    waba: &'a OwnedWaba,
    cache: &'a TemplateCache,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Templates<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Templates")
            .field("caller", self.caller)
            .field("waba", self.waba)
            .finish_non_exhaustive()
    }
}

impl<'a> Templates<'a> {
    /// The operations on `waba` for `caller`, once `authz` admits both
    /// ([`Authorizer::admit`], [`Authorizer::admit_waba`]) and `waba` was
    /// made from a binding of `caller`'s tenant: what `caller` names,
    /// lists, creates or deletes is that WABA's. Lists are cached in
    /// `cache`; failed Graph calls are reported to nobody until
    /// [`Self::observed_by`].
    ///
    /// # Errors
    ///
    /// `caller` or `waba` another [`Authorizer`] made: `403 forbidden`,
    /// logged at `warn`. `waba` another tenant's: `404 not_found`, the
    /// answer for a WABA that does not exist, logged at `warn`. Either
    /// way, before anything is read or asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        waba: &'a OwnedWaba,
        cache: &'a TemplateCache,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_waba(waba)?;
        if waba.tenant() != caller.tenant() {
            tracing::warn!("refused the template services on another tenant's WABA");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
            waba,
            cache,
            observer: &(),
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// The error of a failed Graph call, counted when Meta answered with
    /// a Graph error, without Meta's `details` yet.
    async fn counted(&self, error: &Error) -> ServiceError {
        let api = self.waba.failed(self.authz, error).await;
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        api
    }

    /// [`Self::counted`], with Meta's `details`.
    async fn failed(&self, error: &Error) -> ServiceError {
        self.counted(error).await.with_details(error)
    }

    /// One page of the WABA's templates (`limit` of them, after Meta's
    /// cursor `after`, a [`TemplatePage::next_after`]), from the cache or
    /// from Meta, which then keeps it.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `status` or `name`, before the cache or
    /// Meta is asked; Meta's refusal or failure, with its `details`.
    pub async fn list(
        &self,
        filter: TemplateFilter,
        after: Option<String>,
        limit: usize,
    ) -> Result<Arc<TemplatePage>, ServiceError> {
        let TemplateFilter { status, name } = filter;
        let status = status.as_deref().map(status_filter).transpose()?;
        if let Some(name) = &name {
            template_name("name", name)?;
        }
        let key = CacheKey {
            tenant: self.caller.tenant().as_str().to_owned(),
            waba_id: self.waba.waba_id().as_str().to_owned(),
            status,
            name,
            after,
            limit,
        };
        if let Some(page) = self.cache.get(&key) {
            return Ok(page);
        }
        let mut query = TemplateListQuery::new()
            .fields(TEMPLATE_FIELDS)
            .limit(u32::try_from(limit).unwrap_or(u32::MAX));
        query.status = key.status.as_deref().map(|s| {
            s.parse::<TemplateStatus>()
                .unwrap_or_else(|never| match never {})
        });
        query.name.clone_from(&key.name);
        query.after.clone_from(&key.after);
        let page = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .list(&query)
            .await;
        let page = match page {
            Ok(page) => page,
            Err(error) => return Err(self.failed(&error).await),
        };
        let next_after = page.next_cursor().map(str::to_owned);
        let listed = Arc::new(TemplatePage {
            templates: page.data.into_iter().map(Template::from).collect(),
            next_after,
        });
        self.cache.put(key, listed.clone());
        Ok(listed)
    }

    /// The template `id`, if it is the WABA's: its name first (`GET
    /// /{id}?fields=name`, nothing else of it read before it is known to
    /// be the WABA's), then the WABA's own templates of that name, among
    /// which it must be (see the [module](self)). Never cached.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `id` (not a Meta id), before any request.
    /// `404 not_found`, without Meta's code or text: Meta refusing the id
    /// (none, or not for this token), or a template not among the WABA's.
    /// Meta's refusal or failure otherwise, with its `details`.
    pub async fn get(&self, id: &str) -> Result<Template, ServiceError> {
        let id: TemplateId = typed_graph_id("id", id)?;
        // Its name: what the WABA's edge finds templates by. Nothing else of
        // it is read before it is known to be the WABA's.
        let named = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .get_fields(&id, &["name"])
            .await;
        let name = match named {
            Ok(info) => info.name.ok_or_else(ServiceError::not_found)?,
            Err(error) => return Err(object_failed(self.counted(&error).await, &error)),
        };
        let template = self
            .in_waba(&name, &id, &TEMPLATE_FIELDS)
            .await?
            .ok_or_else(ServiceError::not_found)?;
        Ok(Template::from(template))
    }

    /// The template `id` among the WABA's templates named `name`, with
    /// `fields`, looked for through the WABA's own edge (`GET
    /// /{waba_id}/message_templates?name=…`), or `None` when it is not the
    /// WABA's: Meta's template object does not name its WABA, and a token
    /// may reach other tenants' WABAs.
    async fn in_waba(
        &self,
        name: &str,
        id: &TemplateId,
        fields: &[&str],
    ) -> Result<Option<TemplateInfo>, ServiceError> {
        let templates = self.waba.client().templates(self.waba.waba_id().clone());
        let mut query = TemplateListQuery::new()
            .fields(fields.iter().copied())
            .name(name)
            .limit(LOOKUP_PAGE_SIZE);
        for _ in 0..LOOKUP_PAGES {
            let page = match templates.list(&query).await {
                Ok(page) => page,
                Err(error) => return Err(self.failed(&error).await),
            };
            let next = page.next_cursor().map(str::to_owned);
            if let Some(found) = page.data.into_iter().find(|t| t.id == *id) {
                return Ok(Some(found));
            }
            match next {
                Some(after) => query.after = Some(after),
                None => return Ok(None),
            }
        }
        tracing::warn!(
            target: LOG_TARGET,
            "a template id was not found in the first pages of its name's templates"
        );
        Ok(None)
    }

    /// Create `definition` (read by [`definition`]) in the WABA: Meta's
    /// answer. The WABA's cached lists go either way (a refusal can follow
    /// a creation Meta did make).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details` (`422
    /// template_rejected`, `409 template_limit_reached`; a timeout
    /// `may_have_been_sent`).
    pub async fn create(
        &self,
        definition: &TemplateDefinition,
    ) -> Result<TemplateCreated, ServiceError> {
        let created = self
            .waba
            .client()
            .templates(self.waba.waba_id().clone())
            .create(definition)
            .await;
        self.cache.invalidate(self.waba.waba_id());
        match created {
            Ok(created) => Ok(TemplateCreated::from(created)),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Create or update the authentication template `upsert` in each of
    /// its languages (`POST /{waba_id}/upsert_message_templates`, the
    /// library's `Authentication::upsert`; roadmap M3c). The WABA's cached
    /// lists go, whatever Meta answers. Not replayed on a timeout: updating
    /// an approved template counts against its edit limit.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details` (`422
    /// template_rejected`, `409 template_limit_reached`; a timeout
    /// `may_have_been_sent`).
    pub async fn create_authentication(
        &self,
        upsert: &AuthenticationUpsert,
    ) -> Result<Vec<AuthenticationTemplateCreated>, ServiceError> {
        let upserted = self
            .waba
            .client()
            .authentication(self.waba.waba_id().clone())
            .upsert(upsert)
            .await;
        self.cache.invalidate(self.waba.waba_id());
        match upserted {
            Ok(upserted) => Ok(upserted
                .into_iter()
                .map(|template| AuthenticationTemplateCreated {
                    status: wire(template.status.as_ref()),
                    id: template.id,
                    language: template.language,
                })
                .collect()),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Delete every language of the template `name`, or, with `id`, the
    /// one template of that name and id, if it is the WABA's (looked for
    /// first among the WABA's templates of that name: Meta's page does
    /// not say it checks an `hsm_id` against the WABA). The WABA's cached
    /// lists go, whatever Meta answers; a deletion is the caller's audit
    /// event (`template_deleted` with an id, else `templates_deleted`).
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `name`, then on `id`, before any request.
    /// `404 not_found` for an id not among the WABA's templates of that
    /// name, and nothing deleted. Meta's refusal or failure, with its
    /// `details` (no template of that name is Meta's `invalid_parameter`).
    pub async fn delete(&self, name: &str, id: Option<&str>) -> Result<(), ServiceError> {
        template_name("name", name)?;
        let id: Option<TemplateId> = id.map(|id| typed_graph_id("id", id)).transpose()?;
        let templates = self.waba.client().templates(self.waba.waba_id().clone());
        let deleted = match &id {
            Some(id) => {
                // One of the WABA's own, or nothing is deleted: Meta's page
                // does not say it checks an `hsm_id` against the WABA.
                if self.in_waba(name, id, &["name"]).await?.is_none() {
                    return Err(ServiceError::not_found());
                }
                templates.delete_by_id(name, id).await
            }
            None => templates.delete_by_name(name).await,
        };
        self.cache.invalidate(self.waba.waba_id());
        match deleted {
            Ok(()) => {
                tenant_audit(
                    if id.is_some() {
                        "template_deleted"
                    } else {
                        "templates_deleted"
                    },
                    self.caller.key_id(),
                    &Subject {
                        tenant: Some(self.caller.tenant().as_str()),
                        waba_id: Some(self.waba.waba_id().as_str()),
                        ..Subject::default()
                    },
                );
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(id: &str) -> Arc<TemplatePage> {
        Arc::new(TemplatePage {
            templates: vec![Template {
                id: TemplateId::new(id).unwrap(),
                name: None,
                language: None,
                status: None,
                category: None,
                components: Vec::new(),
            }],
            next_after: None,
        })
    }

    fn first(page: &TemplatePage) -> &str {
        page.templates[0].id.as_str()
    }

    fn key(waba: &str) -> CacheKey {
        CacheKey {
            tenant: "merchant-42".to_owned(),
            waba_id: waba.to_owned(),
            status: None,
            name: None,
            after: None,
            limit: 50,
        }
    }

    /// Pages are kept per WABA and query for the TTL, and a WABA's
    /// invalidation drops its own only. Decisive: the WABA in the key.
    #[tokio::test(start_paused = true)]
    async fn pages_are_cached_per_waba_for_the_ttl() {
        let cache = TemplateCache::new(Duration::from_secs(60));
        cache.put(key("1"), page("11"));
        assert!(cache.get(&key("2")).is_none(), "another WABA's page");
        assert_eq!(first(&cache.get(&key("1")).unwrap()), "11");
        let other_query = CacheKey {
            status: Some("APPROVED".to_owned()),
            ..key("1")
        };
        assert!(cache.get(&other_query).is_none());
        let other_tenant = CacheKey {
            tenant: "merchant-43".to_owned(),
            ..key("1")
        };
        assert!(cache.get(&other_tenant).is_none(), "another tenant's page");
        cache.put(key("2"), page("22"));
        cache.invalidate(&WabaId::new("1").unwrap());
        assert!(cache.get(&key("1")).is_none());
        assert_eq!(first(&cache.get(&key("2")).unwrap()), "22");
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(cache.get(&key("2")).is_none(), "expired");
    }

    /// Bounded per WABA and in all, the least recently used page going
    /// first: a new page is always kept, and one WABA's many queries never
    /// push another WABA's pages out. Decisive: the per-WABA cap and the
    /// recency.
    #[tokio::test(start_paused = true)]
    async fn the_cache_is_bounded_per_waba_and_least_recently_used_first() {
        let cache = TemplateCache::new(Duration::from_secs(60));
        let query = |waba: &str, after: usize| CacheKey {
            after: Some(after.to_string()),
            ..key(waba)
        };
        cache.put(key("other"), page("1"));
        for i in 0..TEMPLATE_CACHE_PER_WABA + 10 {
            tokio::time::advance(Duration::from_millis(1)).await;
            cache.put(query("busy", i), page("2"));
            // The other WABA's page stays in use.
            assert!(cache.get(&key("other")).is_some(), "{i}");
        }
        let busy = cache
            .entries
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.waba_id == "busy")
            .count();
        assert_eq!(busy, TEMPLATE_CACHE_PER_WABA);
        assert!(
            cache.get(&query("busy", 0)).is_none(),
            "its oldest page went"
        );
        assert!(
            cache
                .get(&query("busy", TEMPLATE_CACHE_PER_WABA + 9))
                .is_some()
        );
        // In all: the least recently used page goes, a new page is always
        // kept.
        for i in 0..TEMPLATE_CACHE_ENTRIES + 10 {
            tokio::time::advance(Duration::from_millis(1)).await;
            cache.put(key(&i.to_string()), page("3"));
            assert!(cache.get(&key(&i.to_string())).is_some(), "{i} was kept");
        }
        assert_eq!(cache.entries.lock().unwrap().len(), TEMPLATE_CACHE_ENTRIES);
        assert!(
            cache.get(&key("0")).is_none(),
            "the least recently used went"
        );
    }

    /// A page answered from the cache counts as used: the pages not asked
    /// for since go before it, in a WABA and in all (least recently used,
    /// not first in first out). Decisive: a hit refreshing the page's
    /// recency.
    #[tokio::test(start_paused = true)]
    async fn a_cache_hit_keeps_its_page_longest() {
        let cache = TemplateCache::new(Duration::from_secs(60));
        let tick = || tokio::time::advance(Duration::from_millis(1));
        let query = |after: usize| CacheKey {
            after: Some(after.to_string()),
            ..key("busy")
        };
        for i in 0..TEMPLATE_CACHE_PER_WABA {
            tick().await;
            cache.put(query(i), page("1"));
        }
        tick().await;
        assert!(
            cache.get(&query(0)).is_some(),
            "the oldest page, used again"
        );
        tick().await;
        cache.put(query(TEMPLATE_CACHE_PER_WABA), page("1"));
        assert!(cache.get(&query(0)).is_some(), "the page used again stayed");
        assert!(
            cache.get(&query(1)).is_none(),
            "the least recently used went"
        );
        // In all: fill it with other WABAs' pages, use the oldest again,
        // add one more.
        let other = |i: usize| key(&format!("w{i}"));
        let room = TEMPLATE_CACHE_ENTRIES - cache.entries.lock().unwrap().len();
        for i in 0..room {
            tick().await;
            cache.put(other(i), page("1"));
        }
        assert_eq!(cache.entries.lock().unwrap().len(), TEMPLATE_CACHE_ENTRIES);
        tick().await;
        // `busy`'s pages were put before `other(0)`; use them all again,
        // then `other(0)`, the oldest left.
        for i in 2..=TEMPLATE_CACHE_PER_WABA {
            assert!(cache.get(&query(i)).is_some(), "{i}");
        }
        assert!(cache.get(&query(0)).is_some());
        assert!(cache.get(&other(0)).is_some());
        tick().await;
        cache.put(key("new"), page("1"));
        assert!(cache.get(&other(0)).is_some(), "used again: kept");
        assert!(
            cache.get(&other(1)).is_none(),
            "the least recently used went"
        );
    }

    #[test]
    fn filters_are_checked() {
        assert_eq!(status_filter("approved").unwrap(), "APPROVED");
        assert!(status_filter("APPROVED&x=1").is_err());
        assert!(status_filter(&"A".repeat(65)).is_err());
        assert!(template_name("name", "order_confirmation").is_ok());
        for bad in ["", "Order", "order confirmation", "a&b"] {
            assert!(template_name("name", bad).is_err(), "{bad:?}");
        }
        assert!(template_name("name", &"a".repeat(512)).is_ok());
        assert!(template_name("name", &"a".repeat(513)).is_err());
    }

    /// A definition is read with no key dropped, then checked against the
    /// documented limits, each refusal on its field. Decisive: the
    /// library's `validate`, and the round trip's check.
    #[test]
    fn a_definition_is_read_then_checked() {
        let good = serde_json::json!({"name": "spring_sale", "language": "en_US",
            "category": "MARKETING",
            "components": [{"type": "BODY", "text": "Shop now through the end of April."}]});
        assert_eq!(definition(&good).unwrap().name, "spring_sale");
        let mut bad_name = good.clone();
        bad_name["name"] = serde_json::json!("Spring Sale");
        assert_eq!(definition(&bad_name).unwrap_err().field(), Some("name"));
        let mut misspelled = good.clone();
        misspelled["sub_catgory"] = serde_json::json!("CUSTOM");
        assert_eq!(
            definition(&misspelled).unwrap_err().field(),
            Some("sub_catgory")
        );
        assert_eq!(
            definition(&serde_json::json!([])).unwrap_err().field(),
            Some("body")
        );
    }

    /// Meta's zero-tap example
    /// (`templates/authentication-templates/zero-tap-authentication-templates`,
    /// "Example request"), which the library accepts: refused on the
    /// terms' path, the legal act the service never takes for a tenant.
    /// With `false`, the library's own refusal, on the same path; the
    /// one-tap example, which carries no terms, passes. Decisive: the
    /// walk for `zero_tap_terms_accepted`.
    #[test]
    fn a_definition_accepting_the_zero_tap_terms_is_refused() {
        let zero_tap = |accepted: bool| {
            serde_json::json!({"name": "zero_tap_auth_template", "language": "en_US",
                "category": "authentication", "message_send_ttl_seconds": 60,
                "components": [
                    {"type": "body", "add_security_recommendation": true},
                    {"type": "footer", "code_expiration_minutes": 5},
                    {"type": "buttons", "buttons": [{"type": "otp", "otp_type": "zero_tap",
                        "text": "Copy Code", "autofill_text": "Autofill",
                        "zero_tap_terms_accepted": accepted,
                        "supported_apps": [{"package_name": "com.example.luckyshrub",
                                            "signature_hash": "K8a/AINcGX7"}]}]}]})
        };
        let accepting = zero_tap(true);
        let typed: TemplateDefinition = meta_object("", &accepting).unwrap();
        assert!(typed.validate().is_ok(), "the library would send it");
        assert_eq!(
            definition(&accepting).unwrap_err().field(),
            Some("components[2].buttons[0].zero_tap_terms_accepted")
        );
        assert_eq!(
            definition(&zero_tap(false)).unwrap_err().field(),
            Some("components[2].buttons[0].zero_tap_terms_accepted")
        );
        let mut one_tap = zero_tap(true);
        let button = &mut one_tap["components"][2]["buttons"][0];
        button["otp_type"] = "one_tap".into();
        button.as_object_mut().unwrap().remove(ZERO_TAP_TERMS);
        assert!(definition(&one_tap).is_ok());
        // Wherever it is, and a key that is not a plain name is never
        // echoed.
        assert_eq!(
            zero_tap_terms(&serde_json::json!({"a b": [{ZERO_TAP_TERMS: true}]}), ""),
            Some("body".to_owned())
        );
        assert_eq!(
            zero_tap_terms(
                &serde_json::json!({"cards": [{}, {ZERO_TAP_TERMS: true}]}),
                ""
            ),
            Some("cards[1].zero_tap_terms_accepted".to_owned())
        );
        assert_eq!(
            zero_tap_terms(&serde_json::json!({ZERO_TAP_TERMS: "true"}), ""),
            None,
            "only `true`: a string is no definition anyway"
        );
    }
}
