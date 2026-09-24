//! Embedded Signup: a merchant connects their own WhatsApp number to your
//! platform, and you end up holding their business token, encrypted.
//!
//! ```text
//! merchant's browser                         this server                          Meta
//! GET /  (page, Facebook JS SDK)
//! GET /signup/start (bearer) ──────────────► SignupSessions::start (state ↔ tenant)
//!     ◄── {state, launch_options} ───────────┘
//! FB.login(callback, launch_options) ──────────────────────────────────────────► popup
//!     ◄── code (30 s, single use) + WA_EMBEDDED_SIGNUP message event ──────────────┘
//! POST /signup/complete (bearer) ──────────► redeem(state, tenant)
//!   {state, code, event, pin?}                 EmbeddedSignup::onboard ──────────► exchange code, debug_token,
//!                                                 └► TokenVault (by WABA, by number)  verify WABA and number,
//!                                                                                    subscribe app, register
//! ```
//!
//! | Route | Who may call | What |
//! | --- | --- | --- |
//! | `GET /` | anyone | a minimal page with the Facebook JavaScript SDK launch code (no secrets in it) |
//! | `GET /signup/start` | a tenant | a new attempt bound to the calling tenant: `{"state", "launch_options"}` |
//! | `POST /signup/complete` | the tenant who started it | `{"state", "code", "event", "pin"?}` from the page: redeem, onboard, store |
//! | `POST /signup/resume` | the tenant whose attempt it was | `{"pin"?}`: after `subscribe_app` or `register_phone` failed, redo just those |
//!
//! # Who is calling
//!
//! The tenant (a merchant of your CMS) is whoever your authentication says
//! is calling — never a value the page or the URL chooses. That is what makes
//! [`SignupSessions::redeem`] bind the code to the right merchant, and what
//! stops one merchant from resuming another's onboarding with their own PIN.
//! Here every `/signup` request needs `Authorization: Bearer <token>`, and
//! [`Tenants`], configured from `WA_TENANTS`, maps tokens to tenants: **a
//! stand-in for your session handling**; replace it, and keep the tenant
//! coming from it. Without `WA_TENANTS` the example does not start; it
//! listens on `127.0.0.1` unless `WA_BIND` says otherwise.
//!
//! # The two-step verification PIN
//!
//! Registering a Cloud API number sets its two-step verification PIN (or
//! must match the one it already has), so the PIN is the **merchant's**: the
//! page asks for it and posts it with the attempt, and nothing here logs or
//! stores it. One PIN shared by every merchant's number would let a single
//! leak take over all of them. Without a PIN the number is left
//! unregistered.
//!
//! The page must be served over HTTPS from a domain listed in the app's
//! **Allowed domains** and **Valid OAuth redirect URIs** (Facebook Login for
//! Business → Settings); use a tunnel in development. The token stored here
//! is what the `cms_inbox` example replies with: run both against the same
//! `DATABASE_URL`, `WA_VAULT_KEY` and `WA_TENANTS`.
//!
//! | Variable | Required | What |
//! | --- | --- | --- |
//! | `WA_APP_ID` | yes | your Meta app id (public: it is written into the page) |
//! | `WA_APP_SECRET` | yes | exchanges the code for the business token |
//! | `WA_ES_CONFIG_ID` | yes | the Facebook Login for Business configuration id (App Dashboard → Facebook Login for Business → Configurations) |
//! | `WA_TENANTS` | yes | stand-in for your auth: `{"<tenant>": {"token": "<bearer, 32+ chars>"}}` (the `cms_inbox` example's value works as is) |
//! | `WA_VAULT_KEY` | with `DATABASE_URL` | base64 of 32 random bytes (`openssl rand -base64 32`); without a database a throwaway key is generated |
//! | `WA_VAULT_KEY_ID` | no | id recorded with each encrypted token (default `k1`) |
//! | `DATABASE_URL` | no | Postgres (build with `--features postgres`); memory store otherwise |
//! | `WA_BIND` | no | address to listen on (default `127.0.0.1`) |
//! | `PORT` | no | default `3000` |
//!
//! ```text
//! TOKEN=$(openssl rand -hex 32)   # the demo tenant's bearer token: paste it into the page
//! WA_TENANTS='{"demo-merchant": {"token": "'"$TOKEN"'"}}' \
//!   WA_APP_ID=… WA_APP_SECRET=… WA_ES_CONFIG_ID=… \
//!   cargo run -p wa-rs --example embedded_signup --features axum
//! ```

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use wa_rs::adapters::store::MemoryKvStore;
use wa_rs::client::embedded_signup::{
    CurrentStep, EmbeddedSignup, EmbeddedSignupEvent, FinishKind, LaunchOptions, OnboardingRequest,
    SignupCode, SignupSessions, SignupState, TokenVault, VaultKey, VaultKeys, steps,
};
use wa_rs::client::phone_numbers::TwoStepPin;
use wa_rs::core::config::ApiVersion;
use wa_rs::prelude::*;
// The axum the webhook router is built with, re-exported: no pin of your own.
use wa_rs::webhooks::axum::extract::rejection::JsonRejection;
use wa_rs::webhooks::axum::extract::{DefaultBodyLimit, Request, State};
use wa_rs::webhooks::axum::http::{HeaderValue, StatusCode, header};
use wa_rs::webhooks::axum::middleware::{self, Next};
use wa_rs::webhooks::axum::response::{Html, IntoResponse, Response};
use wa_rs::webhooks::axum::routing::{get, post};
use wa_rs::webhooks::axum::{self, Extension, Json, Router};

/// How long a merchant has to go through the flow: several screens, maybe
/// an SMS code.
const ATTEMPT_TTL: Duration = Duration::from_mins(15);

/// Everything the routes share. Cheap to clone.
#[derive(Clone)]
pub struct Signup {
    /// `client.embedded_signup(AppCredentials::new(app_id, app_secret))`.
    pub onboarding: EmbeddedSignup,
    /// On the same `KvStore` as the vault.
    pub sessions: SignupSessions,
    /// Where business tokens end up.
    pub vault: TokenVault,
    /// Facebook Login for Business configuration id.
    pub config_id: String,
    /// Who is calling: the stand-in for your authentication.
    pub tenants: Arc<Tenants>,
    /// Your tenant → WABA table. Onboarding verifies the WABA with Meta, but
    /// only you know which of *your* merchants may own it.
    pub merchants: Arc<Mutex<HashMap<String, WabaId>>>,
    /// Attempts whose token is stored but whose last steps failed, for
    /// `/signup/resume`, by tenant.
    unfinished: Arc<Mutex<HashMap<String, (WabaId, OnboardingRequest)>>>,
}

impl Signup {
    /// State for [`app`].
    pub fn new(
        onboarding: EmbeddedSignup,
        kv: Arc<dyn KvStore>,
        vault: TokenVault,
        config_id: impl Into<String>,
        tenants: Tenants,
    ) -> Self {
        Self {
            onboarding,
            sessions: SignupSessions::new(kv),
            vault,
            config_id: config_id.into(),
            tenants: Arc::new(tenants),
            merchants: Arc::default(),
            unfinished: Arc::default(),
        }
    }
}

/// The routes. `signup.onboarding.app()` supplies the app id written
/// into the page.
pub fn app(signup: Signup) -> Router {
    let authenticated = Router::new()
        .route("/signup/start", get(start))
        // The event comes from the browser: small, and not trusted.
        .route(
            "/signup/complete",
            post(complete).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/signup/resume", post(resume))
        // Every route above: no tenant, no answer.
        .route_layer(middleware::from_fn_with_state(
            signup.tenants.clone(),
            authenticate,
        ));
    Router::new()
        .route("/", get(page))
        .merge(authenticated)
        .with_state(signup)
}

/// `GET /signup/start`: bind a new attempt to the calling merchant and hand
/// the page what `FB.login` needs.
async fn start(
    State(signup): State<Signup>,
    Extension(Tenant(tenant)): Extension<Tenant>,
) -> Result<Json<Value>, ApiError> {
    let state = signup.sessions.start(&tenant, ATTEMPT_TTL).await?;
    let launch_options = LaunchOptions::new(signup.config_id.as_str()).to_json()?;
    Ok(Json(
        json!({"state": state.as_str(), "launch_options": launch_options}),
    ))
}

/// Body of `POST /signup/complete`: what the page collected. No `Debug`:
/// it holds the code and the PIN.
#[derive(Deserialize)]
struct Completion {
    state: String,
    /// From the `FB.login` callback.
    code: String,
    /// The `WA_EMBEDDED_SIGNUP` message event, as received.
    event: Value,
    /// The number's two-step verification PIN, typed by the merchant: it
    /// becomes the PIN of a new number, or must match the one it has.
    pin: Option<String>,
}

/// `POST /signup/complete`
async fn complete(
    State(signup): State<Signup>,
    Extension(Tenant(tenant)): Extension<Tenant>,
    body: Result<Json<Completion>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = body.map_err(invalid_body)?;
    // Local checks first: a malformed request must not burn the attempt.
    let state = SignupState::parse(&body.state)?;
    let code = SignupCode::new(body.code)?;
    let pin = body.pin.map(TwoStepPin::new).transpose()?;
    let event = EmbeddedSignupEvent::from_value(body.event)?;
    if let EmbeddedSignupEvent::Cancel(cancel) = &event {
        let step = cancel.current_step.as_ref().map(CurrentStep::as_str);
        return Ok(Json(json!({"status": "cancelled", "current_step": step})));
    }
    let mut request = OnboardingRequest::from_event(code, &event)?; // FINISH* only
    // Only the Cloud API flow has a number to register: coexistence numbers
    // are registered already, and FINISH_ONLY_WABA has none.
    if let (Some(FinishKind::Finish), Some(pin)) = (event.finish_kind(), pin) {
        request = request.register_with_pin(pin);
    }

    // Exactly once, and only for the merchant who started the attempt.
    if !signup.sessions.redeem(&state, &tenant).await? {
        return Err(ApiError::StaleAttempt);
    }

    // The code is spent from here on: never retry `onboard` itself (a failed
    // attempt starts again from `/signup/start`).
    let onboarded = signup.onboarding.onboard(&request, &signup.vault).await;
    match onboarded {
        Ok(onboarded) => {
            lock(&signup.merchants).insert(tenant, onboarded.waba_id.clone());
            Ok(Json(json!({
                "status": "connected",
                "waba_id": onboarded.waba_id,
                "phone_number_ids": onboarded.phone_number_ids,
                "steps_completed": onboarded.steps_completed,
                // Coexistence: start the smb_app_data syncs within 24 hours.
                "needs_coexistence_sync": onboarded.needs_coexistence_sync(),
            })))
        }
        Err(error) => {
            // The token is already verified and stored when only the last
            // two steps failed: keep the request so `/signup/resume` can
            // finish once the cause is fixed.
            if resumable(&error)
                && let Some(waba_id) = request.session.primary_waba_id().cloned()
            {
                lock(&signup.merchants).insert(tenant.clone(), waba_id.clone());
                lock(&signup.unfinished).insert(tenant, (waba_id, request));
            }
            Err(error.into())
        }
    }
}

/// Body of `POST /signup/resume`. No `Debug`: it holds the PIN.
#[derive(Deserialize)]
struct Resume {
    /// A corrected two-step verification PIN, when `register_phone` failed
    /// on the PIN.
    pin: Option<String>,
}

/// `POST /signup/resume`: redo `subscribe_app` and `register_phone` with
/// the token onboarding stored, for the calling merchant's own attempt.
async fn resume(
    State(signup): State<Signup>,
    Extension(Tenant(tenant)): Extension<Tenant>,
    body: Result<Json<Resume>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = body.map_err(invalid_body)?;
    let pin = body.pin.map(TwoStepPin::new).transpose()?; // before taking the entry
    // `resume` acts with the stored token of the WABA it is given: this
    // table, keyed by the authenticated tenant, is what ties that WABA to
    // the caller.
    let Some((waba_id, mut request)) = lock(&signup.unfinished).remove(&tenant) else {
        return Err(ApiError::NothingToResume);
    };
    if let Some(pin) = pin {
        request = request.register_with_pin(pin);
    }
    let resumed = signup
        .onboarding
        .resume(&waba_id, &request, &signup.vault)
        .await;
    match resumed {
        Ok(done) => Ok(Json(json!({
            "status": "connected",
            "waba_id": done.waba_id,
            "steps_completed": done.steps_completed,
        }))),
        Err(error) => {
            // Still unfinished: keep it for another try.
            lock(&signup.unfinished).insert(tenant, (waba_id, request));
            Err(error.into())
        }
    }
}

/// Whether `error` left the token stored, so `resume` can finish the job.
fn resumable(error: &Error) -> bool {
    matches!(
        error,
        Error::Step {
            step: steps::SUBSCRIBE_APP | steps::REGISTER_PHONE,
            ..
        }
    )
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A tenant (merchant) of your CMS, as authenticated for this request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tenant(pub String);

/// Stand-in for your authentication: bearer token → tenant.
///
/// Only SHA-256 digests of the tokens are kept, and a presented token is
/// compared with every one of them in constant time. Build it in code with
/// [`Tenants::tenant`], or from `WA_TENANTS` with [`Tenants::from_json`].
#[derive(Default)]
pub struct Tenants {
    /// (SHA-256 of the bearer token, whose it is)
    tokens: Vec<([u8; 32], Tenant)>,
}

/// Shortest bearer token accepted: 32 characters (`openssl rand -hex 32`
/// gives 64).
const MIN_TOKEN_LEN: usize = 32;

impl Tenants {
    /// Add `tenant`, authenticated by `token`.
    pub fn tenant(mut self, tenant: &str, token: &str) -> anyhow::Result<Self> {
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
        self.tokens.push((digest, Tenant(tenant.to_owned())));
        Ok(self)
    }

    /// From JSON: `{"<tenant>": {"token": "…"}}` (other fields, such as the
    /// `cms_inbox` example's `phone_number_ids`, are ignored). Refuses an
    /// empty map. Errors never quote the input (it holds tokens).
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        #[derive(Deserialize)]
        struct Entry {
            token: String,
        }
        let entries: HashMap<String, Entry> = serde_json::from_str(json).map_err(|e| {
            anyhow::anyhow!(
                "WA_TENANTS: expected {{\"<tenant>\": {{\"token\": …}}}} (line {}, column {})",
                e.line(),
                e.column()
            )
        })?;
        anyhow::ensure!(!entries.is_empty(), "WA_TENANTS lists no tenant");
        entries
            .into_iter()
            .try_fold(Self::default(), |tenants, (tenant, entry)| {
                tenants.tenant(&tenant, &entry.token)
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
}

/// Middleware in front of the `/signup` routes: resolve the tenant, or `401`.
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

/// How the signup routes fail: a stable code, never a token, a code or a
/// PIN.
enum ApiError {
    /// No bearer token, or one no tenant has.
    Unauthenticated,
    /// The state expired, was already used, or is another merchant's.
    StaleAttempt,
    NothingToResume,
    /// Rejected locally: a field and why.
    Invalid {
        field: String,
        reason: String,
    },
    /// Onboarding stopped at `step`.
    Step {
        step: &'static str,
        resumable: bool,
    },
    Internal(Error),
}

/// A body that isn't the expected JSON. axum's own rejection quotes the
/// offending value (e.g. "invalid type: integer 581063") — a PIN or a code —
/// so answer with a fixed message instead.
#[allow(clippy::needless_pass_by_value)] // map_err hands the rejection over by value
fn invalid_body(_rejection: JsonRejection) -> ApiError {
    ApiError::Invalid {
        field: "body".to_owned(),
        reason: "expected a JSON object with the documented fields".to_owned(),
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        let resumable = resumable(&error);
        match error {
            Error::Validation(v) => Self::Invalid {
                field: v.field,
                reason: v.reason,
            },
            Error::Step { step, source } => {
                // Meta's error text is for your logs, not the browser.
                eprintln!("embedded signup: step {step} failed: {source}");
                Self::Step { step, resumable }
            }
            e => Self::Internal(e),
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
            Self::StaleAttempt => (StatusCode::FORBIDDEN, json!({"error": "stale_attempt"})),
            Self::NothingToResume => (StatusCode::NOT_FOUND, json!({"error": "nothing_to_resume"})),
            Self::Invalid { field, reason } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"error": "invalid", "field": field, "reason": reason}),
            ),
            Self::Step { step, resumable } => (
                StatusCode::BAD_GATEWAY,
                json!({"error": "onboarding_failed", "step": step, "resumable": resumable}),
            ),
            Self::Internal(error) => {
                eprintln!("embedded signup: {error} (kind {:?})", error.kind());
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"error": "internal"}),
                )
            }
        };
        (status, Json(body)).into_response()
    }
}

/// `GET /`: the launch page, with the app id and Graph API version filled
/// in.
async fn page(State(signup): State<Signup>) -> Html<String> {
    Html(
        PAGE.replace("__APP_ID__", signup.onboarding.app().app_id.as_str())
            .replace("__GRAPH_API_VERSION__", &ApiVersion::DEFAULT.to_string()),
    )
}

/// The Facebook JavaScript SDK launch code from Meta's
/// `embedded-signup/implementation` page, wired to this server. The two
/// placeholders are filled in by [`page`]; the configuration id arrives in
/// `launch_options`.
const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>Connect WhatsApp</title>
<script async defer crossorigin="anonymous" src="https://connect.facebook.net/en_US/sdk.js"></script>
<script>
  window.fbAsyncInit = () => FB.init({
    appId: '__APP_ID__',
    autoLogAppEvents: true,
    xfbml: true,
    version: '__GRAPH_API_VERSION__',
  });

  let attempt = null; // {state, code, event}, posted once all three are here
  let launchOptions = null;

  // Stand-in for your session: in your CMS the browser already carries the
  // merchant's login (a cookie), and the server knows who is calling.
  function auth() {
    return { authorization: 'Bearer ' + document.getElementById('token').value };
  }

  // Fetched before the click: FB.login must run inside the click handler
  // itself, or the browser blocks the popup.
  async function start() {
    document.getElementById('connect').disabled = true;
    const r = await fetch('/signup/start', { headers: auth() });
    if (!r.ok) return show(r.status + ' ' + (await r.text()));
    const { state, launch_options } = await r.json();
    attempt = { state, code: null, event: null };
    launchOptions = launch_options;
    document.getElementById('connect').disabled = false;
  }

  // Session info: the new asset ids (FINISH), or the screen left (CANCEL).
  window.addEventListener('message', (e) => {
    let origin;
    try { origin = new URL(e.origin); } catch { return; }
    // Exactly facebook.com or a subdomain, over HTTPS.
    if (origin.protocol !== 'https:' ||
        !(origin.hostname === 'facebook.com' || origin.hostname.endsWith('.facebook.com'))) return;
    let data;
    try { data = typeof e.data === 'string' ? JSON.parse(e.data) : e.data; } catch { return; }
    if (!attempt || !data || data.type !== 'WA_EMBEDDED_SIGNUP') return;
    if (data.event === 'CANCEL') return show('Cancelled at ' + (data.data && data.data.current_step));
    attempt.event = data;
    complete();
  });

  function connect() {
    FB.login((response) => {
      if (!response.authResponse) return show('Not connected.');
      attempt.code = response.authResponse.code; // lives 30 seconds
      complete();
    }, launchOptions);
  }

  let sent = false;
  async function complete() {
    if (sent || !attempt.code || !attempt.event) return;
    sent = true;
    // The merchant's own PIN: never logged, never stored.
    const pin = document.getElementById('pin').value || null;
    const r = await fetch('/signup/complete', {
      method: 'POST',
      headers: { 'content-type': 'application/json', ...auth() },
      body: JSON.stringify({ ...attempt, pin }),
    });
    show(r.status + ' ' + (await r.text()));
  }

  function show(text) { document.getElementById('result').textContent = text; }
</script>
<p><label>Your tenant token (stands in for your CMS login)
  <input id="token" type="password" autocomplete="off" onchange="start()"></label></p>
<p><label>Two-step verification PIN of the number (6 digits: the current one, or the one to set)
  <input id="pin" type="password" inputmode="numeric" pattern="[0-9]{6}" maxlength="6" autocomplete="off"></label></p>
<button id="connect" onclick="connect()" disabled>Connect WhatsApp</button>
<pre id="result"></pre>
"#;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // RUST_LOG=info (or debug) shows what the library logs.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let app_id = env("WA_APP_ID")?;
    // It is written into a script: digits only.
    anyhow::ensure!(
        !app_id.is_empty() && app_id.bytes().all(|b| b.is_ascii_digit()),
        "WA_APP_ID must be the numeric app id"
    );
    let credentials = AppCredentials::new(app_id, env("WA_APP_SECRET")?);
    // No tenants, no signup: the routes are never served unauthenticated.
    let tenants = Tenants::from_json(&env("WA_TENANTS")?)?;
    let kv = kv_store().await?;
    let vault = TokenVault::new(kv.clone(), VaultKeys::new(vault_key()?))?;
    // No default token: onboarding authenticates with the app, then as the
    // merchant.
    let client = wa_rs::client_builder()?.build()?;

    let signup = Signup::new(
        client.embedded_signup(credentials),
        kv,
        vault,
        env("WA_ES_CONFIG_ID")?,
        tenants,
    );
    let bind: IpAddr = std::env::var("WA_BIND")
        .map_or(Ok(Ipv4Addr::LOCALHOST.into()), |a| a.parse())
        .context("WA_BIND must be an IP address")?;
    let port: u16 = std::env::var("PORT").map_or(Ok(3000), |p| p.parse())?;
    let listener = tokio::net::TcpListener::bind((bind, port)).await?;
    println!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app(signup)).await?;
    Ok(())
}

/// Postgres when `DATABASE_URL` is set, memory otherwise.
// Only the Postgres arm awaits; without the feature there is nothing to await.
#[cfg_attr(not(feature = "postgres"), allow(clippy::unused_async))]
async fn kv_store() -> anyhow::Result<Arc<dyn KvStore>> {
    match std::env::var("DATABASE_URL") {
        #[cfg(feature = "postgres")]
        Ok(url) => {
            // sqlx as wa-rs re-exports it: the `PgPool` the stores take.
            use wa_rs::adapters::store::PostgresKvStore;
            use wa_rs::adapters::store::postgres::{self, sqlx};
            let pool = sqlx::PgPool::connect(&url)
                .await
                .context("connect to DATABASE_URL")?;
            postgres::migrate(&pool).await?; // idempotent
            Ok(Arc::new(PostgresKvStore::new(pool)))
        }
        #[cfg(not(feature = "postgres"))]
        Ok(_) => anyhow::bail!("DATABASE_URL is set, but this build lacks `--features postgres`"),
        Err(_) => {
            println!("DATABASE_URL is not set: memory store, emptied on restart");
            Ok(Arc::new(MemoryKvStore::new()))
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

fn env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("set {name} (see the example's header)"))
}
