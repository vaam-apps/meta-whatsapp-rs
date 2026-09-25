//! Embedded Signup: onboard a business (a merchant of your platform) onto
//! the WhatsApp Business Platform from your own site, and end up holding a
//! business integration system user token ("business token") scoped to
//! their WhatsApp Business Account.
//!
//! Docs: `embedded-signup/{overview, implementation, version-4, versions,
//! default-flow, custom-flows, pre-filled-data, pre-verified-numbers,
//! bypass-phone-addition, app-only-install, hosted-es, errors,
//! onboarding-customers-as-a-tech-provider,
//! onboarding-customers-as-a-solution-partner, onboarding-business-app-users,
//! reconnect-offboarded-coexistence-clients}`, `access-tokens`,
//! `permissions`, `solution-providers/{manage-accounts, manage-webhooks,
//! registering-phone-numbers, share-and-revoke-credit-lines,
//! manage-system-users}`, `webhooks/override`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! # The pieces
//!
//! | Piece | Role |
//! | --- | --- |
//! | [`LaunchOptions`] | JSON for `FB.login` (config id, `featureType` incl. coexistence, pre-fill) |
//! | [`SignupSessions`] | binds an attempt to the merchant that started it (single-use state) |
//! | [`EmbeddedSignupEvent`] | parses the `WA_EMBEDDED_SIGNUP` message event the page forwards |
//! | [`EmbeddedSignup::onboard`] | code → verified, stored, subscribed, (credit line shared,) registered business |
//! | [`TokenVault`] | business tokens encrypted at rest, by WABA and by phone number |
//! | [`SolutionPartner`] | per deployment: onboard as a Solution Partner, funding each customer with your credit line; [`EmbeddedSignup::offboard`], [`EmbeddedSignup::revoke_credit_line`], [`EmbeddedSignup::clear_pending_share`] |
//!
//! # End to end
//!
//! ```no_run
//! # async fn demo(
//! #     client: wa_client::Client,
//! #     kv: std::sync::Arc<dyn wa_core::store::KvStore>,
//! #     merchant_id: &str,
//! #     posted_state: &str,
//! #     posted_code: &str,
//! #     posted_event: &str,
//! #     posted_pin: &str,
//! # ) -> wa_core::Result<()> {
//! use std::time::Duration;
//!
//! use wa_client::AppCredentials;
//! use wa_client::embedded_signup::{
//!     EmbeddedSignupEvent, LaunchOptions, OnboardingRequest, SignupCode, SignupSessions,
//!     SignupState, TokenVault, VaultKey, VaultKeys,
//! };
//! use wa_client::phone_numbers::TwoStepPin;
//!
//! // Once, at startup. The vault key comes from your secret manager.
//! let es = client.embedded_signup(AppCredentials::new("<APP_ID>", "<APP_SECRET>"));
//! let vault = TokenVault::new(
//!     kv.clone(),
//!     VaultKeys::new(VaultKey::from_base64("2026-09", "<32 random bytes, base64>")?),
//! )?;
//! let sessions = SignupSessions::new(kv);
//!
//! // 1. The merchant clicks "Connect WhatsApp": bind an attempt to them and
//! //    serve the FB.login options. The page calls FB.login(callback, options)
//! //    and later posts {state, code, event, pin} back to you.
//! let state = sessions.start(merchant_id, Duration::from_secs(900)).await?;
//! let options = LaunchOptions::new("<CONFIGURATION_ID>").to_json()?;
//! # let _ = (state.as_str(), options);
//!
//! // 2. The page posts back. Check everything local first (a malformed post
//! //    must not burn the single-use state), then redeem the state: exactly
//! //    once, and only for the merchant your own authentication says is
//! //    calling.
//! let state = SignupState::parse(posted_state)?;
//! let event = EmbeddedSignupEvent::from_json(posted_event)?;
//! let request = OnboardingRequest::from_event(SignupCode::new(posted_code)?, &event)?
//!     .register_with_pin(TwoStepPin::new(posted_pin)?); // the merchant's own PIN
//! if !sessions.redeem(&state, merchant_id).await? {
//!     return Ok(()); // expired, replayed, or someone else's attempt
//! }
//!
//! // 3. Onboard: exchange the code (30-second, single use), verify the WABA,
//! //    its owner and the number with Meta, store the token encrypted,
//! //    subscribe, register.
//! let onboarded = es.onboard(&request, &vault).await?;
//! // Record onboarded.waba_id against merchant_id in your own tables.
//!
//! // 4. Any time later: act as the merchant.
//! if let Some(stored) = vault.get(&onboarded.waba_id).await? {
//!     let merchant = client.with_token(stored.token);
//!     # let _ = merchant;
//!     // merchant.messages(phone_number_id).send(…).await?;
//! }
//! # Ok(()) }
//! ```
//!
//! # Onboarding steps, and which are safe to repeat
//!
//! [`EmbeddedSignup::onboard`] fails with [`wa_core::Error::Step`] naming the
//! step (constants in [`steps`]), in this order:
//!
//! | Step | What | Repeatable? |
//! | --- | --- | --- |
//! | `exchange_code` | `GET oauth/access_token` | **No**: the code is single-use and lives 30 s |
//! | `debug_token` | `GET debug_token` on the new token | yes (read-only) |
//! | `verify_assets` | WABA ∈ the token's grants; `GET /{WABA}?fields=owner_business_info`; the claimed number ∈ `GET /{WABA}/phone_numbers` | yes (read-only) |
//! | `approve` | [`EmbeddedSignup::onboard_with_approval`] / [`EmbeddedSignup::resume_with_approval`]: your check of the verified WABA, owner and numbers (recorded in the credit ledger in Solution Partner mode); a refusal stops here, before anything is written. In Solution Partner mode, [`EmbeddedSignup::resume`] fails here when no approval is recorded | yours to say |
//! | `store_token` | encrypt into the [`TokenVault`], indexing every number of the WABA | yes (overwrites) |
//! | `subscribe_app` | `POST /{WABA}/subscribed_apps` | yes, with the same override argument |
//! | `assign_system_user` | Solution Partner, [`CreditSharing::ShareAndAttach`] only: `POST /{WABA}/assigned_users` (system user token) | yes (sets the same grant) |
//! | `share_credit_line` | Solution Partner only: check, then share the credit line (see below) | yes: it checks before it posts, and refuses a business whose line was revoked |
//! | `register_phone` | `POST /{PHONE}/register` | yes, but counts against 10 per 72 h |
//!
//! **Retrying a failed `onboard` is not safe**: the code is spent by the
//! first step. The token is stored **before** the steps after it, and only
//! after Meta confirmed which WABA it grants. That order is deliberate: a
//! failure in a later step (a wrong PIN on a number that already has
//! two-step verification, a callback override that fails verification, a
//! credit line Meta refuses, a Meta hiccup) would otherwise lose the token
//! and send the merchant through the whole flow again. Fix the cause and
//! call [`EmbeddedSignup::resume`] with the same request: it loads the
//! stored token (`load_token`), checks the request against what onboarding
//! verified (`verify_assets`), and redoes only the steps after
//! `store_token`.
//!
//! Meta does not document an "already registered" success response for
//! `register` (its "already registered or invalid state" example shares
//! code `100` with other errors), so none is treated as success. `133016`
//! (too many (de)registrations: the number is locked for 72 hours) is
//! [`wa_core::ErrorKind::Registration`] and is never retried automatically.
//!
//! # Solution Partner mode
//!
//! One choice per deployment. Without [`EmbeddedSignup::solution_partner`]
//! onboarding is the Tech Provider flow: no credit line calls, and the
//! merchant adds a payment method in WhatsApp Manager. With it, following
//! `embedded-signup/onboarding-customers-as-a-solution-partner` (subscribe,
//! share the credit line, register):
//!
//! - The WABA's currency comes from [`OnboardingRequest::currency`], else
//!   [`SolutionPartner::default_currency`]; with neither, `onboard` fails
//!   with a [`ValidationError`](wa_core::error::ValidationError) before the
//!   code is exchanged. A Tech Provider request naming a currency (or a
//!   re-share) is refused the same way. Take it from your billing records,
//!   never from the browser. The first currency is sealed before the first
//!   share is posted; another one is refused later.
//! - [`CreditSharing::ShareAndAttach`] (default, Meta's current method)
//!   adds your system user to the WABA (`assign_system_user`), then calls
//!   `whatsapp_credit_sharing_and_attach` with your system user token.
//!   [`CreditSharing::ShareThenAttach`] (Meta's alternate method) calls
//!   `whatsapp_credit_sharing` with the **verified** owner business (never
//!   the browser's `business_id`) and your system user token, then
//!   `whatsapp_credit_attach` with the merchant's business token.
//! - **Approval first, required.** A credit line cannot be taken back from
//!   a WABA once attached, so plain [`EmbeddedSignup::onboard`] is refused
//!   ([`CreditError::ApprovalRequired`](wa_core::error::CreditError::ApprovalRequired),
//!   before the code is exchanged): onboard with
//!   [`EmbeddedSignup::onboard_with_approval`], whose approval runs before
//!   anything is stored, subscribed or shared and is recorded in the credit
//!   ledger for the token record it stores. [`EmbeddedSignup::resume`]
//!   shares only for a WABA whose stored token record was approved so; a
//!   token stored without one (in Tech Provider mode, before the deployment
//!   switched, or stored again since) needs
//!   [`EmbeddedSignup::resume_with_approval`] once. Which of your tenants
//!   may onboard a WABA is your policy; wa-rs decides none
//!   (`OPEN_QUESTIONS.md` #6).
//! - `share_credit_line` **checks before it posts**, in `onboard` and in
//!   `resume` alike, holding a short per-WABA lease renewed right before
//!   each post (a concurrent attempt, or a lease lost to a slow step, is
//!   [`CreditError::Busy`](wa_core::error::CreditError::Busy)): the records
//!   of your line shared with the customer business
//!   (`owning_credit_allocation_configs`) and the allocation recorded in the
//!   vault, each with its `request_status`; an active one whose receiving
//!   credential is the WABA's `primary_funding_id` means nothing is posted.
//!   A share whose answer is lost (a timeout, a 5xx) may have succeeded,
//!   and Meta refuses to change a line once attached, so it is
//!   [`CreditError::Reconcile`](wa_core::error::CreditError::Reconcile)
//!   (not retryable), and a share is never posted again blindly: the
//!   ledger flags each post until its allocation is recorded
//!   ([`StoredCredit::pending_share`]), and a flagged share that no record
//!   explains, on a WABA something funds, is `Reconcile` too. (When nothing
//!   funds the WABA, `resume` posts again: that assumes Meta shows an
//!   applied share at once, which it does not document.) A flagged share
//!   Meta never shows (a post that never reached it) stays flagged until an
//!   operator who checked Meta Business Suite calls
//!   [`EmbeddedSignup::clear_pending_share`]: it checks Meta again, clears
//!   nothing while a record may be live, and seals who cleared it and when
//!   in the ledger ([`StoredCredit::cleared_shares`]).
//!   Without the owner business (`owner_business_info`) nothing can be
//!   checked or revoked later, so nothing is shared
//!   ([`CreditError::OwnerUnknown`](wa_core::error::CreditError::OwnerUnknown)).
//! - **A revoked business stays revoked.** When
//!   [`EmbeddedSignup::revoke_credit_line`] marked the business revoked, or
//!   Meta reports only `DELETED` records for it, `share_credit_line`
//!   refuses ([`CreditError::Revoked`](wa_core::error::CreditError::Revoked);
//!   a record whose `request_status` Meta does not document is
//!   [`CreditError::StatusUnknown`](wa_core::error::CreditError::StatusUnknown))
//!   unless the request says
//!   [`OnboardingRequest::reshare_after_revocation`]: funding a merchant
//!   again is a product decision. A revocation that runs while a share is
//!   posted ends with the line revoked or the share reported: the
//!   revocation writes its marker before it looks anything up, and the
//!   share reads the marker after it posts (also when the post's answer was
//!   lost). A share that finds a new or changed marker revokes what it may
//!   have made (`Revoked` with `posted`), or, when it cannot find it,
//!   reports it (`Reconcile`) and keeps it pending; the revocation reports
//!   a pending share it revoked nothing for as incomplete (`share_pending`),
//!   never as done.
//! - The allocation is returned in [`Onboarded::allocation_config_id`] and
//!   recorded in the vault's credit ledger ([`TokenVault::credit`],
//!   [`StoredCredit`]), which outlives the token.
//! - [`EmbeddedSignup::revoke_credit_line`] revokes from the owner business
//!   recorded at onboarding (else the business Meta's record of the
//!   recorded allocation names, else a signed webhook's
//!   `owner_business_id` when your line has records for it), for when the
//!   customer removes you (`account_update` `PARTNER_REMOVED`) and the WABA
//!   can no longer be read: call it at once on every `PARTNER_REMOVED` of
//!   your solution, coexistence disconnections included (the owner's
//!   decision, 2026-09-25); nothing calls it for you. It revokes for every
//!   WABA of that business.
//!   [`EmbeddedSignup::revoke_business_credit_line`] does the same from a
//!   business id alone. [`EmbeddedSignup::offboard`] revokes first and
//!   deletes the token second (a CMS disconnect, the
//!   `PARTNER_APP_UNINSTALLED` of your app); either order with
//!   `PARTNER_REMOVED` ends revoked. A revocation that stops part-way is
//!   [`CreditError::RevocationIncomplete`](wa_core::error::CreditError::RevocationIncomplete),
//!   with the report.
//!
//! The lower-level calls are in [`crate::credit_lines`].
//!
//! # The session info is a claim
//!
//! The ids in the message event come from the browser. Before anything is
//! stored, `onboard` requires the token to be valid and issued to this app
//! (a `debug_token` answer without `app_id` is refused), the WABA to be one
//! the exchanged token was granted (`granular_scopes`), and the phone number
//! to be one of that WABA's numbers as Meta lists them to the business
//! token. Otherwise a merchant could post their own code with another
//! merchant's ids and overwrite that merchant's vault entry or phone
//! routing. The claimed `business_id` cannot be checked this way; it is not
//! used, and the WABA's owner business as Meta reports it is stored instead
//! (it is what credit line sharing is keyed on).
//!
//! What this cannot decide for you: whether *your* tenant may own the WABA.
//! The vault is keyed by WABA and knows no tenants. If one Meta business
//! portfolio legitimately holds WABAs that two of your tenants onboarded,
//! either tenant's token is granted both, and the last onboarding of a WABA
//! replaces its vault entry (with a token Meta confirmed can manage it) by
//! the time `onboard` returns. Keep your own tenant → WABA mapping, and
//! decide what a second tenant onboarding an already-mapped
//! [`Onboarded::waba_id`] means for your product.
//!
//! # Coexistence (WhatsApp Business app users)
//!
//! Launch with [`LaunchOptions::coexistence`]. The flow ends with
//! `FINISH_WHATSAPP_BUSINESS_APP_ONBOARDING` and only a WABA id; `onboard`
//! resolves the number from the WABA and refuses to register it (it is
//! already registered). Then, within 24 hours and once each, call
//! [`PhoneNumber::sync_smb_app_data`](crate::phone_numbers::PhoneNumber::sync_smb_app_data)
//! with [`SmbSyncType::SmbAppStateSync`](crate::phone_numbers::SmbSyncType)
//! and `History`, and subscribe to the `history`, `smb_app_state_sync` and
//! `smb_message_echoes` webhook fields.
//!
//! # Secrets
//!
//! The code, the app secret and the token under inspection travel in query
//! strings (that is how Meta defines these two endpoints). This module never
//! logs them, does not let the client's retry loop log errors for these
//! requests, and replaces transport and non-Graph HTTP errors from them with
//! ones that keep the classification but drop the text, because transport
//! libraries commonly render the full URL into their errors.

mod event;
mod launch;
mod ledger;
mod onboard;
mod partner;
mod session;
mod token;
mod vault;

pub use event::{
    CancelInfo, CurrentStep, EmbeddedSignupEvent, FinishKind, MESSAGE_TYPE, ReportedError,
    SessionInfo,
};
pub use launch::{
    AddressPrefill, BusinessPhonePrefill, BusinessPrefill, EsVersion, FeatureName, FeatureType,
    LaunchOptions, MAX_BUSINESS_NAME_CHARS, MAX_PHONE_DESCRIPTION_CHARS, PhoneProfilePrefill,
    PreVerifiedPhone, Setup, WabaPrefill,
};
pub use ledger::{ClearedShare, RevokedBusiness, StoredCredit};
pub use onboard::{Onboarded, OnboardingRequest, VerifiedOnboarding, steps};
pub use partner::{CreditSharing, Offboarded, PendingShareClearance, SharesFound, SolutionPartner};
pub use session::{SESSION_NAMESPACE, SignupSessions, SignupState};
pub use token::{
    BusinessToken, GranularScope, SignupCode, TokenDebug, TokenType, WHATSAPP_BUSINESS_MANAGEMENT,
    WHATSAPP_BUSINESS_MESSAGING,
};
pub use vault::{StoredBusinessToken, TOKEN_NAMESPACE, TokenVault, VaultKey, VaultKeys};

use std::future::Future;

use wa_core::error::TransportError;
use wa_core::secret::AccessToken;
use wa_core::{Error, Result};

use crate::{AppCredentials, Client};

/// Placeholder that replaces error details which may contain a URL with
/// secrets in its query string.
const WITHHELD: &str = "(details withheld: the request URL carries credentials)";

/// Entry point, see [`Client::embedded_signup`].
#[derive(Debug, Clone)]
pub struct EmbeddedSignup {
    client: Client,
    app: AppCredentials,
    inspector: Option<AccessToken>,
    partner: Option<SolutionPartner>,
}

impl Client {
    /// Embedded Signup operations for the app identified by `app`.
    pub fn embedded_signup(&self, app: AppCredentials) -> EmbeddedSignup {
        EmbeddedSignup {
            client: self.clone(),
            app,
            inspector: None,
            partner: None,
        }
    }
}

impl EmbeddedSignup {
    /// The app these operations act for.
    pub fn app(&self) -> &AppCredentials {
        &self.app
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Authenticate `debug_token` with `token` instead of the app token
    /// (`app_id|app_secret`).
    ///
    /// `solution-providers/manage-accounts` shows the call made with a system
    /// user token; the Graph `debug_token` endpoint also accepts the app
    /// token, which is the default here because every integrator has it.
    #[must_use]
    pub fn inspect_with(mut self, token: AccessToken) -> Self {
        self.inspector = Some(token);
        self
    }

    /// Exchange the code returned by `FB.login` for a business token:
    /// `GET /{version}/oauth/access_token?client_id&client_secret&code`,
    /// without an `Authorization` header.
    ///
    /// Do it as soon as the code arrives (30-second lifetime, single use).
    /// Only connection failures are retried (within the client's retry
    /// budget): they prove the request never reached Meta. After a timeout
    /// or a server error the code may already be spent, and a second
    /// exchange would fail with "code already used", hiding the real cause;
    /// those are returned as they are. Graph errors (an expired or reused
    /// code is an `OAuthException`) are never retried.
    pub async fn exchange_code(&self, code: &SignupCode) -> Result<BusinessToken> {
        let never_sent = |e: &Error| matches!(e, Error::Transport(TransportError::Connect(_)));
        let body = self
            .quiet_retries(never_sent, || {
                self.client
                    .get("oauth/access_token")
                    .query("client_id", &self.app.app_id)
                    .query("client_secret", self.app.app_secret.expose_secret())
                    .query("code", code.expose_secret())
                    .no_auth()
                    // Retried by `quiet_retries` instead, which does not log
                    // error text (it may contain this URL).
                    .idempotent(false)
                    .send_raw()
            })
            .await?
            .body;
        decode_business_token(&body)
    }

    /// Inspect `input` with `GET /{version}/debug_token?input_token=…`,
    /// authenticated with the app token (or [`Self::inspect_with`]).
    ///
    /// [`TokenDebug::waba_ids`] lists the WABAs the token can manage, newest
    /// first.
    pub async fn debug_token(&self, input: &AccessToken) -> Result<TokenDebug> {
        let auth = self
            .inspector
            .clone()
            .unwrap_or_else(|| self.app.app_token());
        let resp = self
            .quiet_retries(
                |_| true,
                || {
                    self.client
                        .get("debug_token")
                        .query("input_token", input.expose_secret())
                        .bearer(&auth)
                        .idempotent(false)
                        .send_raw()
                },
            )
            .await?;
        let env: token::DebugEnvelope =
            crate::request::decode_json("debug_token response", &resp.body)?;
        Ok(env.data)
    }

    /// Run `attempt`, retrying errors that `eligible` accepts and the
    /// client's retry policy deems retryable, without logging them, and
    /// return a scrubbed error.
    ///
    /// The requests passed in are marked non-idempotent, so `GraphRequest`
    /// itself only replays throttling rejections (whose logged text is a
    /// Graph error, not a URL). Those are not retried again here.
    async fn quiet_retries<F, Fut, T>(
        &self,
        eligible: impl Fn(&Error) -> bool,
        mut attempt: F,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let policy = self.client.shared.retry;
        let mut n = 0u32;
        loop {
            match attempt().await {
                Ok(v) => return Ok(v),
                Err(e)
                    if eligible(&e)
                        && !retried_by_request(&e)
                        && policy.should_retry(n, &e, true) =>
                {
                    tokio::time::sleep(policy.delay(n, None)).await;
                    n += 1;
                }
                Err(e) => return Err(withhold_url_details(e)),
            }
        }
    }
}

/// Errors `GraphRequest` already replays for non-idempotent requests.
fn retried_by_request(e: &Error) -> bool {
    match e {
        Error::Api(g) => g.kind().is_rejected_before_processing(),
        Error::Http { status: 429, .. } => true,
        _ => false,
    }
}

/// Keep the error's class (and so its retryability) but drop any text that
/// could include the request URL.
fn withhold_url_details(e: Error) -> Error {
    match e {
        Error::Transport(TransportError::Timeout) => Error::Transport(TransportError::Timeout),
        Error::Transport(TransportError::Connect(_)) => {
            Error::Transport(TransportError::Connect(anyhow::anyhow!(WITHHELD)))
        }
        Error::Transport(_) => Error::Transport(TransportError::Backend(anyhow::anyhow!(WITHHELD))),
        Error::Http { status, .. } => Error::Http {
            status,
            body_snippet: WITHHELD.to_owned(),
        },
        other => other,
    }
}

/// Decode the code exchange response. On failure neither the body (it may
/// hold the token) nor serde's message (it quotes values) is kept.
fn decode_business_token(body: &[u8]) -> Result<BusinessToken> {
    const CONTEXT: &str = "code exchange response";
    const REDACTED: &[u8] = b"(withheld: may contain an access token)";
    let shape_error = |detail: String| {
        Error::decode(
            CONTEXT,
            <serde_json::Error as serde::de::Error>::custom(detail),
            REDACTED,
        )
    };
    match serde_json::from_slice::<BusinessToken>(body) {
        Ok(t) if !t.access_token.expose_secret().is_empty() => Ok(t),
        Ok(_) => Err(shape_error("empty access_token".to_owned())),
        Err(e) => Err(shape_error(format!(
            "expected {{\"access_token\", \"token_type\"?, \"expires_in\"?}} ({:?} error at line {} column {})",
            e.classify(),
            e.line(),
            e.column()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use http::Method;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wa_core::ErrorKind;
    use wa_core::testing::ScriptedTransport;

    use super::*;
    use crate::RetryPolicy;

    const APP_SECRET: &str = "614fc2afde15eee07a26b2fe3eaee9b9";
    const CODE: &str =
        "AQBhlXsctMxJYbwbrpybxlo9tLPGy-QAmjBJA03jxLos43wxlBlrYozY5C33BXJULd133cOJf_5y6EkJ";
    const BUSINESS_TOKEN: &str =
        "EAAAN6tcBzAUBOwtDtTfmZCJ9n3FHpSDcDTH86ekf89XnnMZAtaitMUysPDE7LES3CXkA4";

    /// The client carries a default token on purpose: neither the code
    /// exchange nor `debug_token` may send it.
    fn client(t: &ScriptedTransport, retry: RetryPolicy) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("DEFAULT_CLIENT_TOKEN")
            .retry(retry)
            .build()
            .unwrap()
    }

    fn es(t: &ScriptedTransport) -> EmbeddedSignup {
        client(t, RetryPolicy::NONE)
            .embedded_signup(AppCredentials::new("236484624622562", APP_SECRET))
    }

    fn retrying() -> RetryPolicy {
        RetryPolicy {
            max_retries: 2,
            base_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
        }
    }

    fn assert_no_secret(text: &str) {
        for secret in [APP_SECRET, CODE, BUSINESS_TOKEN] {
            assert!(!text.contains(secret), "secret leaked into: {text}");
        }
    }

    #[tokio::test]
    async fn exchange_code_uses_query_params_and_no_auth_header() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"access_token": BUSINESS_TOKEN, "token_type": "bearer"}),
        );
        let token = es(&t)
            .exchange_code(&SignupCode::new(CODE).unwrap())
            .await
            .unwrap();
        assert_eq!(token.access_token.expose_secret(), BUSINESS_TOKEN);
        assert_eq!(token.token_type.as_deref(), Some("bearer"));
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/oauth/access_token");
        assert_eq!(req.query("client_id").as_deref(), Some("236484624622562"));
        assert_eq!(req.query("client_secret").as_deref(), Some(APP_SECRET));
        assert_eq!(req.query("code").as_deref(), Some(CODE));
        assert_eq!(req.header("authorization"), None);
        assert_eq!(t.remaining(), 0);
        assert_no_secret(&format!("{token:?}"));
    }

    #[tokio::test]
    async fn exchange_code_graph_error_is_kept_and_not_retried() {
        let t = ScriptedTransport::new();
        t.push_json(
            400,
            json!({"error": {"message": "This authorization code has been used.", "type": "OAuthException", "code": 100, "error_subcode": 36009, "fbtrace_id": "A"}}),
        );
        let e = client(&t, retrying())
            .embedded_signup(AppCredentials::new("1", APP_SECRET))
            .exchange_code(&SignupCode::new(CODE).unwrap())
            .await
            .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidParameter);
        assert_eq!(e.graph().unwrap().error_subcode, Some(36009));
        assert_eq!(t.requests().len(), 1);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn exchange_code_transport_errors_are_retried_and_scrubbed() {
        // An adapter that renders the URL into its error, as reqwest does.
        let leaky = || {
            TransportError::Connect(anyhow::anyhow!(
                "error sending request for url (https://graph.facebook.com/v25.0/oauth/access_token?client_id=1&client_secret={APP_SECRET}&code={CODE})"
            ))
        };
        let t = ScriptedTransport::new();
        t.push_error(leaky);
        t.push_json(200, json!({"access_token": BUSINESS_TOKEN}));
        let es = client(&t, retrying()).embedded_signup(AppCredentials::new("1", APP_SECRET));
        let token = es
            .exchange_code(&SignupCode::new(CODE).unwrap())
            .await
            .unwrap();
        assert_eq!(token.access_token.expose_secret(), BUSINESS_TOKEN);
        assert_eq!(t.requests().len(), 2, "connect failure retried once");

        for _ in 0..3 {
            t.push_error(leaky);
        }
        let e = es
            .exchange_code(&SignupCode::new(CODE).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Transport(TransportError::Connect(_))));
        assert!(e.is_retryable(), "classification kept");
        assert_no_secret(&format!("{e} {e:?}"));
        assert_eq!(t.remaining(), 0);

        t.push_bytes(
            502,
            "text/html",
            format!("<html>bad gateway for ?client_secret={APP_SECRET}</html>"),
        );
        let e = es_no_retry(&t)
            .exchange_code(&SignupCode::new(CODE).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Http { status: 502, .. }));
        assert_no_secret(&format!("{e} {e:?}"));
    }

    #[tokio::test]
    async fn exchange_code_is_not_retried_when_the_code_may_be_spent() {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        t.push_json(
            500,
            json!({"error": {"message": "x", "code": 2, "is_transient": true}}),
        );
        let es = client(&t, retrying()).embedded_signup(AppCredentials::new("1", APP_SECRET));
        let e = es
            .exchange_code(&SignupCode::new(CODE).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(e, Error::Transport(TransportError::Timeout)));
        assert_eq!(t.requests().len(), 1, "a timeout is returned, not replayed");
        let e = es
            .exchange_code(&SignupCode::new(CODE).unwrap())
            .await
            .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::ServiceUnavailable);
        assert_eq!(
            t.requests().len(),
            2,
            "a server error is returned, not replayed"
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn debug_token_is_retried_on_transient_errors() {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        t.push_json(
            500,
            json!({"error": {"message": "x", "code": 2, "is_transient": true}}),
        );
        t.push_json(200, json!({"data": {"is_valid": true}}));
        let debug = client(&t, retrying())
            .embedded_signup(AppCredentials::new("1", APP_SECRET))
            .debug_token(&AccessToken::new(BUSINESS_TOKEN))
            .await
            .unwrap();
        assert!(debug.is_valid);
        assert_eq!(t.requests().len(), 3);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn debug_token_does_not_replay_permanent_or_already_throttled_errors() {
        let t = ScriptedTransport::new();
        t.push_json(400, json!({"error": {"message": "bad", "code": 100}}));
        let es = client(&t, retrying()).embedded_signup(AppCredentials::new("1", APP_SECRET));
        let e = es
            .debug_token(&AccessToken::new(BUSINESS_TOKEN))
            .await
            .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidParameter);
        assert_eq!(t.requests().len(), 1, "a permanent error is not replayed");

        // Throttling is replayed by GraphRequest itself (max_retries = 2);
        // the quiet loop must not multiply that budget.
        for _ in 0..3 {
            t.push_json(
                400,
                json!({"error": {"message": "slow down", "code": 130429}}),
            );
        }
        let e = es
            .debug_token(&AccessToken::new(BUSINESS_TOKEN))
            .await
            .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::RateLimited);
        assert_eq!(t.requests().len(), 4, "1 + 3 throttled attempts");
        assert_eq!(t.remaining(), 0);
    }

    fn es_no_retry(t: &ScriptedTransport) -> EmbeddedSignup {
        client(t, RetryPolicy::NONE).embedded_signup(AppCredentials::new("1", APP_SECRET))
    }

    #[tokio::test]
    async fn exchange_code_bad_body_never_echoes_the_token() {
        let t = ScriptedTransport::new();
        // The shape the ES pages literally print: a bare token.
        t.push_bytes(200, "application/json", format!("\"{BUSINESS_TOKEN}\""));
        t.push_json(200, json!({"access_token": ""}));
        t.push_json(200, json!({"access_token": 12345, "note": BUSINESS_TOKEN}));
        let es = es_no_retry(&t);
        for _ in 0..3 {
            let e = es
                .exchange_code(&SignupCode::new(CODE).unwrap())
                .await
                .unwrap_err();
            assert!(matches!(e, Error::Decode { .. }), "{e}");
            assert_no_secret(&format!("{e} {e:?}"));
        }
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn debug_token_uses_app_token_and_parses_docs_example() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
              "data": {
                "app_id": "670843887433847",
                "application": "JaspersMarket",
                "data_access_expires_at": 1672092840,
                "expires_at": 1665090000,
                "granular_scopes": [
                  {"scope": "whatsapp_business_management", "target_ids": ["102289599326934", "101569239400667"]},
                  {"scope": "whatsapp_business_messaging", "target_ids": ["102289599326934", "101569239400667"]}
                ],
                "is_valid": true,
                "scopes": ["whatsapp_business_management", "whatsapp_business_messaging", "public_profile"],
                "type": "USER",
                "user_id": "10222270944537964"
              }
            }),
        );
        let debug = es(&t)
            .debug_token(&AccessToken::new(BUSINESS_TOKEN))
            .await
            .unwrap();
        assert_eq!(debug.waba_ids()[0].as_str(), "102289599326934");
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/debug_token");
        assert_eq!(req.query("input_token").as_deref(), Some(BUSINESS_TOKEN));
        assert_eq!(
            req.bearer(),
            Some(format!("236484624622562|{APP_SECRET}").as_str())
        );
        assert_eq!(t.remaining(), 0);

        t.push_json(200, json!({"data": {"is_valid": false}}));
        es(&t)
            .inspect_with(AccessToken::new("SYSTEM_USER_TOKEN"))
            .debug_token(&AccessToken::new(BUSINESS_TOKEN))
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().bearer(),
            Some("SYSTEM_USER_TOKEN")
        );
        assert_eq!(t.remaining(), 0);
    }

    #[test]
    fn debug_output_of_the_entry_point_hides_the_app_secret() {
        let t = ScriptedTransport::new();
        let dbg = format!(
            "{:?}",
            es(&t).inspect_with(AccessToken::new(BUSINESS_TOKEN))
        );
        assert_no_secret(&dbg);
    }
}
