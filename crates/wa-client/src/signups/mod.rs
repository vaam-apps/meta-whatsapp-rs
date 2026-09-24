//! In-App Signup API: opt-in deep links (`wa.me/<phone>/signup/<id>`) that
//! subscribe WhatsApp users to a business's marketing messages, plus the
//! WABA's default messaging customer base those subscribers land in.
//!
//! Docs: `in-app-signup` (Graph API v22.0+). Messaging customer bases are
//! created and listed on the business: see
//! [`Business::create_messaging_customer_base`](crate::waba::Business::create_messaging_customer_base).
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! # Local validation
//!
//! Checked before any request, from the limits the page states:
//! `signup_message` and `confirmation_message` 1–300 characters,
//! `privacy_policy_url` `http(s)://`, `website_url` `https://`,
//! `promo_code` 1–50 alphanumeric characters, `display_name` 1–256
//! characters, and — on create — a `{{promo_code}}` placeholder requires a
//! `promo_code` (Meta: `2494167`).
//!
//! Not checked locally, on purpose:
//!
//! - **Unknown placeholders.** Meta rejects them (`2494166`) but does not
//!   publish the allowed set; `{{promo_code}}` is the only one documented.
//!   Guessing the set would reject placeholders Meta adds later.
//! - **The placeholder rule on update.** A signup may already carry a stored
//!   `promo_code`; an update that only changes the message is not provably
//!   wrong from here.
//! - **"Alphanumeric"** is checked with Unicode letters and digits, not
//!   ASCII only: the page says "letters and numbers" without narrowing it.
//!
//! Error codes: `2494164` not found ([`wa_core::ErrorKind::NotFound`]),
//! `2494165` API not enabled for the WABA
//! ([`wa_core::ErrorKind::FeatureNotAvailable`]); `2494166` unknown
//! placeholder, `2494167` placeholder without a value, `2494168` terms not
//! accepted, `2494176` terms already accepted, `2494177` terms URL not
//! allowed, `2494179` website URL not https (all
//! [`wa_core::ErrorKind::InvalidParameter`]; read
//! [`wa_core::GraphApiError::code`] to tell them apart).
//!
//! # Terms of Service
//!
//! A business's **first** `create` must carry [`SignupPolicy::accept_terms`]
//! (without it: `2494168`); every later `create` must **omit** it (with it:
//! `2494176`). Only send the policy after the business has actually agreed
//! to the terms at [`SIGNUP_TOS_URL`]; on `2494176`, send the same request
//! without `policy` (nothing was created). This module does not do that
//! retry for you: whether consent was given is your record, not Meta's.
//!
//! Every path is built from segments ([`Client::get_at`] and friends), so a
//! WABA or signup id containing `/` or `..` cannot address another object.

use futures::Stream;
use serde::{Deserialize, Serialize};
use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::{SignupId, WabaId};
use wa_core::paging::Page;

use crate::{Client, GraphRequest};

/// The Terms of Service URL Meta requires in the first create request of a
/// business (`policy.tos`).
pub const SIGNUP_TOS_URL: &str =
    "https://www.facebook.com/legal/ads-manager-marketing-messages-terms";

/// The only placeholder Meta documents in `confirmation_message`.
pub const PROMO_CODE_PLACEHOLDER: &str = "{{promo_code}}";

/// Maximum characters of `signup_message` and `confirmation_message`.
pub const MAX_MESSAGE_CHARS: usize = 300;
/// Maximum characters of `promo_code`.
pub const MAX_PROMO_CODE_CHARS: usize = 50;
/// Maximum characters of `display_name`.
pub const MAX_DISPLAY_NAME_CHARS: usize = 256;
/// Maximum digits of an E.164 number (ITU-T E.164, not a Meta limit).
const MAX_E164_DIGITS: usize = 15;

/// Entry point, see [`Client::signups`].
#[derive(Debug, Clone)]
pub struct Signups {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Signups`] API for `waba_id`.
    pub fn signups(&self, waba_id: impl Into<WabaId>) -> Signups {
        Signups {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

/// Terms of Service acceptance, required on a business's first create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct SignupPolicy {
    /// Terms URL; must be [`SIGNUP_TOS_URL`].
    pub tos: String,
    /// Must be `true`.
    pub accepted: bool,
}

impl SignupPolicy {
    /// Accept the documented terms ([`SIGNUP_TOS_URL`]). Only do this after
    /// the business has actually agreed to them.
    pub fn accept_terms() -> Self {
        Self {
            tos: SIGNUP_TOS_URL.to_owned(),
            accepted: true,
        }
    }
}

/// Body of `POST /{WABA_ID}/signups`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct NewSignup {
    /// Shown on the pre-consent screen (1–300 characters, WhatsApp
    /// formatting allowed).
    pub signup_message: String,
    /// Sent right after opt-in (1–300 characters, may contain
    /// `{{promo_code}}`).
    pub confirmation_message: String,
    /// Privacy policy (`http://` or `https://`). Immutable after creation.
    pub privacy_policy_url: String,
    /// Business website (`https://`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
    /// Replaces `{{promo_code}}` (1–50 letters/digits).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promo_code: Option<String>,
    /// Business-facing nickname (1–256 characters), not shown to users.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Terms acceptance; required on the first create, and an error
    /// (`2494176`) on later ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy: Option<SignupPolicy>,
}

impl NewSignup {
    /// The three required fields.
    pub fn new(
        signup_message: impl Into<String>,
        confirmation_message: impl Into<String>,
        privacy_policy_url: impl Into<String>,
    ) -> Self {
        Self {
            signup_message: signup_message.into(),
            confirmation_message: confirmation_message.into(),
            privacy_policy_url: privacy_policy_url.into(),
            website_url: None,
            promo_code: None,
            display_name: None,
            policy: None,
        }
    }

    /// Business website.
    #[must_use]
    pub fn website_url(mut self, url: impl Into<String>) -> Self {
        self.website_url = Some(url.into());
        self
    }

    /// Promo code for `{{promo_code}}`.
    #[must_use]
    pub fn promo_code(mut self, code: impl Into<String>) -> Self {
        self.promo_code = Some(code.into());
        self
    }

    /// Business-facing nickname.
    #[must_use]
    pub fn display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// Include the Terms of Service acceptance (first create only).
    #[must_use]
    pub fn policy(mut self, policy: SignupPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Check the documented limits.
    pub fn validate(&self) -> Result<(), ValidationError> {
        check_message("signup_message", &self.signup_message)?;
        check_message("confirmation_message", &self.confirmation_message)?;
        if !(self.privacy_policy_url.starts_with("http://")
            || self.privacy_policy_url.starts_with("https://"))
        {
            return Err(ValidationError::new(
                "privacy_policy_url",
                "must start with http:// or https://",
            ));
        }
        if let Some(url) = &self.website_url {
            check_website(url)?;
        }
        if let Some(code) = &self.promo_code {
            check_promo_code(code)?;
        }
        if let Some(name) = &self.display_name {
            check_display_name(name)?;
        }
        if self.confirmation_message.contains(PROMO_CODE_PLACEHOLDER) && self.promo_code.is_none() {
            return Err(ValidationError::new(
                "promo_code",
                "required when confirmation_message contains {{promo_code}}",
            ));
        }
        if let Some(policy) = &self.policy
            && !policy.accepted
        {
            return Err(ValidationError::new(
                "policy.accepted",
                "must be true: the terms must be accepted",
            ));
        }
        Ok(())
    }
}

/// Signup status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum SignupStatus {
    /// The deep link works.
    Active,
    /// Users who open the link see an error. Reversible.
    Disabled,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

/// Body of `POST /signups/{SIGNUP_ID}`; only the fields set are changed.
/// `privacy_policy_url` cannot be updated.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct SignupUpdate {
    /// New status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SignupStatus>,
    /// New pre-consent description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signup_message: Option<String>,
    /// New post-opt-in message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation_message: Option<String>,
    /// New promo code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promo_code: Option<String>,
    /// New nickname.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// New website.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
}

impl SignupUpdate {
    /// Empty update.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the status.
    #[must_use]
    pub fn status(mut self, status: SignupStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Set the pre-consent description.
    #[must_use]
    pub fn signup_message(mut self, message: impl Into<String>) -> Self {
        self.signup_message = Some(message.into());
        self
    }

    /// Set the confirmation message.
    #[must_use]
    pub fn confirmation_message(mut self, message: impl Into<String>) -> Self {
        self.confirmation_message = Some(message.into());
        self
    }

    /// Set the promo code.
    #[must_use]
    pub fn promo_code(mut self, code: impl Into<String>) -> Self {
        self.promo_code = Some(code.into());
        self
    }

    /// Set the nickname.
    #[must_use]
    pub fn display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// Set the website.
    #[must_use]
    pub fn website_url(mut self, url: impl Into<String>) -> Self {
        self.website_url = Some(url.into());
        self
    }

    /// Check the documented limits of the fields present.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if *self == Self::default() {
            return Err(ValidationError::new("update", "nothing to update"));
        }
        if let Some(m) = &self.signup_message {
            check_message("signup_message", m)?;
        }
        if let Some(m) = &self.confirmation_message {
            check_message("confirmation_message", m)?;
        }
        if let Some(code) = &self.promo_code {
            check_promo_code(code)?;
        }
        if let Some(name) = &self.display_name {
            check_display_name(name)?;
        }
        if let Some(url) = &self.website_url {
            check_website(url)?;
        }
        if self.status == Some(SignupStatus::Unknown) {
            return Err(ValidationError::new("status", "must be ACTIVE or DISABLED"));
        }
        Ok(())
    }
}

/// A signup, from `GET /signups/{id}` or the list.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SignupInfo {
    /// Signup id.
    pub id: SignupId,
    /// Parent WABA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba_id: Option<WabaId>,
    /// Pre-consent description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signup_message: Option<String>,
    /// Post-opt-in message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation_message: Option<String>,
    /// Privacy policy URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy_policy_url: Option<String>,
    /// Promo code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promo_code: Option<String>,
    /// Status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SignupStatus>,
    /// Nickname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Website.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
}

/// `{"id": "..."}` returned by create.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreatedSignup {
    /// The new signup id; build links with [`deep_link`].
    pub id: SignupId,
}

/// A WABA's default messaging customer base.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct DefaultMessagingCustomerBase {
    /// The default base id.
    pub default_messaging_customer_base_id: String,
    /// Last change, as Meta formats it (`2026-06-03T19:20:00+0000`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_time: Option<String>,
}

#[derive(Serialize)]
struct DefaultBaseBody<'a> {
    messaging_customer_base_id: &'a str,
}

/// Build the deep link for `signup_id` on `phone_number`, in the format the
/// docs give: `wa.me/<PHONE_NUMBER>/signup/<SIGNUP_ID>`.
///
/// `phone_number` must be strict E.164: `+`, then 1–15 digits, the first
/// not `0`, nothing else (e.g. `+15551234567`, as a business number's
/// `display_phone_number` minus its formatting). `wa.me` reads its digits as
/// an international number, so a number without its country code, or with
/// a national trunk prefix (`+44 (0)20…`), would silently point the link at
/// a different number in another country; requiring the `+` form refuses
/// those instead of guessing. Meta prints the link without a scheme; prefix
/// `https://` when you render it as a hyperlink. Any number of the signup's
/// WABA can be used with the same signup id.
pub fn deep_link(phone_number: &str, signup_id: &SignupId) -> Result<String> {
    let digits = phone_number
        .strip_prefix('+')
        .filter(|d| {
            (1..=MAX_E164_DIGITS).contains(&d.len())
                && d.bytes().all(|b| b.is_ascii_digit())
                && !d.starts_with('0')
        })
        .ok_or_else(|| {
            ValidationError::new(
                "phone_number",
                "must be E.164: + followed by 1-15 digits, no spaces or punctuation",
            )
        })?;
    if signup_id.as_str().is_empty()
        || !signup_id
            .as_str()
            .chars()
            .all(|c| c.is_ascii_alphanumeric())
    {
        return Err(
            ValidationError::new("signup_id", "must be a non-empty alphanumeric id").into(),
        );
    }
    Ok(format!("wa.me/{digits}/signup/{signup_id}"))
}

impl Signups {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `POST /{WABA_ID}/signups`: create a signup deep link entity.
    /// Not replayed on timeouts (it would create a second signup).
    pub async fn create(&self, signup: &NewSignup) -> Result<CreatedSignup> {
        signup.validate()?;
        self.client
            .post_at(&[self.waba_id.as_str(), "signups"])
            .json(signup)
            .context("create signup response")
            .send()
            .await
    }

    /// `GET /signups/{SIGNUP_ID}`.
    pub async fn get(&self, signup_id: &SignupId) -> Result<SignupInfo> {
        self.client
            .get_at(&["signups", signup_id.as_str()])
            .context("signup")
            .send()
            .await
    }

    fn list_request(&self, limit: Option<u32>) -> Result<GraphRequest> {
        if limit == Some(0) {
            return Err(ValidationError::new("limit", "must be at least 1").into());
        }
        Ok(self
            .client
            .get_at(&[self.waba_id.as_str(), "signups"])
            .query_opt("limit", limit)
            .context("signups"))
    }

    /// `GET /{WABA_ID}/signups`, one page of `limit` (Meta's default when
    /// `None`).
    pub async fn list(&self, limit: Option<u32>) -> Result<Page<SignupInfo>> {
        self.list_request(limit)?.send().await
    }

    /// All signups, following cursors.
    pub fn list_stream(
        &self,
        limit: Option<u32>,
    ) -> impl Stream<Item = Result<SignupInfo>> + Send + 'static + use<> {
        crate::request::paginate_or_error(self.list_request(limit))
    }

    /// `POST /signups/{SIGNUP_ID}`: change the fields set in `update`.
    pub async fn update(&self, signup_id: &SignupId, update: &SignupUpdate) -> Result<()> {
        update.validate()?;
        self.client
            .post_at(&["signups", signup_id.as_str()])
            .json(update)
            // Sets fields to values: a replay leaves the same state.
            .idempotent(true)
            .context("update signup response")
            .send_success()
            .await
    }

    /// Disable a signup (there is no delete). Reversible with [`Self::enable`].
    pub async fn disable(&self, signup_id: &SignupId) -> Result<()> {
        self.update(
            signup_id,
            &SignupUpdate::new().status(SignupStatus::Disabled),
        )
        .await
    }

    /// Re-activate a disabled signup.
    pub async fn enable(&self, signup_id: &SignupId) -> Result<()> {
        self.update(signup_id, &SignupUpdate::new().status(SignupStatus::Active))
            .await
    }

    /// `GET /{WABA_ID}/default_messaging_customer_base`.
    pub async fn default_messaging_customer_base(&self) -> Result<DefaultMessagingCustomerBase> {
        self.client
            .get_at(&[self.waba_id.as_str(), "default_messaging_customer_base"])
            .context("default messaging customer base")
            .send()
            .await
    }

    /// `POST /{WABA_ID}/default_messaging_customer_base`: route this WABA's
    /// new subscribers into `messaging_customer_base_id`.
    pub async fn set_default_messaging_customer_base(
        &self,
        messaging_customer_base_id: &str,
    ) -> Result<DefaultMessagingCustomerBase> {
        if messaging_customer_base_id.trim().is_empty() {
            return Err(ValidationError::new("messaging_customer_base_id", "required").into());
        }
        self.client
            .post_at(&[self.waba_id.as_str(), "default_messaging_customer_base"])
            .json(&DefaultBaseBody {
                messaging_customer_base_id,
            })
            .idempotent(true)
            .context("default messaging customer base response")
            .send()
            .await
    }
}

fn check_message(field: &'static str, value: &str) -> Result<(), ValidationError> {
    let n = value.chars().count();
    if n == 0 || n > MAX_MESSAGE_CHARS {
        return Err(ValidationError::new(
            field,
            format!("must be 1-{MAX_MESSAGE_CHARS} characters"),
        ));
    }
    Ok(())
}

fn check_website(url: &str) -> Result<(), ValidationError> {
    if url.starts_with("https://") {
        Ok(())
    } else {
        Err(ValidationError::new(
            "website_url",
            "must start with https://",
        ))
    }
}

fn check_promo_code(code: &str) -> Result<(), ValidationError> {
    let n = code.chars().count();
    if n == 0 || n > MAX_PROMO_CODE_CHARS || !code.chars().all(char::is_alphanumeric) {
        return Err(ValidationError::new(
            "promo_code",
            format!("must be 1-{MAX_PROMO_CODE_CHARS} letters or digits"),
        ));
    }
    Ok(())
}

fn check_display_name(name: &str) -> Result<(), ValidationError> {
    let n = name.chars().count();
    if n == 0 || n > MAX_DISPLAY_NAME_CHARS {
        return Err(ValidationError::new(
            "display_name",
            format!("must be 1-{MAX_DISPLAY_NAME_CHARS} characters"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wa_core::testing::ScriptedTransport;

    use super::*;
    use crate::RetryPolicy;

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn docs_signup() -> NewSignup {
        NewSignup::new(
            "Get exclusive offers and news delivered directly to your WhatsApp!",
            "Thank you for signing up! Here is your welcome code: {{promo_code}}.",
            "https://example-business.com/privacy-policy",
        )
        .promo_code("WELCOME10")
        .display_name("Summer Sale Signup")
        .website_url("https://example-business.com")
        .policy(SignupPolicy::accept_terms())
    }

    fn field(err: &wa_core::Error) -> String {
        match err {
            wa_core::Error::Validation(v) => v.field.clone(),
            other => panic!("not a validation error: {other}"),
        }
    }

    #[tokio::test]
    async fn create_sends_the_docs_example() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"id": "9876543210123456"}));
        let created = client(&t)
            .signups("102290129340398")
            .create(&docs_signup())
            .await
            .unwrap();
        assert_eq!(created.id.as_str(), "9876543210123456");
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/102290129340398/signups");
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({
              "signup_message": "Get exclusive offers and news delivered directly to your WhatsApp!",
              "confirmation_message": "Thank you for signing up! Here is your welcome code: {{promo_code}}.",
              "privacy_policy_url": "https://example-business.com/privacy-policy",
              "promo_code": "WELCOME10",
              "display_name": "Summer Sale Signup",
              "website_url": "https://example-business.com",
              "policy": {
                "tos": "https://www.facebook.com/legal/ads-manager-marketing-messages-terms",
                "accepted": true
              }
            }))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn create_validates_before_sending() {
        let t = ScriptedTransport::new();
        let s = client(&t).signups("W");
        let cases = [
            (
                NewSignup::new("m", "Code: {{promo_code}}", "https://p.example"),
                "promo_code",
            ),
            (
                NewSignup::new("", "c", "https://p.example"),
                "signup_message",
            ),
            (
                NewSignup::new("x".repeat(301), "c", "https://p.example"),
                "signup_message",
            ),
            (
                NewSignup::new("m", "c".repeat(301), "https://p.example"),
                "confirmation_message",
            ),
            (
                NewSignup::new("m", "c", "ftp://p.example"),
                "privacy_policy_url",
            ),
            (
                NewSignup::new("m", "c", "https://p").website_url("http://w.example"),
                "website_url",
            ),
            (
                NewSignup::new("m", "c", "https://p").promo_code("SAVE-20"),
                "promo_code",
            ),
            (
                NewSignup::new("m", "c", "https://p").promo_code("A".repeat(51)),
                "promo_code",
            ),
            (
                NewSignup::new("m", "c", "https://p").display_name(""),
                "display_name",
            ),
            (
                NewSignup::new("m", "c", "https://p").policy(SignupPolicy {
                    tos: SIGNUP_TOS_URL.into(),
                    accepted: false,
                }),
                "policy.accepted",
            ),
        ];
        for (signup, expected) in cases {
            let err = s.create(&signup).await.unwrap_err();
            assert_eq!(field(&err), expected, "{signup:?}");
        }
        assert!(t.requests().is_empty(), "nothing invalid was sent");
        // Boundaries are inclusive and counted in characters.
        let ok = NewSignup::new("é".repeat(300), "c", "http://p.example")
            .promo_code("A".repeat(50))
            .display_name("d".repeat(256));
        assert!(ok.validate().is_ok());
        // Unknown placeholders are left to Meta (2494166).
        assert!(
            NewSignup::new("m", "Hi {{first_name}}", "https://p")
                .validate()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn get_and_list_parse_docs_examples() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
              "id": "9876543210123456",
              "waba_id": "102290129340398",
              "signup_message": "Get exclusive offers and news delivered directly to your WhatsApp!",
              "confirmation_message": "Thank you for signing up!",
              "privacy_policy_url": "https://example-business.com/privacy-policy",
              "promo_code": "WELCOME10",
              "status": "ACTIVE",
              "display_name": "Summer Sale Signup",
              "website_url": "https://example-business.com"
            }),
        );
        t.push_json(
            200,
            json!({
              "data": [
                {"id": "9876543210123456", "signup_message": "Get exclusive offers and news...", "status": "ACTIVE"},
                {"id": "9876543210654321", "signup_message": "Subscribe for weekly updates...", "status": "ACTIVE"}
              ],
              "paging": {
                "cursors": {"before": "xyz789", "after": "abc123"},
                "next": "https://graph.facebook.com/v25.0/102290129340398/signups?limit=10&after=abc123"
              }
            }),
        );
        let s = client(&t).signups("102290129340398");
        let one = s.get(&SignupId::new("9876543210123456")).await.unwrap();
        assert_eq!(one.status, Some(SignupStatus::Active));
        assert_eq!(
            one.waba_id.as_ref().map(WabaId::as_str),
            Some("102290129340398")
        );
        let page = s.list(Some(10)).await.unwrap();
        assert_eq!(page.data.len(), 2);
        assert_eq!(page.next_cursor(), Some("abc123"));
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::GET);
        assert_eq!(reqs[0].path(), "/v25.0/signups/9876543210123456");
        assert_eq!(reqs[1].path(), "/v25.0/102290129340398/signups");
        assert_eq!(reqs[1].query("limit").as_deref(), Some("10"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn list_stream_follows_after_cursor() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "1"}], "paging": {"cursors": {"after": "abc123"}, "next": "https://graph.facebook.com/x"}}),
        );
        t.push_json(200, json!({"data": [{"id": "2", "status": "DISABLED"}]}));
        let items: Vec<SignupInfo> = client(&t)
            .signups("W")
            .list_stream(Some(10))
            .map(Result::unwrap)
            .collect()
            .await;
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].status, Some(SignupStatus::Disabled));
        let reqs = t.requests();
        assert_eq!(reqs[1].query("after").as_deref(), Some("abc123"));
        assert_eq!(reqs[1].query("limit").as_deref(), Some("10"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn update_and_disable_send_only_set_fields() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        t.push_json(200, json!({"success": true}));
        let s = client(&t).signups("W");
        let id = SignupId::new("9876543210123456");
        s.update(
            &id,
            &SignupUpdate::new()
                .confirmation_message(
                    "Welcome! Use code {{promo_code}} for 20% off your first order.",
                )
                .promo_code("SAVE20"),
        )
        .await
        .unwrap();
        s.disable(&id).await.unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::POST);
        assert_eq!(reqs[0].path(), "/v25.0/signups/9876543210123456");
        assert_eq!(
            reqs[0].json(),
            Some(json!({
              "confirmation_message": "Welcome! Use code {{promo_code}} for 20% off your first order.",
              "promo_code": "SAVE20"
            }))
        );
        assert_eq!(reqs[1].json(), Some(json!({"status": "DISABLED"})));
        assert_eq!(t.remaining(), 0);

        assert!(s.update(&id, &SignupUpdate::new()).await.is_err());
        assert!(
            s.update(&id, &SignupUpdate::new().website_url("http://x"))
                .await
                .is_err()
        );
        assert_eq!(t.requests().len(), 2);
    }

    #[tokio::test]
    async fn default_messaging_customer_base_get_and_set() {
        let t = ScriptedTransport::new();
        let body = json!({"default_messaging_customer_base_id": "456789012345678", "updated_time": "2026-06-03T19:20:00+0000"});
        t.push_json(200, body.clone());
        t.push_json(200, body);
        let s = client(&t).signups("102290129340398");
        let set = s
            .set_default_messaging_customer_base("456789012345678")
            .await
            .unwrap();
        let got = s.default_messaging_customer_base().await.unwrap();
        assert_eq!(set, got);
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::POST);
        assert_eq!(
            reqs[0].path(),
            "/v25.0/102290129340398/default_messaging_customer_base"
        );
        assert_eq!(
            reqs[0].json(),
            Some(json!({"messaging_customer_base_id": "456789012345678"}))
        );
        assert_eq!(reqs[1].method, Method::GET);
        assert_eq!(t.remaining(), 0);
    }

    #[test]
    fn deep_link_uses_the_documented_format() {
        // in-app-signup, "Example response": wa.me/15551234567/signup/9876543210123456.
        let id = SignupId::new("9876543210123456");
        assert_eq!(
            deep_link("+15551234567", &id).unwrap(),
            "wa.me/15551234567/signup/9876543210123456"
        );
        assert_eq!(
            deep_link("+123456789012345", &id).unwrap(),
            "wa.me/123456789012345/signup/9876543210123456",
            "15 digits is the E.164 maximum"
        );
        assert!(deep_link("15551234567", &SignupId::new("../x")).is_err());
        assert!(deep_link("+15551234567", &SignupId::new("")).is_err());
    }

    #[test]
    fn deep_link_refuses_numbers_that_are_not_strict_e164() {
        let id = SignupId::new("9876543210123456");
        for bad in [
            // Digits only: `12015553931` meant as a local number would link
            // to +1 201…, a different business.
            "12015553931",
            "5551234567",
            // Formatting could hide a national trunk prefix: +44 (0)20 …
            // would become 44020…, a number that does not exist.
            "+44 (0)20 7946 0000",
            "+1 (555) 123-4567",
            "+1-555-123-4567",
            "+0123456789",
            "+",
            "+1234567890123456",
            "++15551234567",
            "+1555CALLNOW",
            "+１５５５１２３４５６７",
            " +15551234567",
        ] {
            let err = deep_link(bad, &id).unwrap_err();
            assert!(
                matches!(&err, wa_core::Error::Validation(v) if v.field == "phone_number"),
                "{bad:?}: {err}"
            );
        }
    }

    #[tokio::test]
    async fn terms_of_service_errors_are_classified() {
        let t = ScriptedTransport::new();
        let s = client(&t).signups("W");
        for (code, kind) in [
            (2494168, wa_core::ErrorKind::InvalidParameter),
            (2494176, wa_core::ErrorKind::InvalidParameter),
            (2494177, wa_core::ErrorKind::InvalidParameter),
            (2494165, wa_core::ErrorKind::FeatureNotAvailable),
        ] {
            t.push_json(
                400,
                json!({"error": {"message": "x", "type": "OAuthException", "code": code}}),
            );
            let err = s.create(&docs_signup()).await.unwrap_err();
            assert_eq!(err.kind(), kind, "{code}");
            assert_eq!(err.graph().map(|g| g.code), Some(code));
        }
        t.push_json(
            400,
            json!({"error": {"message": "x", "type": "OAuthException", "code": 2494164}}),
        );
        let err = s.get(&SignupId::new("1")).await.unwrap_err();
        assert_eq!(err.kind(), wa_core::ErrorKind::NotFound);
        assert_eq!(t.requests().len(), 5, "creates are not replayed");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn ids_cannot_escape_their_path_segment() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"id": "1"}));
        t.push_json(200, json!({"success": true}));
        t.push_json(200, json!({"id": "9"}));
        let c = client(&t);
        c.signups("OTHER_WABA/signups")
            .create(&docs_signup())
            .await
            .unwrap();
        // A signup id that tries to climb to another object.
        c.signups("W")
            .disable(&SignupId::new("1/../../OTHER_WABA"))
            .await
            .unwrap();
        c.signups("W").get(&SignupId::new("1?x=y")).await.unwrap();
        let paths: Vec<String> = t.requests().iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(
            paths,
            vec![
                "/v25.0/OTHER_WABA%2Fsignups/signups",
                "/v25.0/signups/1%2F..%2F..%2FOTHER_WABA",
                "/v25.0/signups/1%3Fx=y",
            ]
        );
        assert!(matches!(
            c.signups("W").get(&SignupId::new("..")).await,
            Err(wa_core::Error::Validation(_))
        ));
        assert!(c.signups("..").create(&docs_signup()).await.is_err());
        assert_eq!(t.requests().len(), 3, "invalid ids never reach the wire");
        assert_eq!(t.remaining(), 0);
    }
}
