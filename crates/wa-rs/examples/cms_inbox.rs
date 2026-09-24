//! The CMS inbox: every merchant chats with their own customers from your
//! app, on the number they connected with Embedded Signup.
//!
//! ```text
//! Meta ─ POST /webhook ─► WebhookHandler ─► FanoutSink ─┬─► InboxSink ─► ConversationStore
//!        (signature, dedup)                             └─► BroadcastSink ─► GET /inbox/{id}/events (SSE)
//! merchant UI ─ POST /inbox/{id}/reply ─► Inbox::reply ─► Meta, with the merchant's token
//!                                                       └► ConversationStore (outbound, `accepted`)
//! ```
//!
//! | Route | What |
//! | --- | --- |
//! | `GET /webhook`, `POST /webhook` | Meta's callback URL: the subscription check, then deliveries |
//! | `GET /inbox/{phone_number_id}/events` | live events of that number, as Server-Sent Events |
//! | `GET /inbox/{phone_number_id}/conversations` | conversations, most recent first |
//! | `GET /inbox/{phone_number_id}/conversations/{contact}` | one conversation's messages, newest first, and when its reply window closes; marks it read |
//! | `POST /inbox/{phone_number_id}/reply` | `{"contact": "<BSUID or wa_id>", "text": "…"}`: send inside the 24-hour window, and record |
//!
//! **The `/inbox` routes are unauthenticated in this example.** In your CMS
//! they must sit behind your own authentication, and every one of them must
//! check that the signed-in merchant owns `{phone_number_id}`; otherwise
//! anyone can read and answer any merchant's customers. The `/webhook`
//! routes authenticate Meta by signature and verify token and stay public.
//!
//! The merchant's business token is looked up per request from the
//! [`TokenVault`] by phone number: the `embedded_signup` example puts it
//! there. Run both against the same `DATABASE_URL` and `WA_VAULT_KEY` to go
//! from onboarding to chatting; or, to try this one alone, set
//! `WA_WABA_ID`, `WA_PHONE_NUMBER_ID` and `WA_TOKEN` to your own test
//! number and a system user token, and it is put in the vault at startup.
//!
//! | Variable | Required | What |
//! | --- | --- | --- |
//! | `WA_APP_SECRET` | yes | your Meta app's secret: verifies `X-Hub-Signature-256` |
//! | `WA_VERIFY_TOKEN` | yes | the verify token you typed into the App Dashboard's webhook settings |
//! | `WA_VAULT_KEY` | with `DATABASE_URL` | base64 of 32 random bytes (`openssl rand -base64 32`); without a database a throwaway key is generated |
//! | `WA_VAULT_KEY_ID` | no | id recorded with each encrypted token (default `k1`) |
//! | `DATABASE_URL` | no | Postgres (build with `--features postgres`); memory stores otherwise |
//! | `WA_WABA_ID`, `WA_PHONE_NUMBER_ID`, `WA_TOKEN` | no | one merchant to put in the vault at startup |
//! | `PORT` | no | default `3000` |
//!
//! ```text
//! WA_APP_SECRET=… WA_VERIFY_TOKEN=… cargo run -p wa-rs --example cms_inbox --features axum
//! ```
//!
//! Meta only calls HTTPS callback URLs: during development put a tunnel in
//! front of the port and set `https://<tunnel>/webhook` as the callback URL.

use std::sync::Arc;

use anyhow::Context as _;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use wa_rs::adapters::sink::{BroadcastSink, FanoutSink};
use wa_rs::adapters::store::{MemoryConversationStore, MemoryKvStore};
use wa_rs::client::embedded_signup::{StoredBusinessToken, TokenVault, VaultKey, VaultKeys};
use wa_rs::client::messages::Text;
use wa_rs::core::store::{ConversationSummary, StoredMessage};
use wa_rs::prelude::*;

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
}

/// The whole HTTP app: Meta's webhook endpoint and the inbox routes.
///
/// `kv` backs webhook deduplication; `conversations` is where
/// [`InboxSink`] records and where the inbox reads.
pub fn app(
    client: Client,
    app_secret: AppSecret,
    verify_token: VerifyToken,
    kv: Arc<dyn KvStore>,
    conversations: Arc<dyn ConversationStore>,
    vault: TokenVault,
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

    let state = AppState {
        client,
        vault,
        conversations,
        live,
    };
    // Put your authentication in front of these (see the header).
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
        .with_state(state);
    Ok(inbox_routes.nest("/webhook", webhook))
}

/// `GET /inbox/{phone_number_id}/events`
async fn events(
    State(state): State<AppState>,
    Path(phone_number_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let number = inbox(&state, phone_number_id)
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
    Path(phone_number_id): Path<String>,
) -> Result<Json<Vec<ConversationSummary>>, ApiError> {
    let inbox = inbox(&state, phone_number_id).await?;
    Ok(Json(inbox.conversations(None, PAGE).await?))
}

/// `GET /inbox/{phone_number_id}/conversations/{contact}`
async fn conversation(
    State(state): State<AppState>,
    Path((phone_number_id, contact)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let inbox = inbox(&state, phone_number_id).await?;
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
    Path(phone_number_id): Path<String>,
    Json(body): Json<ReplyRequest>,
) -> Result<Json<Value>, ApiError> {
    let inbox = inbox(&state, phone_number_id).await?;
    let key = inbox.key(body.contact);
    // Refused locally, before any request, outside the 24-hour window.
    let sent = inbox.reply(&key, Text::new(body.text).into()).await?;
    Ok(Json(json!({"message_id": sent.message_id()})))
}

/// The inbox of `phone_number_id`, replying as the merchant who connected
/// it. Your CMS checks here that the signed-in merchant owns the number.
async fn inbox(state: &AppState, phone_number_id: String) -> Result<Inbox, ApiError> {
    let number = PhoneNumberId::new(phone_number_id);
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

/// How the inbox routes fail. Answers carry a stable code, never a token or
/// Meta's error text (that goes to the server log).
enum ApiError {
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
    let (kv, conversations) = stores().await?;
    let vault = TokenVault::new(kv.clone(), VaultKeys::new(vault_key()?))?;
    connect_merchant_from_env(&vault).await?;
    // No default token: replies run as the merchant (see `inbox`).
    let client = wa_rs::client_builder()?.build()?;

    let app = app(client, app_secret, verify_token, kv, conversations, vault)?;
    let port: u16 = std::env::var("PORT").map_or(Ok(3000), |p| p.parse())?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    println!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

/// Postgres when `DATABASE_URL` is set, memory otherwise.
async fn stores() -> anyhow::Result<(Arc<dyn KvStore>, Arc<dyn ConversationStore>)> {
    match std::env::var("DATABASE_URL") {
        #[cfg(feature = "postgres")]
        Ok(url) => {
            use wa_rs::adapters::store::{PostgresConversationStore, PostgresKvStore, postgres};
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
/// verified with Meta; here you vouch for them yourself.
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
