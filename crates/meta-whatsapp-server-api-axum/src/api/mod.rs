//! The internal listener's `/v1` API on axum and its OpenAPI document
//! (docs/design/server.md, sections 2 and 4), with what the HTTP crate
//! shares re-exported here: the public listener's router, Meta's webhook,
//! media and operations (`meta-whatsapp-server-http`'s `api`).
//!
//! | Listener | Routes |
//! | --- | --- |
//! | public (`WA_SERVER_PUBLIC_BIND`) | `GET` and `POST /webhooks/meta` (Meta's subscription check and deliveries, bodies up to 3 MiB), `GET /livez`, nothing else ([`public_router`], the HTTP crate's) |
//! | internal (`WA_SERVER_INTERNAL_BIND`, loopback by default) | `/v1/admin/…` (admin key), `/v1/wabas`, `/v1/numbers/…` (tenant or platform key: scope `numbers`, registration and the two-step PIN included; messages `send`; media `media`; templates `templates`; conversations `inbox`; payment configurations, lookups and refunds `payments`; erasing a customer `erasure`; commerce settings and catalogs `commerce`; QR codes `qr_codes`; analytics `analytics`; the block list `contacts`), `/v1/events` (scope `events`), `/v1/webhook-endpoints` (scope `webhooks`), `/v1/otp/…` (scope `otp`), `/v1/signup/…` (scope `signup`), `/livez`, `/readyz`, `/metrics`, `/v1/openapi.json`, `/v1/version` |
//!
//! Every API route is registered through utoipa-axum's `routes!`, which
//! adds the handler and its `#[utoipa::path]` documentation at once: a
//! route cannot be served without being in the OpenAPI document, which is
//! what the tests iterate (the committed copy is
//! `crates/meta-whatsapp-server/openapi/v1.json`, beside the binary whose
//! `openapi` command prints it). The router and the document are built
//! from one list of route groups (`api_routes`), the HTTP crate's media and
//! operations groups among them, so no group is served without being
//! documented either.

pub mod admin;
pub mod analytics;
pub mod blocked_users;
pub mod commerce;
pub mod erasure;
pub mod events;
pub mod inbox;
mod listing;
pub mod messages;
pub mod numbers;
pub mod otp;
pub mod payments;
pub mod qr_codes;
pub mod registration;
pub mod signup;
pub mod template_groups;
pub mod template_lifecycle;
pub mod templates;
pub mod webhook_endpoints;

pub use meta_whatsapp_server_http::api::{
    MAX_BODY_BYTES, PUBLIC_OPERATIONS, PUBLIC_ROUTES, REQUEST_DEADLINE, common, media, ops,
    public_router, webhooks, with_deadline,
};

use std::sync::LazyLock;

use meta_whatsapp_rs::webhooks::axum;
use meta_whatsapp_rs::webhooks::axum::Router;
use meta_whatsapp_rs::webhooks::axum::extract::DefaultBodyLimit;
use meta_whatsapp_rs::webhooks::axum::http::{StatusCode, header};
use meta_whatsapp_rs::webhooks::axum::middleware;
use meta_whatsapp_rs::webhooks::axum::response::{IntoResponse, Response};
use meta_whatsapp_server_http::api::internal_router_with;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::auth::{Operation, Operations, admin_guard, guard, tenant_guard};
use crate::error::{ErrorBody, ErrorCodeSchema, KnownErrorCodeSchema};
use crate::model::Scope;
use crate::state::Grant;

struct Security;

impl Modify for Security {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "api_key",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("wak_<key id>_<secret>")
                    .description(Some(
                        "An API key: a tenant key, a platform key (with the WA-Tenant header) or \
                         an admin key (/v1/admin only).",
                    ))
                    .build(),
            ),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "meta-whatsapp-server",
        description = "The meta-whatsapp-rs HTTP service: WhatsApp Business Platform over REST, for \
                       apps not written in Rust. Errors: every error answers `ErrorBody`; branch on \
                       `error.code` (`ErrorCode`), and resend only when `may_have_been_sent` is false."
    ),
    modifiers(&Security),
    components(schemas(ErrorBody, ErrorCodeSchema, KnownErrorCodeSchema, events::EventType, events::KnownEventType, messages::CtaUrlParameters, messages::OrderParameters, messages::FlowParameters, messages::VoiceCallParameters, messages::AddressParameters, messages::CatalogParameters, messages::ListSectionObject, messages::ProductSectionObject)),
    tags(
        (name = "admin", description = "Tenants, keys, WABA bindings and the vault key (admin key)"),
        (name = "numbers", description = "WABAs, numbers and business profiles (scope `numbers`)"),
        (name = "registration", description = "A number's verification code, registration, deregistration and two-step PIN, the PIN typed per attempt and never stored (scope `numbers`)"),
        (name = "messages", description = "Sending messages and read receipts (scope `send`)"),
        (name = "media", description = "Uploading, downloading and deleting media (scope `media`)"),
        (name = "templates", description = "Message templates (scope `templates`): listing, creating and deleting them; since roadmap M5b editing, unpausing, comparing, archiving and unarchiving them, the Template Library, migrations and template groups"),
        (name = "payments", description = "India's payment configurations, a payment's lookup and its refunds (scope `payments`); the payment messages are sends (scope `send`)"),
        (name = "commerce", description = "A number's commerce settings (the cart, the catalog's visibility) and the catalog its WABA connects, with its products (scope `commerce`)"),
        (name = "qr_codes", description = "QR codes and short links that open a chat with a prefilled message, each checked to be the number's own (scope `qr_codes`)"),
        (name = "analytics", description = "A WABA's messaging, conversation, pricing and template analytics, their ranges bounded (scope `analytics`)"),
        (name = "contacts", description = "The block list: blocking and unblocking people on a number, audited by count (scope `contacts`)"),
        (name = "inbox", description = "Conversations, their history and replies, checked against the 24-hour window and the thread's owner (scope `inbox`)"),
        (name = "erasure", description = "Erasing a customer on a number, from the inbox and the event outbox, journaled and audited: irreversible, only when called (scope `erasure`)"),
        (name = "events", description = "Meta's webhook events, routed to their tenant (scope `events`)"),
        (name = "webhooks", description = "Webhook endpoints: the tenant's events pushed to its URLs, signed the Standard Webhooks way, retried for 72 hours; their secrets, pings and deliveries (scope `webhooks`)"),
        (name = "otp", description = "One-time passcodes sent with the tenant's authentication template, and checked (scope `otp`)"),
        (name = "signup", description = "Embedded Signup: an attempt bound to the tenant, its completion (the business onboarded, the WABA and its numbers bound to the tenant), and the attempts to resume (scope `signup`)"),
        (name = "operations", description = "Health, metrics, this document, versions (no key)"),
    )
)]
struct ApiDoc;

fn admin_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(admin::create_tenant, admin::list_tenants))
        .routes(routes!(
            admin::get_tenant,
            admin::update_tenant,
            admin::delete_tenant
        ))
        .routes(routes!(admin::mint_tenant_key, admin::list_tenant_keys))
        .routes(routes!(admin::revoke_tenant_key))
        .routes(routes!(admin::mint_platform_key, admin::list_platform_keys))
        .routes(routes!(admin::revoke_platform_key))
        .routes(routes!(admin::attach_waba))
        .routes(routes!(
            admin::get_otp_settings,
            admin::set_otp_settings,
            admin::clear_otp_settings
        ))
        .routes(routes!(admin::get_waba))
        .routes(routes!(admin::unbind_waba))
        .routes(routes!(admin::rotate_vault))
        .routes(routes!(signup::reshare_credit_line))
}

fn numbers_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(numbers::list_wabas))
        .routes(routes!(numbers::list_numbers))
        .routes(routes!(numbers::get_number))
        .routes(routes!(numbers::get_profile, numbers::update_profile))
        .routes(routes!(numbers::disconnect_waba))
}

fn registration_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(registration::request_verification_code))
        .routes(routes!(registration::verify_code))
        .routes(routes!(registration::register_number))
        .routes(routes!(registration::deregister_number))
        .routes(routes!(registration::set_two_step_pin))
}

fn events_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(events::list_events))
        .routes(routes!(events::get_event))
        // The live streams (Server-Sent Events): the HTTP crate's, as
        // media's are.
        .merge(events::stream_routes())
}

fn messages_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(messages::send_message))
        .routes(routes!(messages::mark_read))
}

fn inbox_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(inbox::list_conversations))
        .routes(routes!(
            inbox::list_conversation_messages,
            inbox::reply_to_conversation
        ))
        .routes(routes!(inbox::get_conversation_window))
        .routes(routes!(inbox::mark_conversation_read))
        .routes(routes!(inbox::get_inbox_message))
}

fn erasure_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new().routes(routes!(erasure::erase_customer))
}

fn payments_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(
            payments::list_payment_configurations,
            payments::create_payment_configuration
        ))
        .routes(routes!(
            payments::get_payment_configuration,
            payments::delete_payment_configuration
        ))
        .routes(routes!(payments::regenerate_payment_oauth_link))
        .routes(routes!(payments::get_payment))
        .routes(routes!(payments::refund_payment))
}

fn templates_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(
            templates::list_templates,
            templates::create_template,
            templates::delete_templates
        ))
        .routes(routes!(
            templates::get_template,
            template_lifecycle::edit_template
        ))
        .routes(routes!(templates::create_authentication_template))
        // Roadmap M5b.
        .routes(routes!(template_lifecycle::unpause_template))
        .routes(routes!(template_lifecycle::compare_templates))
        .routes(routes!(template_lifecycle::archive_templates))
        .routes(routes!(template_lifecycle::unarchive_templates))
        .routes(routes!(
            template_lifecycle::list_library_templates,
            template_lifecycle::create_library_template
        ))
        .routes(routes!(template_lifecycle::migrate_templates))
        .routes(routes!(template_groups::create_template_group))
        .routes(routes!(
            template_groups::get_template_group,
            template_groups::update_template_group,
            template_groups::delete_template_group
        ))
}

fn commerce_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(
            commerce::get_commerce_settings,
            commerce::update_commerce_settings
        ))
        .routes(routes!(commerce::list_catalogs))
        .routes(routes!(commerce::get_catalog))
        .routes(routes!(commerce::list_catalog_products))
}

fn qr_codes_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(qr_codes::create_qr_code, qr_codes::list_qr_codes))
        .routes(routes!(
            qr_codes::get_qr_code,
            qr_codes::update_qr_code,
            qr_codes::delete_qr_code
        ))
}

fn analytics_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(analytics::get_messaging_analytics))
        .routes(routes!(analytics::get_conversation_analytics))
        .routes(routes!(analytics::get_pricing_analytics))
        .routes(routes!(analytics::get_template_analytics))
}

fn contacts_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(
            blocked_users::list_blocked_users,
            blocked_users::block_users
        ))
        .routes(routes!(blocked_users::unblock_users))
}

fn webhook_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(
            webhook_endpoints::create_webhook_endpoint,
            webhook_endpoints::list_webhook_endpoints
        ))
        .routes(routes!(
            webhook_endpoints::get_webhook_endpoint,
            webhook_endpoints::update_webhook_endpoint,
            webhook_endpoints::delete_webhook_endpoint
        ))
        .routes(routes!(webhook_endpoints::rotate_webhook_secret))
        .routes(routes!(webhook_endpoints::test_webhook_endpoint))
        .routes(routes!(webhook_endpoints::list_webhook_deliveries))
        .routes(routes!(webhook_endpoints::retry_webhook_delivery))
}

fn otp_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(otp::issue_otp))
        .routes(routes!(otp::verify_otp))
}

/// Embedded Signup's routes, their bodies at most 16 KiB (the design's
/// limit for the completion, section 4.1; roadmap M3a).
fn signup_routes() -> OpenApiRouter<Grant> {
    OpenApiRouter::new()
        .routes(routes!(signup::start_signup))
        .routes(routes!(signup::complete_signup))
        .routes(routes!(signup::list_signup_attempts))
        .routes(routes!(signup::resume_signup))
        .layer(DefaultBodyLimit::max(signup::MAX_SIGNUP_BODY_BYTES))
}

/// The HTTP crate's operations routes, and this document's own.
fn ops_routes() -> OpenApiRouter<Grant> {
    ops::routes().routes(routes!(openapi_json))
}

/// `GET /v1/openapi.json`: this document.
#[utoipa::path(
    get,
    path = "/v1/openapi.json",
    tag = "operations",
    responses((status = 200, description = "The OpenAPI 3.1 document", content_type = "application/json", body = Object))
)]
pub async fn openapi_json() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        openapi_document(),
    )
        .into_response()
}

/// The internal listener's route groups, one list for the router and the
/// document: a group the router served but the document left out would
/// escape every test that iterates the document (the `429` and
/// authorization ones among them). `admin` wraps `/v1/admin`, `tenant`
/// each tenant group with its scope: the document passes them through,
/// the router adds the guards. The media and operations groups are the
/// HTTP crate's (`media::routes`, `ops::routes`).
fn api_routes(
    admin: impl FnOnce(OpenApiRouter<Grant>) -> OpenApiRouter<Grant>,
    tenant: impl Fn(OpenApiRouter<Grant>, Scope) -> OpenApiRouter<Grant>,
) -> OpenApiRouter<Grant> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(admin(admin_routes()))
        .merge(tenant(numbers_routes(), Scope::Numbers))
        .merge(tenant(registration_routes(), Scope::Numbers))
        .merge(tenant(messages_routes(), Scope::Send))
        .merge(tenant(media::routes(), Scope::Media))
        .merge(tenant(templates_routes(), Scope::Templates))
        .merge(tenant(inbox_routes(), Scope::Inbox))
        .merge(tenant(payments_routes(), Scope::Payments))
        .merge(tenant(erasure_routes(), Scope::Erasure))
        .merge(tenant(commerce_routes(), Scope::Commerce))
        .merge(tenant(qr_codes_routes(), Scope::QrCodes))
        .merge(tenant(analytics_routes(), Scope::Analytics))
        .merge(tenant(contacts_routes(), Scope::Contacts))
        .merge(tenant(events_routes(), Scope::Events))
        .merge(tenant(webhook_routes(), Scope::Webhooks))
        .merge(tenant(otp_routes(), Scope::Otp))
        .merge(tenant(signup_routes(), Scope::Signup))
        .merge(ops_routes())
}

/// The OpenAPI document of the internal listener's routes.
pub fn openapi() -> utoipa::openapi::OpenApi {
    let (_, mut document) = api_routes(|routes| routes, |routes, _| routes).split_for_parts();
    add_default_errors(&mut document);
    document
}

/// Every operation may also answer statuses it does not list (a `503
/// storage_unavailable`, a `429`, a `413`, a `504` at the deadline): each
/// gets a `default` response with the error body, so a generated client
/// types every failure.
fn add_default_errors(document: &mut utoipa::openapi::OpenApi) {
    use utoipa::openapi::{ContentBuilder, Ref, ResponseBuilder};
    let default = ResponseBuilder::new()
        .description("Any other error: branch on `error.code`")
        .content(
            "application/json",
            ContentBuilder::new()
                .schema(Some(Ref::from_schema_name("ErrorBody")))
                .build(),
        )
        .build();
    for item in document.paths.paths.values_mut() {
        let operations = [
            &mut item.get,
            &mut item.put,
            &mut item.post,
            &mut item.delete,
            &mut item.patch,
        ];
        for operation in operations.into_iter().flatten() {
            operation
                .responses
                .responses
                .entry("default".to_owned())
                .or_insert_with(|| default.clone().into());
        }
    }
}

/// The OpenAPI document as committed: pretty JSON and a final newline.
pub fn openapi_document() -> String {
    static DOCUMENT: LazyLock<String> = LazyLock::new(|| {
        let mut json = openapi().to_pretty_json().unwrap_or_default();
        json.push('\n');
        json
    });
    DOCUMENT.clone()
}

/// The internal listener's route templates, for logs and metrics.
pub fn internal_routes() -> Vec<String> {
    openapi().paths.paths.keys().cloned().collect()
}

/// The tenant operations of the internal listener, by method and route
/// template: every operation of the document whose `operationId` is a
/// core [`Operation`]'s id (the admin and operations routes are none),
/// what the tenant guard charges ([`Operations`]). The `tenant_operations`
/// tests check each tenant route has one, with its group's scope.
pub fn tenant_operations() -> Operations {
    let document = openapi();
    let mut routes = Vec::new();
    for (template, item) in &document.paths.paths {
        let methods = [
            ("GET", &item.get),
            ("PUT", &item.put),
            ("POST", &item.post),
            ("DELETE", &item.delete),
            ("PATCH", &item.patch),
        ];
        for (method, operation) in methods {
            let id = operation
                .as_ref()
                .and_then(|operation| operation.operation_id.as_deref());
            if let Some(operation) = id.and_then(Operation::from_id) {
                routes.push((method.to_owned(), template.clone(), operation));
            }
        }
    }
    Operations::new(routes)
}

/// The internal listener's router, served with the state's `grant` (which
/// only the state's composer holds: `AppState::from_backend`): the route
/// groups behind their guards, in the HTTP crate's layers
/// ([`internal_router_with`]).
pub fn internal_router(grant: &Grant) -> Router {
    let state = grant.state();
    let admin = |routes: OpenApiRouter<Grant>| {
        routes.route_layer(middleware::from_fn_with_state(state.clone(), admin_guard))
    };
    let operations = tenant_operations();
    let tenant = |routes: OpenApiRouter<Grant>, scope: Scope| {
        routes.route_layer(middleware::from_fn_with_state(
            guard(state, scope, &operations),
            tenant_guard,
        ))
    };
    let (router, _) = api_routes(admin, tenant).split_for_parts();
    internal_router_with(router, grant, internal_routes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ratelimit::RouteClass;

    /// `/v1`'s budgets did not move when the class became the operation's
    /// (roadmap S6): each tenant route's operation is in the class its
    /// method and scope gave it before (template management for the
    /// templates scope, a read for `GET`, a send for any other method).
    /// (In the service's `ratelimit` until roadmap S7.)
    #[test]
    fn v1s_routes_keep_their_class() {
        let operations = tenant_operations();
        assert!(operations.routes().len() >= 16);
        for (method, template, operation) in operations.routes() {
            // OTP's routes came after: classes of their own (roadmap M3c).
            if operation.scope() == Scope::Otp {
                let class = if *operation == Operation::IssueOtp {
                    RouteClass::OtpIssue
                } else {
                    RouteClass::OtpVerify
                };
                assert_eq!(RouteClass::of(*operation), class, "{method} {template}");
                continue;
            }
            let before = if operation.scope() == Scope::Templates {
                RouteClass::Templates
            } else if method == "GET" {
                RouteClass::Read
            } else {
                RouteClass::Send
            };
            assert_eq!(RouteClass::of(*operation), before, "{method} {template}");
        }
    }

    /// Every API route is added with `routes!`, which documents it: no
    /// file of this crate adds a plain axum route (the only ones are the
    /// public listener's, in the HTTP crate, whose own test checks its
    /// files). A route added any other way would escape the tests that
    /// iterate the document (M1.3's among them).
    #[test]
    fn routes_are_only_added_with_their_documentation() {
        let sources = [
            ("mod.rs", include_str!("mod.rs")),
            ("admin.rs", include_str!("admin.rs")),
            ("erasure.rs", include_str!("erasure.rs")),
            ("numbers.rs", include_str!("numbers.rs")),
            ("registration.rs", include_str!("registration.rs")),
            ("events.rs", include_str!("events.rs")),
            ("inbox.rs", include_str!("inbox.rs")),
            ("messages.rs", include_str!("messages.rs")),
            ("payments.rs", include_str!("payments.rs")),
            ("commerce.rs", include_str!("commerce.rs")),
            ("qr_codes.rs", include_str!("qr_codes.rs")),
            ("analytics.rs", include_str!("analytics.rs")),
            ("blocked_users.rs", include_str!("blocked_users.rs")),
            ("listing.rs", include_str!("listing.rs")),
            ("templates.rs", include_str!("templates.rs")),
            (
                "template_lifecycle.rs",
                include_str!("template_lifecycle.rs"),
            ),
            ("template_groups.rs", include_str!("template_groups.rs")),
            ("otp.rs", include_str!("otp.rs")),
            ("signup.rs", include_str!("signup.rs")),
            ("webhook_endpoints.rs", include_str!("webhook_endpoints.rs")),
        ];
        let mut plain = Vec::new();
        for (file, source) in sources {
            // The code, not this test.
            let code = source.split("#[cfg(test)]").next().unwrap_or(source);
            for needle in [".route(", ".route_service(", ".nest(", ".nest_service("] {
                for (i, _) in code.match_indices(needle) {
                    let line = code[i..].lines().next().unwrap_or_default();
                    plain.push(format!("{file}: {line}"));
                }
            }
        }
        assert_eq!(plain, Vec::<String>::new());
    }

    /// Whether some path matches both templates: the same number of
    /// segments, and wherever both are literal, the same literal.
    fn templates_overlap(a: &str, b: &str) -> bool {
        let param = |s: &str| s.starts_with('{') && s.ends_with('}');
        let (a, b): (Vec<_>, Vec<_>) = (a.split('/').collect(), b.split('/').collect());
        a.len() == b.len()
            && a.iter()
                .zip(&b)
                .all(|(x, y)| param(x) || param(y) || x == y)
    }

    /// Whether, at the first segment where `a` and `b` differ, one has a
    /// literal and the other a parameter: the router then routes the
    /// literal's path to its template.
    fn literal_beats_parameter(a: &str, b: &str) -> bool {
        let param = |s: &str| s.starts_with('{') && s.ends_with('}');
        a.split('/')
            .zip(b.split('/'))
            .find(|(x, y)| x != y)
            .is_some_and(|(x, y)| param(x) != param(y))
    }

    /// Every tenant route is one core operation (roadmap S6): each
    /// operation of a tenant group's document has an `operationId` that is
    /// a core [`Operation`]'s id, with its group's scope, and the guard's
    /// [`Operations`] resolves a path of its template, with its method (and
    /// `HEAD` for a `GET`), to it, an empty parameter included (axum
    /// routes one), and no path could match another template of its
    /// method: two templates a path could match would charge one
    /// operation's budget for another's. Every core operation is served
    /// exactly once. Decisive: a tenant route the guard cannot resolve, or
    /// resolves to another operation.
    #[test]
    fn each_tenant_route_is_one_core_operation_with_its_groups_scope() {
        let grouped = std::cell::RefCell::new(Vec::new());
        let _ = api_routes(
            |routes| routes,
            |routes, scope| {
                for (template, item) in &routes.get_openapi().paths.paths {
                    for (method, operation) in [
                        ("GET", &item.get),
                        ("PUT", &item.put),
                        ("POST", &item.post),
                        ("DELETE", &item.delete),
                        ("PATCH", &item.patch),
                    ] {
                        if let Some(operation) = operation {
                            let id = operation.operation_id.clone().unwrap();
                            grouped
                                .borrow_mut()
                                .push((method, template.clone(), id, scope));
                        }
                    }
                }
                routes
            },
        );
        // The overlap check itself: a literal against a parameter, two
        // parameters crossed, and literals apart.
        assert!(templates_overlap("/v1/numbers/{pn}", "/v1/numbers/stats"));
        assert!(templates_overlap("/v1/{a}/x", "/v1/y/{b}"));
        assert!(!templates_overlap(
            "/v1/numbers/{pn}/profile",
            "/v1/wabas/{waba_id}/templates"
        ));
        assert!(!templates_overlap("/v1/numbers/{pn}", "/v1/numbers"));
        let operations = tenant_operations();
        let mut served = std::collections::HashSet::new();
        for (method, template, id, scope) in &grouped.into_inner() {
            let operation = Operation::from_id(id)
                .unwrap_or_else(|| panic!("{method} {template}: `{id}` is no core operation"));
            assert_eq!(operation.scope(), *scope, "{id}");
            assert!(served.insert(operation), "{id} served twice");
            // No path of this template is another's of the same method,
            // but where one template has a literal segment and the other a
            // parameter (`/v1/events/stream` and `/v1/events/{id}`): axum
            // routes the literal's paths to it, and so does the guard
            // (`Operations::of`, checked below with the literal path).
            let others: Vec<_> = operations
                .routes()
                .iter()
                .filter(|(m, t, _)| m == method && t != template)
                .filter(|(_, t, _)| templates_overlap(t, template))
                .filter(|(_, t, _)| !literal_beats_parameter(t, template))
                .collect();
            assert!(
                others.is_empty(),
                "{method} {template}: a path could also match {others:?}"
            );
            // A path of the template, its parameters filled in, spelled
            // plainly or percent-encoded: the guard reads the path as the
            // request carries it.
            // And empty: axum routes an empty segment between two others
            // to its template (`/v1/numbers//profile`), so the guard does.
            for value in ["106540352242922", "%31%30%36", "wamid.HBgL%3D%3D", ""] {
                let path = template
                    .split('/')
                    .map(|part| if part.starts_with('{') { value } else { part })
                    .collect::<Vec<_>>()
                    .join("/");
                assert_eq!(
                    operations.of(method, &path),
                    Some(operation),
                    "{method} {path}"
                );
                if *method == "GET" {
                    assert_eq!(operations.of("HEAD", &path), Some(operation), "HEAD {path}");
                }
            }
        }
        assert_eq!(
            served.len(),
            Operation::ALL.len(),
            "every core operation is served"
        );
        assert_eq!(operations.routes().len(), Operation::ALL.len());
        // Nothing else resolves: a method the route does not take, an
        // admin route, a lower-case method, a segment too many.
        assert_eq!(operations.of("POST", "/v1/numbers"), None);
        assert_eq!(operations.of("GET", "/v1/admin/tenants"), None);
        assert_eq!(operations.of("get", "/v1/numbers"), None);
        assert_eq!(operations.of("GET", "/v1/numbers/x/y"), None);
        assert_eq!(operations.of("GET", "/v1/numbers/1/profile/"), None);
        // A literal segment beats a parameter, as axum routes it (roadmap
        // M2b): the stream is not an event of id `stream`.
        assert!(literal_beats_parameter(
            "/v1/events/stream",
            "/v1/events/{id}"
        ));
        assert!(!literal_beats_parameter("/v1/{a}/x", "/v1/{b}/y"));
        assert_eq!(
            operations.of("GET", "/v1/events/stream"),
            Some(Operation::StreamEvents)
        );
        assert_eq!(
            operations.of("GET", "/v1/events/evt_4042233518184f0e9822bebdbba028a8"),
            Some(Operation::GetEvent)
        );
    }
}
