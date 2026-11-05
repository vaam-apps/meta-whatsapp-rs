//! WhatsApp Flows' management (roadmap M5d, parity row 61): a WABA's Flows
//! created, listed, read, changed, filled with their Flow JSON, previewed,
//! published, deprecated and deleted, as services an API adapter calls
//! with the tenant's WABA token. Each goes through the library's calls
//! (`client::flows::{Flows, Flow}`), after the checks the service adds.
//!
//! | Operation | Graph calls |
//! | --- | --- |
//! | [`Flows::create`] | with a `clone_flow_id`: `GET /{clone_flow_id}` first (it must be the WABA's); then `POST /{waba_id}/flows` |
//! | [`Flows::list`] | `GET /{waba_id}/flows[?after=…]` |
//! | [`Flows::get`] | `GET /{flow_id}?fields=…` (its WABA must be this one) |
//! | [`Flows::update`], [`Flows::upload_json`], [`Flows::publish`], [`Flows::deprecate`], [`Flows::delete`] | the same read, then `POST /{flow_id}`, `POST /{flow_id}/assets` (multipart), `POST /{flow_id}/publish`, `POST /{flow_id}/deprecate`, `DELETE /{flow_id}` |
//! | [`Flows::preview`] | the same read, then `GET /{flow_id}?fields=preview.invalidate(…)` |
//!
//! Pages: `flows/guides/flowsapi`, `flows/guides/lifecycle`,
//! `flows/guides/implementingyourflowendpoint`.
//!
//! **A Flow id is the WABA's, or it does not exist** (the design's M5.2).
//! A Flow id is Meta's, global, and one token may reach several tenants'
//! WABAs, so a Flow named by an id (in a path, or as the `clone_flow_id`
//! of a creation) is read first (`GET /{flow_id}`) and acted on only when
//! Meta answers that very id with this WABA as its
//! `whatsapp_business_account`: the check design D102 makes before a Flow
//! message is sent. Another WABA's Flow, an id Meta refuses, or another
//! kind of node is `404 not_found`, without Meta's code or text, and
//! nothing of it is answered, changed or deleted (`DELETE /{id}` deletes
//! whatever node an id names). The read is the only request made for it.
//!
//! A change that succeeded is the caller's tenant audit event
//! ([`crate::audit::tenant_audit`]: `flow_updated`, `flow_json_uploaded`,
//! `flow_published`, `flow_deprecated`, `flow_deleted`; a creation is not
//! audited, as a template's is not). A creation runs under
//! [`crate::idempotency::run`].
//!
//! **What is not here**, because the library does not carry it: the
//! `application_id` of a Flow's update (the Meta app connected to a Flow
//! with an endpoint), the `health_status` of a Flow's details, and the
//! Flow's assets. The Flow JSON itself is Meta's to validate: its
//! validation errors come back in the answer of a creation and of an
//! upload, never as an error.

use bytes::Bytes;
use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::flows::{
    CreateFlow, FlowCategory, FlowDetails, FlowValidationError, ListFlows, MAX_FLOW_JSON_BYTES,
    UpdateFlow,
};
use meta_whatsapp_rs::core::ids::{FlowId, WabaId};
use serde::Deserialize;
use serde_json::Value;

use crate::audit::{Subject, tenant_audit};
use crate::authz::{Authorizer, Caller, OwnedWaba, object_failed};
use crate::error::ServiceError;
use crate::observe::Observer;
use crate::request::{graph_id, wire};

/// Longest Flow name accepted. Meta's page states none; the bound keeps a
/// name a name (the Flow builder shows it in a list).
pub const MAX_NAME_CHARS: usize = 256;

/// Most categories a Flow carries: the pages list eight (`SIGN_UP`,
/// `SIGN_IN`, `APPOINTMENT_BOOKING`, `LEAD_GENERATION`, `CONTACT_US`,
/// `CUSTOMER_SUPPORT`, `SURVEY`, `OTHER`) and say "multiple values are
/// possible".
pub const MAX_CATEGORIES: usize = 8;

/// Longest data endpoint URL accepted. Meta's page states none; 2,048 is
/// the usual bound of a URL a browser or a proxy keeps.
pub const MAX_ENDPOINT_URI_CHARS: usize = 2048;

// ─── The answers ─────────────────────────────────────────────────────────

/// A problem Meta found in a Flow's JSON, as Meta writes it
/// (`flows/guides/flowsapi`): every one must be fixed before the Flow is
/// published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowIssue {
    /// Machine-readable error (`INVALID_PROPERTY_VALUE`).
    pub error: String,
    /// Its family (`FLOW_JSON_ERROR`).
    pub error_type: String,
    /// Meta's explanation of it.
    pub message: String,
    /// The first line of the offending span in the uploaded JSON.
    pub line_start: Option<u32>,
    /// Its last line.
    pub line_end: Option<u32>,
    /// Its first column.
    pub column_start: Option<u32>,
    /// Its last column.
    pub column_end: Option<u32>,
    /// The places in the Flow JSON it refers to.
    pub pointers: Vec<FlowIssuePointer>,
}

/// One place a [`FlowIssue`] points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowIssuePointer {
    /// First line.
    pub line_start: Option<u32>,
    /// Last line.
    pub line_end: Option<u32>,
    /// First column.
    pub column_start: Option<u32>,
    /// Last column.
    pub column_end: Option<u32>,
    /// The path inside the Flow JSON (`screens [0]. layout.children [0].type`,
    /// spacing as Meta writes it).
    pub path: Option<String>,
}

impl From<FlowValidationError> for FlowIssue {
    fn from(error: FlowValidationError) -> Self {
        let FlowValidationError {
            error,
            error_type,
            message,
            line_start,
            line_end,
            column_start,
            column_end,
            pointers,
        } = error;
        Self {
            error,
            error_type,
            message,
            line_start,
            line_end,
            column_start,
            column_end,
            pointers: pointers
                .into_iter()
                .map(|p| FlowIssuePointer {
                    line_start: p.line_start,
                    line_end: p.line_end,
                    column_start: p.column_start,
                    column_end: p.column_end,
                    path: p.path,
                })
                .collect(),
        }
    }
}

/// A Flow, as Meta describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowInfo {
    /// Its id.
    pub id: FlowId,
    /// Its name (not shown to users).
    pub name: Option<String>,
    /// Where it is in its lifecycle, as Meta names it (`DRAFT`,
    /// `PUBLISHED`, `DEPRECATED`, `BLOCKED`, `THROTTLED`).
    pub status: Option<String>,
    /// Its categories, as Meta names them.
    pub categories: Vec<String>,
    /// What blocks its publication.
    pub validation_errors: Vec<FlowIssue>,
    /// The `version` of its uploaded Flow JSON.
    pub json_version: Option<String>,
    /// The `data_api_version` of its uploaded Flow JSON (Flows with an
    /// endpoint).
    pub data_api_version: Option<String>,
    /// Its data endpoint's URL.
    pub endpoint_uri: Option<String>,
}

impl From<FlowDetails> for FlowInfo {
    fn from(flow: FlowDetails) -> Self {
        let FlowDetails {
            id,
            name,
            status,
            categories,
            validation_errors,
            json_version,
            data_api_version,
            endpoint_uri,
            // The WABA is the ownership check's; the preview has its own
            // call; the application is "an object" no page types.
            preview: _,
            whatsapp_business_account: _,
            application: _,
        } = flow;
        Self {
            id,
            name,
            status: wire(status.as_ref()),
            categories: categories.iter().filter_map(|c| wire(Some(c))).collect(),
            validation_errors: validation_errors.into_iter().map(FlowIssue::from).collect(),
            json_version,
            data_api_version,
            endpoint_uri,
        }
    }
}

/// One page of a WABA's Flows.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowPage {
    /// The Flows (Meta's default fields: id, name, status, categories,
    /// validation errors).
    pub flows: Vec<FlowInfo>,
    /// Meta's cursor for the page after, if there is one.
    pub next_after: Option<String>,
}

/// A created Flow.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowCreated {
    /// Its id.
    pub id: FlowId,
    /// The problems Meta found in the Flow JSON sent with it, if any: the
    /// Flow is created as a draft all the same.
    pub validation_errors: Vec<FlowIssue>,
}

/// What Meta made of an uploaded Flow JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowJsonUploaded {
    /// Meta's `success`: the upload was stored, errors or not.
    pub success: bool,
    /// The problems Meta found in it.
    pub validation_errors: Vec<FlowIssue>,
}

/// A Flow's web preview link. **A secret**: its token opens the Flow for
/// anyone holding it, for 30 days or until invalidated. `Debug` shows the
/// expiry only.
#[derive(Clone, PartialEq, Eq)]
pub struct FlowPreviewLink {
    url: String,
    /// When it expires, as Meta writes it (`2023-05-21T11:18:09+0000`).
    pub expires_at: String,
}

impl FlowPreviewLink {
    /// The link, token included. Never log it.
    pub fn expose_url(&self) -> &str {
        &self.url
    }
}

impl std::fmt::Debug for FlowPreviewLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowPreviewLink")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

// ─── The requests ────────────────────────────────────────────────────────

/// A Flow's creation, as the request carried it (`flows/guides/flowsapi`,
/// "Creating a Flow"): Meta's names, no other key.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewFlowBody {
    name: String,
    categories: Vec<String>,
    clone_flow_id: Option<String>,
    endpoint_uri: Option<String>,
    flow_json: Option<Value>,
    publish: Option<bool>,
}

/// A Flow's creation checked: the library's request, and the Flow to copy.
#[derive(Debug, Clone)]
pub struct NewFlow {
    request: CreateFlow,
    clone_of: Option<FlowId>,
    endpoint_uri: Option<String>,
}

/// A Flow's metadata change, as the request carried it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FlowChangeBody {
    name: Option<String>,
    categories: Option<Vec<String>>,
    endpoint_uri: Option<String>,
}

/// A Flow's metadata change checked.
#[derive(Debug, Clone)]
pub struct FlowChange {
    update: UpdateFlow,
    endpoint_uri: Option<String>,
}

/// `id` from a path or a body, a Meta id, else `422` on `field`.
fn flow_id(field: &'static str, id: &str) -> Result<FlowId, ServiceError> {
    graph_id(field, id)?;
    Ok(FlowId::new(id))
}

fn name(field: &'static str, name: &str) -> Result<(), ServiceError> {
    if name.trim().is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(ServiceError::invalid(field));
    }
    Ok(())
}

/// `categories` read as the library's, each one of Meta's (an unknown one
/// is Meta's to refuse, so a new category is never held back), 1 to
/// [`MAX_CATEGORIES`], none blank.
fn categories(field: &'static str, values: &[String]) -> Result<Vec<FlowCategory>, ServiceError> {
    if values.is_empty() || values.len() > MAX_CATEGORIES {
        return Err(ServiceError::invalid(field));
    }
    values
        .iter()
        .enumerate()
        .map(|(i, value)| {
            let valid = !value.is_empty()
                && value.len() <= 64
                && value.bytes().all(|b| b.is_ascii_uppercase() || b == b'_');
            if !valid {
                return Err(ServiceError::invalid(format!("{field}[{i}]")));
            }
            Ok(value
                .parse::<FlowCategory>()
                .unwrap_or_else(|never| match never {}))
        })
        .collect()
}

/// A data endpoint URL: `https`, a host, no credentials, at most
/// [`MAX_ENDPOINT_URI_CHARS`] (`flows/guides/implementingyourflowendpoint`:
/// "use HTTPS").
fn endpoint_uri(uri: &str) -> Result<(), ServiceError> {
    let invalid = || ServiceError::invalid("endpoint_uri");
    if uri.chars().count() > MAX_ENDPOINT_URI_CHARS {
        return Err(invalid());
    }
    let url = url::Url::parse(uri).map_err(|_| invalid())?;
    let ok = url.scheme() == "https"
        && url.host_str().is_some_and(|h| !h.is_empty())
        && url.username().is_empty()
        && url.password().is_none();
    if ok { Ok(()) } else { Err(invalid()) }
}

/// A data endpoint URL that names this service's own address for Flows
/// (`.../webhooks/flows/<id>`) must name the WABA the Flow is created or
/// changed in (design D136): Meta's request carries no tenant or Flow id,
/// so the address is what routes it to a tenant, and a tenant must not
/// point a Flow of its own at another tenant's address (its users' answers
/// would reach that tenant's app). A URL that does not have that shape is
/// the tenant's own endpoint, whatever the host. Path segments are compared
/// percent-decoded, so `%77ebhooks` does not slip by.
fn endpoint_names_waba(uri: &str, waba: &WabaId) -> Result<(), ServiceError> {
    let invalid = || ServiceError::invalid("endpoint_uri");
    let url = url::Url::parse(uri).map_err(|_| invalid())?;
    // Decoded as a whole, so an encoded slash splits a segment too.
    let path = percent_decoded(url.path());
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for window in segments.windows(3) {
        if window[0].eq_ignore_ascii_case("webhooks")
            && window[1].eq_ignore_ascii_case("flows")
            && window[2] != waba.as_str()
        {
            return Err(invalid());
        }
    }
    // The address with no id after it (`/webhooks/flows`) names no WABA.
    let n = segments.len();
    if n >= 2
        && segments[n - 2].eq_ignore_ascii_case("webhooks")
        && segments[n - 1].eq_ignore_ascii_case("flows")
    {
        return Err(invalid());
    }
    Ok(())
}

/// `text` with its `%XX` escapes decoded (bytes that are not UTF-8 become
/// U+FFFD, which matches nothing).
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&byte) = bytes.get(i) {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (byte, hex) {
            (b'%', Some(decoded)) => {
                out.push(decoded);
                i += 3;
            }
            _ => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A Flow JSON given as a request's `flow_json`: the text Meta takes, or
/// the JSON object itself (written compactly).
fn flow_json_text(value: &Value) -> Result<String, ServiceError> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Object(_) => value.to_string(),
        _ => return Err(ServiceError::invalid("flow_json")),
    };
    if text.is_empty() || text.len() > MAX_FLOW_JSON_BYTES {
        return Err(ServiceError::invalid("flow_json"));
    }
    Ok(text)
}

/// A Flow's creation in Meta's JSON (`name`, `categories`,
/// `clone_flow_id`, `endpoint_uri`, `flow_json` as Meta's text or as the
/// object, `publish`), checked before any request.
///
/// # Errors
///
/// `422 invalid_request` on `body` (not that object, or a key Meta's page
/// does not list), `name`, `categories` / `categories[i]`,
/// `clone_flow_id` (not a Meta id), `endpoint_uri` (not an `https` URL) or
/// `flow_json` (not an object or text, empty, or over 10 MiB).
pub fn new_flow(body: &Value) -> Result<NewFlow, ServiceError> {
    let NewFlowBody {
        name: flow_name,
        categories: flow_categories,
        clone_flow_id,
        endpoint_uri: uri,
        flow_json,
        publish,
    } = NewFlowBody::deserialize(body).map_err(|_| ServiceError::invalid("body"))?;
    name("name", &flow_name)?;
    let mut request = CreateFlow::new(flow_name, categories("categories", &flow_categories)?);
    let clone_of: Option<FlowId> = clone_flow_id
        .as_deref()
        .map(|id| flow_id("clone_flow_id", id))
        .transpose()?;
    if let Some(id) = &clone_of {
        request = request.clone_flow_id(id.clone());
    }
    if let Some(uri) = &uri {
        endpoint_uri(uri)?;
        request = request.endpoint_uri(uri.clone());
    }
    if let Some(flow_json) = &flow_json {
        request = request.flow_json(flow_json_text(flow_json)?);
    }
    if let Some(publish) = publish {
        request = request.publish(publish);
    }
    Ok(NewFlow {
        request,
        clone_of,
        endpoint_uri: uri,
    })
}

/// A Flow's metadata change in Meta's JSON (`name`, `categories`,
/// `endpoint_uri`), checked before any request.
///
/// # Errors
///
/// `422 invalid_request` on `body` (not that object, a key the library
/// does not send, or nothing to change), `name`, `categories` /
/// `categories[i]` or `endpoint_uri`.
pub fn flow_change(body: &Value) -> Result<FlowChange, ServiceError> {
    let FlowChangeBody {
        name: flow_name,
        categories: flow_categories,
        endpoint_uri: uri,
    } = FlowChangeBody::deserialize(body).map_err(|_| ServiceError::invalid("body"))?;
    if flow_name.is_none() && flow_categories.is_none() && uri.is_none() {
        return Err(ServiceError::invalid("body"));
    }
    let mut update = UpdateFlow::new();
    if let Some(flow_name) = flow_name {
        name("name", &flow_name)?;
        update = update.name(flow_name);
    }
    if let Some(values) = flow_categories {
        update = update.categories(categories("categories", &values)?);
    }
    if let Some(uri) = &uri {
        endpoint_uri(uri)?;
        update = update.endpoint_uri(uri.clone());
    }
    Ok(FlowChange {
        update,
        endpoint_uri: uri,
    })
}

/// A Flow JSON uploaded as the request's body: the bytes as they came
/// (the validation errors' lines refer to them), once they are a JSON
/// object of at most 10 MiB.
///
/// # Errors
///
/// `422 invalid_request` on `body`: not a JSON object, or empty. `413`
/// is the adapter's, at its own limit.
pub fn flow_json_upload(body: Bytes) -> Result<Bytes, ServiceError> {
    if body.is_empty() || body.len() > MAX_FLOW_JSON_BYTES {
        return Err(ServiceError::invalid("body"));
    }
    match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(_)) => Ok(body),
        _ => Err(ServiceError::invalid("body")),
    }
}

// ─── The operations ──────────────────────────────────────────────────────

/// The Flow operations on a WABA of the caller's tenant: an [`OwnedWaba`]
/// its [`Authorizer`] made from that tenant's binding, for a [`Caller`] it
/// made, both admitted ([`Self::new`]), with the WABA's token. A failed
/// Graph call is told to the [`Observer`] (the adapter's metrics), and a
/// `190` marks the WABA's numbers `reconnect_required`
/// ([`OwnedWaba::failed`]). Cheap: a few references.
pub struct Flows<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    waba: &'a OwnedWaba,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Flows<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Flows")
            .field("caller", self.caller)
            .field("waba", self.waba)
            .finish_non_exhaustive()
    }
}

impl<'a> Flows<'a> {
    /// The operations on `waba` for `caller`, once `authz` admits both and
    /// `waba` was made from a binding of `caller`'s tenant (as
    /// [`crate::templates::Templates::new`]).
    ///
    /// # Errors
    ///
    /// `caller` or `waba` another [`Authorizer`] made: `403 forbidden`.
    /// `waba` another tenant's: `404 not_found`. Either way, before
    /// anything is read or asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        waba: &'a OwnedWaba,
    ) -> Result<Self, ServiceError> {
        authz.admit(caller)?;
        authz.admit_waba(waba)?;
        if waba.tenant() != caller.tenant() {
            tracing::warn!("refused the Flow services on another tenant's WABA");
            return Err(ServiceError::not_found());
        }
        Ok(Self {
            authz,
            caller,
            waba,
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

    /// The caller's audit event `action` on the WABA.
    fn audit(&self, action: &'static str) {
        tenant_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.waba.waba_id().as_str()),
                ..Subject::default()
            },
        );
    }

    /// The Flow `id`, if it is the WABA's: `GET /{id}` (the library's
    /// `Flow::get`), whose `whatsapp_business_account` must be this WABA
    /// (see the [module](self)).
    async fn owned_flow(&self, id: &FlowId) -> Result<FlowDetails, ServiceError> {
        let flow = self.waba.client().flow(id.clone()).get().await;
        let flow = match flow {
            Ok(flow) => flow,
            Err(error) => return Err(object_failed(self.counted(&error).await, &error)),
        };
        let owner = flow
            .whatsapp_business_account
            .as_ref()
            .and_then(|waba| waba.get("id"))
            .and_then(Value::as_str);
        // The node answered is the one asked for: an id Meta resolves to
        // another node (an alias, a redirect) is not acted on as the Flow.
        if flow.id == *id && owner == Some(self.waba.waba_id().as_str()) {
            Ok(flow)
        } else {
            tracing::warn!("refused a Flow that is not the WABA's");
            Err(ServiceError::not_found())
        }
    }

    /// One page of the WABA's Flows, after Meta's cursor `after`.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn list(&self, after: Option<String>) -> Result<FlowPage, ServiceError> {
        let mut query = ListFlows::new();
        query.after = after;
        let page = self
            .waba
            .client()
            .flows(self.waba.waba_id().clone())
            .list(&query)
            .await;
        match page {
            Ok(page) => {
                let next_after = page.next_cursor().map(str::to_owned);
                Ok(FlowPage {
                    flows: page.data.into_iter().map(FlowInfo::from).collect(),
                    next_after,
                })
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The Flow `id`, if it is the WABA's.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `flow_id`, before any request. `404
    /// not_found`: Meta refusing the id, or a Flow (or node) not this
    /// WABA's. Meta's failure otherwise, with its `details`.
    pub async fn get(&self, id: &str) -> Result<FlowInfo, ServiceError> {
        let id = flow_id("flow_id", id)?;
        Ok(FlowInfo::from(self.owned_flow(&id).await?))
    }

    /// Create `flow` (read by [`new_flow`]) in the WABA: `POST
    /// /{waba_id}/flows`, the library's `Flows::create`, not replayed on a
    /// timeout. A Flow to copy is read first and must be the WABA's.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `endpoint_uri` when it is this service's
    /// address for another WABA's Flows (see [`endpoint_names_waba`]),
    /// before any request. `404 not_found` for a `clone_flow_id` not the
    /// WABA's, and nothing created. Meta's refusal or failure, with its `details` (a timeout
    /// `may_have_been_sent`).
    pub async fn create(&self, flow: &NewFlow) -> Result<FlowCreated, ServiceError> {
        if let Some(uri) = &flow.endpoint_uri {
            endpoint_names_waba(uri, self.waba.waba_id())?;
        }
        if let Some(source) = &flow.clone_of {
            self.owned_flow(source).await?;
        }
        let created = self
            .waba
            .client()
            .flows(self.waba.waba_id().clone())
            .create(&flow.request)
            .await;
        match created {
            Ok(created) => Ok(FlowCreated {
                id: created.id,
                validation_errors: created
                    .validation_errors
                    .into_iter()
                    .map(FlowIssue::from)
                    .collect(),
            }),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Change the Flow `id`'s name, categories or endpoint (`change` read
    /// by [`flow_change`]), if it is the WABA's: `POST /{flow_id}`. The
    /// caller's audit event is `flow_updated`.
    ///
    /// # Errors
    ///
    /// As [`Self::get`]; `422` on `endpoint_uri` as [`Self::create`]
    /// (before the read); Meta's refusal of an edit (a published Flow's) is
    /// Meta's, with its `details`.
    pub async fn update(&self, id: &str, change: &FlowChange) -> Result<(), ServiceError> {
        let id = flow_id("flow_id", id)?;
        if let Some(uri) = &change.endpoint_uri {
            endpoint_names_waba(uri, self.waba.waba_id())?;
        }
        self.owned_flow(&id).await?;
        let updated = self.waba.client().flow(id).update(&change.update).await;
        match updated {
            Ok(()) => {
                self.audit("flow_updated");
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Replace the Flow `id`'s Flow JSON with `json` (read by
    /// [`flow_json_upload`]), if it is the WABA's: `POST
    /// /{flow_id}/assets`. Meta's answer carries the validation errors.
    /// The caller's audit event is `flow_json_uploaded`.
    ///
    /// # Errors
    ///
    /// As [`Self::get`].
    pub async fn upload_json(
        &self,
        id: &str,
        json: Bytes,
    ) -> Result<FlowJsonUploaded, ServiceError> {
        let id = flow_id("flow_id", id)?;
        self.owned_flow(&id).await?;
        let uploaded = self.waba.client().flow(id).upload_flow_json(json).await;
        match uploaded {
            Ok(uploaded) => {
                self.audit("flow_json_uploaded");
                Ok(FlowJsonUploaded {
                    success: uploaded.success,
                    validation_errors: uploaded
                        .validation_errors
                        .into_iter()
                        .map(FlowIssue::from)
                        .collect(),
                })
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The web preview link of the Flow `id`, if it is the WABA's: `GET
    /// /{flow_id}?fields=preview.invalidate(…)`. Asking mints a link;
    /// `invalidate` revokes the current one first.
    ///
    /// # Errors
    ///
    /// As [`Self::get`].
    pub async fn preview(
        &self,
        id: &str,
        invalidate: bool,
    ) -> Result<FlowPreviewLink, ServiceError> {
        let id = flow_id("flow_id", id)?;
        self.owned_flow(&id).await?;
        let preview = self.waba.client().flow(id).preview(invalidate).await;
        match preview {
            Ok(preview) => Ok(FlowPreviewLink {
                url: preview.preview_url.expose_secret().to_owned(),
                expires_at: preview.expires_at_raw,
            }),
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Publish the Flow `id`, if it is the WABA's: `POST
    /// /{flow_id}/publish`. The caller's audit event is `flow_published`.
    ///
    /// # Errors
    ///
    /// As [`Self::get`]; Meta's refusal while validation errors or
    /// publishing checks are outstanding is Meta's, with its `details`.
    pub async fn publish(&self, id: &str) -> Result<(), ServiceError> {
        let id = flow_id("flow_id", id)?;
        self.owned_flow(&id).await?;
        let published = self.waba.client().flow(id).publish().await;
        match published {
            Ok(()) => {
                self.audit("flow_published");
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Deprecate the Flow `id`, if it is the WABA's: `POST
    /// /{flow_id}/deprecate`. The caller's audit event is
    /// `flow_deprecated`.
    ///
    /// # Errors
    ///
    /// As [`Self::get`].
    pub async fn deprecate(&self, id: &str) -> Result<(), ServiceError> {
        let id = flow_id("flow_id", id)?;
        self.owned_flow(&id).await?;
        let deprecated = self.waba.client().flow(id).deprecate().await;
        match deprecated {
            Ok(()) => {
                self.audit("flow_deprecated");
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// Delete the Flow `id`, if it is the WABA's: `DELETE /{flow_id}`
    /// (Meta deletes drafts only). The caller's audit event is
    /// `flow_deleted`.
    ///
    /// # Errors
    ///
    /// As [`Self::get`].
    pub async fn delete(&self, id: &str) -> Result<(), ServiceError> {
        let id = flow_id("flow_id", id)?;
        self.owned_flow(&id).await?;
        let deleted = self.waba.client().flow(id).delete().await;
        match deleted {
            Ok(()) => {
                self.audit("flow_deleted");
                Ok(())
            }
            Err(error) => Err(self.failed(&error).await),
        }
    }

    /// The WABA these operations act on.
    pub fn waba_id(&self) -> &WabaId {
        self.waba.waba_id()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn field(error: &ServiceError) -> String {
        error.field().unwrap_or("?").to_owned()
    }

    /// A creation is Meta's object, checked before any request.
    /// Decisive: each check of [`new_flow`].
    #[test]
    fn a_creation_is_read_then_checked() {
        let good = json!({"name": "My first flow", "categories": ["OTHER"],
            "flow_json": "{\"version\":\"5.0\"}", "publish": true,
            "endpoint_uri": "https://example.com/flows", "clone_flow_id": "550"});
        let flow = new_flow(&good).unwrap();
        assert_eq!(flow.clone_of.as_ref().map(FlowId::as_str), Some("550"));
        assert_eq!(flow.request.publish, Some(true));
        let with = |key: &str, value: Value| {
            let mut body = good.clone();
            body[key] = value;
            body
        };
        let object = json!({"version": "5.0", "screens": []});
        assert_eq!(
            new_flow(&with("flow_json", object.clone()))
                .unwrap()
                .request
                .flow_json
                .as_deref(),
            Some(r#"{"screens":[],"version":"5.0"}"#)
        );
        for (body, at) in [
            (json!("x"), "body"),
            (json!({"name": "x"}), "body"),
            (with("flow", json!(1)), "body"),
            (with("name", json!(" ")), "name"),
            (with("name", json!("n".repeat(257))), "name"),
            (with("categories", json!([])), "categories"),
            (with("categories", json!(["other"])), "categories[0]"),
            (with("categories", json!(["OTHER", "A B"])), "categories[1]"),
            (with("clone_flow_id", json!("0550")), "clone_flow_id"),
            (with("clone_flow_id", json!("../x")), "clone_flow_id"),
            (
                with("endpoint_uri", json!("http://example.com")),
                "endpoint_uri",
            ),
            (
                with("endpoint_uri", json!("https://user:pw@example.com/")),
                "endpoint_uri",
            ),
            (with("endpoint_uri", json!("not a url")), "endpoint_uri"),
            (with("flow_json", json!(3)), "flow_json"),
            (with("flow_json", json!("")), "flow_json"),
            (
                with("flow_json", json!("x".repeat(MAX_FLOW_JSON_BYTES + 1))),
                "flow_json",
            ),
        ] {
            assert_eq!(field(&new_flow(&body).unwrap_err()), at, "{body}");
        }
    }

    /// A Flow's endpoint may not be this service's address for another
    /// WABA (design D136). Decisive: `endpoint_names_waba`.
    #[test]
    fn an_endpoint_may_not_name_another_wabas_address() {
        let waba = WabaId::new("102290129340398").unwrap();
        for ok in [
            "https://example.com/flows",
            "https://flows.example.com/webhooks/flows/102290129340398",
            "https://example.com/pfx/webhooks/flows/102290129340398/",
            "https://example.com/webhooks/other/1",
            "https://example.com/webhooks/meta",
        ] {
            assert!(endpoint_names_waba(ok, &waba).is_ok(), "{ok}");
        }
        for bad in [
            "https://example.com/webhooks/flows/102290129340399",
            "https://example.com/webhooks/flows",
            "https://example.com/webhooks/flows/",
            "https://example.com/webhooks/Flows/102290129340399",
            "https://example.com/%77ebhooks/flows/102290129340399",
            "https://example.com/webhooks%2Fflows/102290129340399",
            "https://example.com/x/../webhooks/flows/102290129340399",
            "https://example.com/webhooks/flows/102290129340398x",
        ] {
            assert_eq!(
                field(&endpoint_names_waba(bad, &waba).unwrap_err()),
                "endpoint_uri",
                "{bad}"
            );
        }
    }

    /// A metadata change is Meta's object with at least one field.
    /// Decisive: each check of [`flow_change`].
    #[test]
    fn a_change_is_read_then_checked() {
        assert!(flow_change(&json!({"name": "New flow name"})).is_ok());
        assert!(flow_change(&json!({"categories": ["SURVEY"]})).is_ok());
        for (body, at) in [
            (json!({}), "body"),
            (json!({"name": "x", "application_id": "1"}), "body"),
            (json!({"name": ""}), "name"),
            (json!({"categories": []}), "categories"),
            (json!({"categories": ["survey"]}), "categories[0]"),
            (json!({"endpoint_uri": "ftp://x.test"}), "endpoint_uri"),
        ] {
            assert_eq!(field(&flow_change(&body).unwrap_err()), at, "{body}");
        }
    }

    /// An upload is a JSON object, kept byte for byte.
    #[test]
    fn an_upload_is_a_json_object_kept_as_sent() {
        let text = Bytes::from_static(b"{\n  \"version\": \"5.0\"\n}");
        assert_eq!(flow_json_upload(text.clone()).unwrap(), text);
        for bad in [&b""[..], b"[]", b"\"x\"", b"{", b"null"] {
            assert_eq!(
                field(&flow_json_upload(Bytes::copy_from_slice(bad)).unwrap_err()),
                "body"
            );
        }
    }

    /// The preview link's token never reaches `Debug`.
    #[test]
    fn a_preview_links_token_is_not_in_debug() {
        let link = FlowPreviewLink {
            url: "https://business.facebook.com/wa/manage/flows/1/preview/?token=SECRET".to_owned(),
            expires_at: "2023-05-21T11:18:09+0000".to_owned(),
        };
        let shown = format!("{link:?}");
        assert!(
            !shown.contains("SECRET") && !shown.contains("business.facebook"),
            "{shown}"
        );
        assert!(shown.contains("2023-05-21"));
    }
}
