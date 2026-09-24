//! The CMS inbox: every merchant chats with their own customers from your
//! app, on the number they connected with Embedded Signup.
//!
//! ```text
//! Meta ─ POST /webhook ─► WebhookHandler ─► FanoutSink ─┬─► InboxSink ─► ConversationStore
//!        (signature, dedup)                             └─► BroadcastSink ─► GET /inbox/{id}/events (SSE)
//! merchant UI ─ POST /inbox/{id}/reply ─► Inbox::reply ─► Meta, with the merchant's token
//!   (bearer → tenant → owns {id}?)                      └► ConversationStore (outbound, `accepted`)
//! ```
//!
//! | Route | Who may call | What |
//! | --- | --- | --- |
//! | `GET /webhook`, `POST /webhook` | Meta (verify token, `X-Hub-Signature-256`) | Meta's callback URL: the subscription check, then deliveries |
//! | `GET /inbox/{phone_number_id}/events` | the tenant owning the number | live events of that number, as Server-Sent Events |
//! | `GET /inbox/{phone_number_id}/conversations` | the tenant owning the number | conversations, most recent first |
//! | `GET /inbox/{phone_number_id}/conversations/{contact}` | the tenant owning the number | one conversation's messages, newest first, and when its reply window closes; marks it read |
//! | `POST /inbox/{phone_number_id}/reply` | the tenant owning the number | `{"contact": "<BSUID or wa_id>", "text": "…"}`: send inside the 24-hour window, and record |
//!
//! # Who may use the inbox
//!
//! Every `/inbox` request needs `Authorization: Bearer <token>`; the token
//! names a tenant (a merchant of your CMS), and a tenant reaches only the
//! phone numbers listed for it. Anything else is refused before the
//! merchant's business token is even looked up: no token or a wrong one is
//! `401`, another tenant's number is `403`. Without it, anyone who can reach
//! the port reads and answers every merchant's customers, as that merchant.
//!
//! [`Tenants`], configured from `WA_TENANTS`, **stands in for two things
//! your CMS already has**: its session handling (who is calling) and its
//! tenant → phone number table (fill it from `Onboarded::phone_number_ids`
//! when Embedded Signup finishes). Replace it with those; keep the ownership
//! check in [`inbox`] exactly where it is. The example refuses to start
//! without `WA_TENANTS`, and listens on `127.0.0.1` unless `WA_BIND` says
//! otherwise: put your TLS-terminating proxy in front before binding
//! anything public.
//!
//! A browser's `EventSource` cannot send an `Authorization` header: with a
//! browser UI, authenticate the events route with your cookie session, or
//! read it with a `fetch`-based SSE client.
//!
//! # The merchant's business token
//!
//! Looked up per request from the [`TokenVault`] by phone number: the
//! `embedded_signup` example puts it there. Run both against the same
//! `DATABASE_URL`, `WA_VAULT_KEY` and `WA_TENANTS` to go from onboarding to
//! chatting (add the `phone_number_ids` onboarding returns to the tenant's
//! entry); or, to try this one alone, set `WA_WABA_ID`, `WA_PHONE_NUMBER_ID`
//! and `WA_TOKEN` to your own test number and a system user token, and it is
//! put in the vault at startup.
//!
//! | Variable | Required | What |
//! | --- | --- | --- |
//! | `WA_APP_SECRET` | yes | your Meta app's secret: verifies `X-Hub-Signature-256` |
//! | `WA_VERIFY_TOKEN` | yes | the verify token you typed into the App Dashboard's webhook settings |
//! | `WA_TENANTS` | yes | stand-in for your auth: `{"<tenant>": {"token": "<bearer, 32+ chars>", "phone_number_ids": ["<id>", …]}}` |
//! | `WA_VAULT_KEY` | with `DATABASE_URL` | base64 of 32 random bytes (`openssl rand -base64 32`); without a database a throwaway key is generated |
//! | `WA_VAULT_KEY_ID` | no | id recorded with each encrypted token (default `k1`) |
//! | `DATABASE_URL` | no | Postgres (build with `--features postgres`); memory stores otherwise |
//! | `WA_WABA_ID`, `WA_PHONE_NUMBER_ID`, `WA_TOKEN` | no | one merchant to put in the vault at startup |
//! | `WA_BIND` | no | address to listen on (default `127.0.0.1`) |
//! | `PORT` | no | default `3000` |
//!
//! ```text
//! TOKEN=$(openssl rand -hex 32)   # the demo tenant's bearer token
//! WA_TENANTS='{"demo-merchant": {"token": "'"$TOKEN"'", "phone_number_ids": ["<phone number id>"]}}' \
//!   WA_APP_SECRET=… WA_VERIFY_TOKEN=… cargo run -p wa-rs --example cms_inbox --features axum
//! curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:3000/inbox/<phone number id>/conversations
//! ```
//!
//! Meta only calls HTTPS callback URLs: during development put a tunnel in
//! front of the port and set `https://<tunnel>/webhook` as the callback URL.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use tokio::sync::broadcast;
use wa_rs::adapters::sink::{BroadcastSink, FanoutSink};
use wa_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
use wa_rs::client::embedded_signup::{StoredBusinessToken, TokenVault, VaultKey, VaultKeys};
use wa_rs::client::messages::Text;
use wa_rs::core::store::{ConversationSummary, StoredMessage};
use wa_rs::prelude::*;
// The axum the webhook router is built with, re-exported: no pin of your own.
use wa_rs::webhooks::axum::extract::{Path, Request, State};
use wa_rs::webhooks::axum::http::{HeaderValue, StatusCode, header};
use wa_rs::webhooks::axum::middleware::{self, Next};
use wa_rs::webhooks::axum::response::{IntoResponse, Response};
use wa_rs::webhooks::axum::routing::{get, post};
use wa_rs::webhooks::axum::{self, Extension, Json, Router};

/// Page size of the list routes. Pass the last row's cursor to
/// `Inbox::conversations` / `Inbox::history` for the next page.
const PAGE: usize = 50;

/// What the routes share. Cheap to clone.
#[derive(Clone)]
struct AppState {
    /// No default token: every reply runs as the merchant who owns the
    /// number, with the token from `vault`.
    client: Client,
    vault: TokenVault,
    conversations: Arc<dyn ConversationStore>,
    /// Every webhook event, for the SSE route to filter.
    live: broadcast::Sender<WebhookEvent>,
    tenants: Arc<Tenants>,
}

/// The whole HTTP app: Meta's webhook endpoint and the inbox routes.
///
/// `kv` backs webhook deduplication; `conversations` is where
/// [`InboxSink`] records and where the inbox reads; `tenants` says who may
/// read and answer which number.
pub fn app(
    client: Client,
    app_secret: AppSecret,
    verify_token: VerifyToken,
    kv: Arc<dyn KvStore>,
    conversations: Arc<dyn ConversationStore>,
    vault: TokenVault,
    tenants: Tenants,
) -> wa_rs::Result<Router> {
    // Every event goes to the store (history per conversation) and to
    // `live`, which each SSE client subscribes to; a client that falls 256
    // events behind gets an `event: lagged` and should reload. The two sinks
    // run concurrently, so a live event can reach the UI before the store
    // has it: render the event itself (it carries the whole message).
    let (live, _) = broadcast::channel(256);
    let sink = FanoutSink::new()
        .with(InboxSink::new(conversations.clone()))
        .with(BroadcastSink::from_sender(live.clone()));
    let handler = WebhookHandler::builder(
        SignatureVerifier::new(vec![app_secret])?, // X-Hub-Signature-256
        verify_token,
        Arc::new(sink),
    )
    .dedup(DedupGuard::new(kv)) // Meta retries for 7 days: record each event once
    .build();
    let webhook = wa_rs::webhooks::router(Arc::new(handler)); // GET verify, POST deliver

    let tenants = Arc::new(tenants);
    let state = AppState {
        client,
        vault,
        conversations,
        live,
        tenants: tenants.clone(),
    };
    let inbox_routes = Router::new()
        .route("/inbox/{phone_number_id}/events", get(events))
        .route(
            "/inbox/{phone_number_id}/conversations",
            get(conversations_page),
        )
        .route(
            "/inbox/{phone_number_id}/conversations/{contact}",
            get(conversation),
        )
        .route("/inbox/{phone_number_id}/reply", post(reply))
        // Every route above: no tenant, no answer.
        .route_layer(middleware::from_fn_with_state(tenants, authenticate))
        .with_state(state);
    // Meta authenticates by signature and verify token: stays public.
    Ok(inbox_routes.nest("/webhook", webhook))
}

/// `GET /inbox/{phone_number_id}/events`
async fn events(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Path(phone_number_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let number = inbox(&state, &tenant, phone_number_id)
        .await?
        .phone_number_id()
        .clone();
    // An allow-list: only events that name this number. Events without one
    // (bodies that did not parse, unknown fields) may belong to any merchant.
    let only_this_number = move |event: &WebhookEvent| event.phone_number_id() == Some(&number);
    Ok(wa_rs::webhooks::sse(
        state.live.subscribe(),
        only_this_number,
    ))
}

/// `GET /inbox/{phone_number_id}/conversations`
async fn conversations_page(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Path(phone_number_id): Path<String>,
) -> Result<Json<Vec<ConversationSummary>>, ApiError> {
    let inbox = inbox(&state, &tenant, phone_number_id).await?;
    Ok(Json(inbox.conversations(None, PAGE).await?))
}

/// `GET /inbox/{phone_number_id}/conversations/{contact}`
async fn conversation(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Path((phone_number_id, contact)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let inbox = inbox(&state, &tenant, phone_number_id).await?;
    let key = inbox.key(contact);
    let messages: Vec<StoredMessage> = inbox.history(&key, None, PAGE).await?;
    // After it closes, only templates reach the customer: let the UI switch.
    let window_closes_at = inbox
        .window(&key)
        .await?
        .closes_at()
        .map(OffsetDateTime::unix_timestamp);
    inbox.mark_read(&key).await?;
    Ok(Json(
        json!({"messages": messages, "window_closes_at": window_closes_at}),
    ))
}

/// Body of `POST /inbox/{phone_number_id}/reply`.
#[derive(Deserialize)]
struct ReplyRequest {
    /// The conversation's contact: a BSUID (`US.…`), or a `wa_id`.
    contact: String,
    text: String,
}

/// `POST /inbox/{phone_number_id}/reply`
async fn reply(
    State(state): State<AppState>,
    Extension(tenant): Extension<Tenant>,
    Path(phone_number_id): Path<String>,
    Json(body): Json<ReplyRequest>,
) -> Result<Json<Value>, ApiError> {
    let inbox = inbox(&state, &tenant, phone_number_id).await?;
    let key = inbox.key(body.contact);
    // Refused locally, before any request, outside the 24-hour window.
    let sent = inbox.reply(&key, Text::new(body.text).into()).await?;
    Ok(Json(json!({"message_id": sent.message_id()})))
}

/// The inbox of `phone_number_id`, replying as the merchant who connected
/// it — if the calling tenant owns that number.
async fn inbox(
    state: &AppState,
    tenant: &Tenant,
    phone_number_id: String,
) -> Result<Inbox, ApiError> {
    let number = PhoneNumberId::new(phone_number_id);
    // Your own table says which numbers this tenant owns; ask it before
    // touching the vault, whose tokens belong to every merchant.
    if !state.tenants.owns(tenant, &number) {
        return Err(ApiError::Forbidden);
    }
    // The business token Embedded Signup stored for this number's WABA.
    let Some(merchant) = state.vault.get_by_phone_number(&number).await? else {
        return Err(ApiError::NotConnected);
    };
    if merchant.is_expired(OffsetDateTime::now_utc()) {
        return Err(ApiError::Reconnect);
    }
    let client = state.client.with_token(merchant.token);
    Ok(Inbox::new(client, number, state.conversations.clone()))
}

/// A tenant (merchant) of your CMS, as authenticated for this request.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Tenant(pub String);

/// Stand-in for your authentication and your tenant → phone number table.
///
/// Each tenant has a bearer token and the phone number ids it may use. Only
/// SHA-256 digests of the tokens are kept, and a presented token is compared
/// with every one of them in constant time. Build it in code with
/// [`Tenants::tenant`], or from `WA_TENANTS` with [`Tenants::from_json`].
#[derive(Default)]
pub struct Tenants {
    /// (SHA-256 of the bearer token, whose it is)
    tokens: Vec<([u8; 32], Tenant)>,
    numbers: HashMap<Tenant, HashSet<PhoneNumberId>>,
}

/// Shortest bearer token accepted: 32 characters (`openssl rand -hex 32`
/// gives 64).
const MIN_TOKEN_LEN: usize = 32;

impl Tenants {
    /// Add `tenant`, authenticated by `token` and owning `phone_number_ids`.
    pub fn tenant<I>(
        mut self,
        tenant: &str,
        token: &str,
        phone_number_ids: I,
    ) -> anyhow::Result<Self>
    where
        I: IntoIterator,
        I::Item: Into<PhoneNumberId>,
    {
        anyhow::ensure!(!tenant.is_empty(), "a tenant id is empty");
        anyhow::ensure!(
            token.len() >= MIN_TOKEN_LEN,
            "tenant {tenant}: the bearer token must be at least {MIN_TOKEN_LEN} characters \
             (`openssl rand -hex 32`)"
        );
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        anyhow::ensure!(
            !self.tokens.iter().any(|(known, _)| *known == digest),
            "tenant {tenant}: another tenant has the same bearer token"
        );
        let tenant = Tenant(tenant.to_owned());
        self.tokens.push((digest, tenant.clone()));
        self.numbers
            .entry(tenant)
            .or_default()
            .extend(phone_number_ids.into_iter().map(Into::into));
        Ok(self)
    }

    /// From JSON: `{"<tenant>": {"token": "…", "phone_number_ids": ["…"]}}`.
    /// Refuses an empty map. Errors never quote the input (it holds tokens).
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        #[derive(Deserialize)]
        struct Entry {
            token: String,
            #[serde(default)]
            phone_number_ids: Vec<String>,
        }
        let entries: HashMap<String, Entry> = serde_json::from_str(json).map_err(|e| {
            anyhow::anyhow!(
                "WA_TENANTS: expected {{\"<tenant>\": {{\"token\": …, \"phone_number_ids\": […]}}}} \
                 (line {}, column {})",
                e.line(),
                e.column()
            )
        })?;
        anyhow::ensure!(!entries.is_empty(), "WA_TENANTS lists no tenant");
        entries
            .into_iter()
            .try_fold(Self::default(), |tenants, (tenant, entry)| {
                tenants.tenant(&tenant, &entry.token, entry.phone_number_ids)
            })
    }

    /// The tenant whose token `authorization` (`Bearer <token>`) carries.
    fn authenticate(&self, authorization: Option<&HeaderValue>) -> Option<Tenant> {
        let value = authorization?.to_str().ok()?;
        let (scheme, token) = value.split_once(' ')?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return None;
        }
        let presented: [u8; 32] = Sha256::digest(token.trim_start().as_bytes()).into();
        // No early return: every entry is compared, each in constant time,
        // so a guess learns nothing from how long the answer took.
        let mut found = None;
        for (digest, tenant) in &self.tokens {
            if bool::from(digest.ct_eq(&presented)) {
                found = Some(tenant.clone());
            }
        }
        found
    }

    /// Whether `tenant` may read and answer `number`.
    fn owns(&self, tenant: &Tenant, number: &PhoneNumberId) -> bool {
        self.numbers
            .get(tenant)
            .is_some_and(|numbers| numbers.contains(number))
    }
}

/// Middleware in front of the inbox routes: resolve the tenant, or `401`.
async fn authenticate(
    State(tenants): State<Arc<Tenants>>,
    mut request: Request,
    next: Next,
) -> Response {
    match tenants.authenticate(request.headers().get(header::AUTHORIZATION)) {
        Some(tenant) => {
            request.extensions_mut().insert(tenant);
            next.run(request).await
        }
        None => ApiError::Unauthenticated.into_response(),
    }
}

/// How the inbox routes fail. Answers carry a stable code, never a token or
/// Meta's error text (that goes to the server log).
enum ApiError {
    /// No bearer token, or one no tenant has.
    Unauthenticated,
    /// The calling tenant does not own this number.
    Forbidden,
    /// No merchant connected this number.
    NotConnected,
    /// The merchant's token expired: they go through Embedded Signup again.
    Reconnect,
    /// More than 24 hours since the customer's last message: send a template.
    WindowClosed,
    /// Rejected locally: a field and why.
    Invalid { field: String, reason: String },
    /// Meta, the network or a store failed.
    Upstream(Error),
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        match error {
            // `Inbox::reply` refuses locally with this field; Meta's own
            // refusal (131047) is the `ErrorKind` below.
            Error::Validation(v) if v.field == "customer_service_window" => Self::WindowClosed,
            Error::Validation(v) => Self::Invalid {
                field: v.field,
                reason: v.reason,
            },
            e if e.kind() == ErrorKind::CustomerServiceWindowClosed => Self::WindowClosed,
            e => Self::Upstream(e),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            Self::Unauthenticated => {
                let body = Json(json!({"error": "unauthenticated"}));
                let challenge = [(header::WWW_AUTHENTICATE, "Bearer")];
                return (StatusCode::UNAUTHORIZED, challenge, body).into_response();
            }
            Self::Forbidden => (StatusCode::FORBIDDEN, json!({"error": "forbidden"})),
            Self::NotConnected => (StatusCode::NOT_FOUND, json!({"error": "not_connected"})),
            Self::Reconnect => (StatusCode::CONFLICT, json!({"error": "reconnect_whatsapp"})),
            Self::WindowClosed => (
                StatusCode::CONFLICT,
                json!({"error": "customer_service_window_closed"}),
            ),
            Self::Invalid { field, reason } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"error": "invalid", "field": field, "reason": reason}),
            ),
            Self::Upstream(error) => {
                eprintln!("inbox: {error} (kind {:?})", error.kind());
                (StatusCode::BAD_GATEWAY, json!({"error": "upstream"}))
            }
        };
        (status, Json(body)).into_response()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // RUST_LOG=info (or debug) shows what the library logs.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let app_secret = AppSecret::new(env("WA_APP_SECRET")?);
    let verify_token = VerifyToken::new(env("WA_VERIFY_TOKEN")?);
    // No tenants, no inbox: the routes are never served unauthenticated.
    let tenants = Tenants::from_json(&env("WA_TENANTS")?)?;
    let (kv, conversations) = stores().await?;
    let vault = TokenVault::new(kv.clone(), VaultKeys::new(vault_key()?))?;
    connect_merchant_from_env(&vault).await?;
    // No default token: replies run as the merchant (see `inbox`).
    let client = wa_rs::client_builder()?.build()?;

    let app = app(
        client,
        app_secret,
        verify_token,
        kv,
        conversations,
        vault,
        tenants,
    )?;
    let bind: IpAddr = std::env::var("WA_BIND")
        .map_or(Ok(Ipv4Addr::LOCALHOST.into()), |a| a.parse())
        .context("WA_BIND must be an IP address")?;
    let port: u16 = std::env::var("PORT").map_or(Ok(3000), |p| p.parse())?;
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    println!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

/// Postgres when `DATABASE_URL` is set, memory otherwise.
// Only the Postgres arm awaits; without the feature there is nothing to await.
#[cfg_attr(not(feature = "postgres"), allow(clippy::unused_async))]
async fn stores() -> anyhow::Result<(Arc<dyn KvStore>, Arc<dyn ConversationStore>)> {
    match std::env::var("DATABASE_URL") {
        #[cfg(feature = "postgres")]
        Ok(url) => {
            // sqlx as wa-rs re-exports it: the `PgPool` the stores take.
            use wa_rs::adapters::store::postgres::{self, sqlx};
            use wa_rs::adapters::store::{PostgresConversationStore, PostgresKvStore};
            let pool = sqlx::PgPool::connect(&url)
                .await
                .context("connect to DATABASE_URL")?;
            postgres::migrate(&pool).await?; // idempotent
            Ok((
                Arc::new(PostgresKvStore::new(pool.clone())),
                Arc::new(PostgresConversationStore::new(pool)),
            ))
        }
        #[cfg(not(feature = "postgres"))]
        Ok(_) => anyhow::bail!("DATABASE_URL is set, but this build lacks `--features postgres`"),
        Err(_) => {
            println!("DATABASE_URL is not set: memory stores, emptied on restart");
            Ok((
                Arc::new(MemoryKvStore::new()),
                Arc::new(MemoryConversationStore::new()),
            ))
        }
    }
}

/// The key the vault encrypts business tokens with. Comes from your secret
/// manager in production; rotating it is `VaultKeys::with_previous`.
fn vault_key() -> anyhow::Result<VaultKey> {
    let id = std::env::var("WA_VAULT_KEY_ID").unwrap_or_else(|_| "k1".to_owned());
    if let Ok(encoded) = std::env::var("WA_VAULT_KEY") {
        return Ok(VaultKey::from_base64(id, &encoded)?);
    }
    anyhow::ensure!(
        std::env::var_os("DATABASE_URL").is_none(),
        "set WA_VAULT_KEY: tokens stored under a throwaway key are unreadable after a restart"
    );
    println!("WA_VAULT_KEY is not set: throwaway vault key");
    Ok(VaultKey::generate(id)?)
}

/// Put one merchant in the vault when `WA_WABA_ID`, `WA_PHONE_NUMBER_ID` and
/// `WA_TOKEN` are all set. `embedded_signup` does this for real, with ids it
/// verified with Meta; here you vouch for them yourself. The number still
/// has to be in a tenant's `phone_number_ids` to be served.
async fn connect_merchant_from_env(vault: &TokenVault) -> anyhow::Result<()> {
    let (Ok(waba_id), Ok(phone_number_id), Ok(token)) = (
        std::env::var("WA_WABA_ID"),
        std::env::var("WA_PHONE_NUMBER_ID"),
        std::env::var("WA_TOKEN"),
    ) else {
        return Ok(());
    };
    vault
        .store(
            &StoredBusinessToken::new(waba_id, AccessToken::new(token))
                .phone_number_ids([phone_number_id.clone()]),
        )
        .await?;
    println!("connected {phone_number_id} from the environment");
    Ok(())
}

fn env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("set {name} (see the example's header)"))
}
