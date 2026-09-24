//! Embedded Signup: a merchant connects their own WhatsApp number to your
//! platform, and you end up holding their business token, encrypted.
//!
//! ```text
//! merchant's browser                         this server                          Meta
//! GET /  (page, Facebook JS SDK)
//! GET /signup/start ───────────────────────► SignupSessions::start (state ↔ tenant)
//!     ◄── {state, launch_options} ───────────┘
//! FB.login(callback, launch_options) ──────────────────────────────────────────► popup
//!     ◄── code (30 s, single use) + WA_EMBEDDED_SIGNUP message event ──────────────┘
//! POST /signup/complete {state, code, event} ► redeem(state, tenant)
//!                                              EmbeddedSignup::onboard ──────────► exchange code, debug_token,
//!                                                 └► TokenVault (by WABA, by number)  verify WABA and number,
//!                                                                                    subscribe app, register
//! ```
//!
//! | Route | What |
//! | --- | --- |
//! | `GET /` | a minimal page with the Facebook JavaScript SDK launch code |
//! | `GET /signup/start?tenant=…` | a new attempt bound to the merchant: `{"state", "launch_options"}` |
//! | `POST /signup/complete?tenant=…` | `{"state", "code", "event"}` from the page: redeem, onboard, store |
//! | `POST /signup/resume?tenant=…` | `{"pin"?}`: after `subscribe_app` or `register_phone` failed, redo just those |
//!
//! **`tenant` in the query string stands in for your own session.** In
//! production it is the merchant id your authentication established for the
//! request, never a value the page chooses; that is what makes
//! [`SignupSessions::redeem`] bind the code to the right merchant.
//!
//! The page must be served over HTTPS from a domain listed in the app's
//! **Allowed domains** and **Valid OAuth redirect URIs** (Facebook Login for
//! Business → Settings); use a tunnel in development. The token stored here
//! is what the `cms_inbox` example replies with: run both against the same
//! `DATABASE_URL` and `WA_VAULT_KEY`.
//!
//! | Variable | Required | What |
//! | --- | --- | --- |
//! | `WA_APP_ID` | yes | your Meta app id (public: it is written into the page) |
//! | `WA_APP_SECRET` | yes | exchanges the code for the business token |
//! | `WA_ES_CONFIG_ID` | yes | the Facebook Login for Business configuration id (App Dashboard → Facebook Login for Business → Configurations) |
//! | `WA_REGISTER_PIN` | no | six digits: register Cloud API numbers with this two-step verification PIN; without it numbers are left unregistered |
//! | `WA_VAULT_KEY` | with `DATABASE_URL` | base64 of 32 random bytes (`openssl rand -base64 32`); without a database a throwaway key is generated |
//! | `WA_VAULT_KEY_ID` | no | id recorded with each encrypted token (default `k1`) |
//! | `DATABASE_URL` | no | Postgres (build with `--features postgres`); memory store otherwise |
//! | `PORT` | no | default `3000` |
//!
//! ```text
//! WA_APP_ID=… WA_APP_SECRET=… WA_ES_CONFIG_ID=… \
//!   cargo run -p wa-rs --example embedded_signup --features axum
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use wa_rs::adapters::store::MemoryKvStore;
use wa_rs::client::embedded_signup::{
    CurrentStep, EmbeddedSignup, EmbeddedSignupEvent, FinishKind, LaunchOptions, OnboardingRequest,
    SignupCode, SignupSessions, SignupState, TokenVault, VaultKey, VaultKeys, steps,
};
use wa_rs::client::phone_numbers::TwoStepPin;
use wa_rs::core::config::ApiVersion;
use wa_rs::prelude::*;

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
    /// Register Cloud API numbers with this PIN; `None` leaves them
    /// unregistered.
    pub pin: Option<TwoStepPin>,
    /// Your tenant → WABA table. Onboarding verifies the WABA with Meta, but
    /// only you know which of *your* merchants may own it.
    pub merchants: Arc<Mutex<HashMap<String, WabaId>>>,
    /// Attempts whose token is stored but whose last steps failed, for
    /// `/signup/resume`.
    unfinished: Arc<Mutex<HashMap<String, (WabaId, OnboardingRequest)>>>,
}

impl Signup {
    /// State for [`app`].
    pub fn new(
        onboarding: EmbeddedSignup,
        kv: Arc<dyn KvStore>,
        vault: TokenVault,
        config_id: impl Into<String>,
        pin: Option<TwoStepPin>,
    ) -> Self {
        Self {
            onboarding,
            sessions: SignupSessions::new(kv),
            vault,
            config_id: config_id.into(),
            pin,
            merchants: Arc::default(),
            unfinished: Arc::default(),
        }
    }
}

/// The routes. `signup.onboarding.app()` supplies the app id written
/// into the page.
pub fn app(signup: Signup) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/signup/start", get(start))
        // The event comes from the browser: small, and not trusted.
        .route(
            "/signup/complete",
            post(complete).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/signup/resume", post(resume))
        .with_state(signup)
}

/// Stand-in for your authentication: whose attempt this is.
#[derive(Deserialize)]
struct Tenant {
    tenant: String,
}

/// `GET /signup/start?tenant=…`: bind a new attempt to the merchant and
/// hand the page what `FB.login` needs.
async fn start(
    State(signup): State<Signup>,
    Query(Tenant { tenant }): Query<Tenant>,
) -> Result<Json<Value>, ApiError> {
    let state = signup.sessions.start(&tenant, ATTEMPT_TTL).await?;
    let launch_options = LaunchOptions::new(signup.config_id.as_str()).to_json()?;
    Ok(Json(
        json!({"state": state.as_str(), "launch_options": launch_options}),
    ))
}

/// Body of `POST /signup/complete`: what the page collected.
#[derive(Deserialize)]
struct Completion {
    state: String,
    /// From the `FB.login` callback.
    code: String,
    /// The `WA_EMBEDDED_SIGNUP` message event, as received.
    event: Value,
}

/// `POST /signup/complete?tenant=…`
async fn complete(
    State(signup): State<Signup>,
    Query(Tenant { tenant }): Query<Tenant>,
    Json(body): Json<Completion>,
) -> Result<Json<Value>, ApiError> {
    // Local checks first: a malformed request must not burn the attempt.
    let state = SignupState::parse(&body.state)?;
    let code = SignupCode::new(body.code)?;
    let event = EmbeddedSignupEvent::from_value(body.event)?;
    if let EmbeddedSignupEvent::Cancel(cancel) = &event {
        let step = cancel.current_step.as_ref().map(CurrentStep::as_str);
        return Ok(Json(json!({"status": "cancelled", "current_step": step})));
    }
    let mut request = OnboardingRequest::from_event(code, &event)?; // FINISH* only
    // Only the Cloud API flow has a number to register: coexistence numbers
    // are registered already, and FINISH_ONLY_WABA has none.
    if let (Some(FinishKind::Finish), Some(pin)) = (event.finish_kind(), &signup.pin) {
        request = request.register_with_pin(pin.clone());
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

/// Body of `POST /signup/resume`.
#[derive(Deserialize)]
struct Resume {
    /// A corrected two-step verification PIN, when `register_phone` failed
    /// on the PIN.
    pin: Option<String>,
}

/// `POST /signup/resume?tenant=…`: redo `subscribe_app` and
/// `register_phone` with the token onboarding stored.
async fn resume(
    State(signup): State<Signup>,
    Query(Tenant { tenant }): Query<Tenant>,
    Json(body): Json<Resume>,
) -> Result<Json<Value>, ApiError> {
    let pin = body.pin.map(TwoStepPin::new).transpose()?; // before taking the entry
    // `resume` acts with the stored token of the WABA it is given: this
    // tenant-keyed table is what ties that WABA to the caller.
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

/// How the signup routes fail: a stable code, never a token or a code.
enum ApiError {
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

  // Stand-in for your session: in production the server knows the merchant.
  const tenant = 'demo-merchant';
  let attempt = null; // {state, code, event}, posted once all three are here
  let launchOptions = null;

  // Fetched before the click: FB.login must run inside the click handler
  // itself, or the browser blocks the popup.
  fetch('/signup/start?tenant=' + encodeURIComponent(tenant))
    .then((r) => r.json())
    .then(({ state, launch_options }) => {
      attempt = { state, code: null, event: null };
      launchOptions = launch_options;
      document.getElementById('connect').disabled = false;
    });

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
    const r = await fetch('/signup/complete?tenant=' + encodeURIComponent(tenant), {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(attempt),
    });
    show(r.status + ' ' + (await r.text()));
  }

  function show(text) { document.getElementById('result').textContent = text; }
</script>
<button id="connect" onclick="connect()" disabled>Connect WhatsApp</button>
<pre id="result"></pre>
"#;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app_id = env("WA_APP_ID")?;
    // It is written into a script: digits only.
    anyhow::ensure!(
        !app_id.is_empty() && app_id.bytes().all(|b| b.is_ascii_digit()),
        "WA_APP_ID must be the numeric app id"
    );
    let credentials = AppCredentials::new(app_id, env("WA_APP_SECRET")?);
    let pin = std::env::var("WA_REGISTER_PIN")
        .ok()
        .map(TwoStepPin::new)
        .transpose()?;
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
        pin,
    );
    let port: u16 = std::env::var("PORT").map_or(Ok(3000), |p| p.parse())?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    println!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app(signup)).await?;
    Ok(())
}

/// Postgres when `DATABASE_URL` is set, memory otherwise.
async fn kv_store() -> anyhow::Result<Arc<dyn KvStore>> {
    match std::env::var("DATABASE_URL") {
        #[cfg(feature = "postgres")]
        Ok(url) => {
            use wa_rs::adapters::store::{PostgresKvStore, postgres};
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
