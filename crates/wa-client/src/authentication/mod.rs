//! Authentication templates (copy code, one-tap autofill, zero-tap),
//! template previews, bulk upsert, and the [`OtpService`] (generate, store
//! hashed, send, verify).
//!
//! Docs: `templates/authentication-templates/*` (authentication-templates,
//! copy-code-button-, autofill-button-, zero-tap-authentication-templates,
//! template-preview, bulk-management, error-signals, keyboard-suggestions,
//! authentication-best-practices), `templates/time-to-live`,
//! `business-scoped-user-ids` (OTP buttons need a phone number).
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! Not wrapped, because there is nothing server-side to wrap: the Android
//! handshake and the `com.whatsapp.otp.OTP_ERROR` error signals
//! (`error-signals`) happen in your Android app, and the iOS keyboard
//! suggestion opt-out (`keyboard-suggestions`) exists only in WhatsApp
//! Manager.
//!
//! # Create an authentication template
//!
//! ```no_run
//! # async fn demo(client: wa_client::Client) -> wa_core::Result<()> {
//! use wa_client::authentication::{AuthenticationTemplate, SupportedApp};
//!
//! let template = AuthenticationTemplate::one_tap(
//!     "login_code",
//!     "en_US",
//!     [SupportedApp::new("com.example.luckyshrub", "K8a/AINcGX7")],
//! )
//! .security_recommendation(true)
//! .code_expiration_minutes(10) // keep equal to OtpConfig::ttl
//! .message_send_ttl_seconds(600);
//! let created = client.authentication("<WABA_ID>").create(&template).await?;
//! # let _ = created; Ok(()) }
//! ```
//!
//! # Issue and verify a one-time passcode
//!
//! ```no_run
//! # async fn demo(
//! #     client: wa_client::Client,
//! #     kv: std::sync::Arc<dyn wa_core::store::KvStore>,
//! # ) -> anyhow::Result<()> {
//! use std::sync::Arc;
//! use wa_client::authentication::{
//!     IssueOutcome, OtpConfig, OtpPepper, OtpService, OtpTemplate, VerifyOutcome,
//! };
//! use wa_core::clock::SystemClock;
//! use wa_core::recipient::Recipient;
//!
//! let otp = OtpService::new(
//!     client,
//!     "<PHONE_NUMBER_ID>",
//!     OtpTemplate::new("login_code", "en_US"),
//!     kv,
//!     Arc::new(SystemClock),
//!     OtpPepper::new(std::env::var("OTP_PEPPER")?.into_bytes())?,
//!     OtpConfig::default(),
//! )?;
//!
//! let user = Recipient::phone("+16505551234");
//! match otp.issue(&user, "login").await? {
//!     IssueOutcome::Sent(challenge) => println!("sent, expires at {}", challenge.expires_at),
//!     IssueOutcome::CoolingDown { retry_after } | IssueOutcome::RateLimited { retry_after } => {
//!         println!("try again in {retry_after:?}")
//!     }
//! }
//!
//! // …the user types the code they received…
//! match otp.verify(&user, "login", "123456").await? {
//!     VerifyOutcome::Verified => println!("welcome"),
//!     VerifyOutcome::Invalid { attempts_left } => println!("wrong code, {attempts_left} left"),
//!     VerifyOutcome::Expired | VerifyOutcome::NotFound => println!("request a new code"),
//!     VerifyOutcome::TooManyAttempts => println!("locked; request a new code later"),
//! }
//! # Ok(()) }
//! ```

mod otp;

#[cfg(test)]
mod tests;

pub use crate::templates::{OtpButton, SupportedApp};
pub use otp::{
    Challenge, IssueLimit, IssueOutcome, OtpConfig, OtpPepper, OtpService, OtpTemplate,
    VerifyOutcome,
};

use serde::{Deserialize, Serialize};
use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::{TemplateId, WabaId};

use crate::Client;
use crate::templates::{
    BodyComponent, Button, DataList, FooterComponent, Parameter, TemplateCategory,
    TemplateComponent, TemplateCreated, TemplateDefinition, TemplateMessage, TemplateStatus,
    validate,
};

/// Entry point, see [`Client::authentication`].
#[derive(Debug, Clone)]
pub struct Authentication {
    client: Client,
    waba_id: WabaId,
}

impl Client {
    /// [`Authentication`] API for `waba_id`.
    pub fn authentication(&self, waba_id: impl Into<WabaId>) -> Authentication {
        Authentication {
            client: self.clone(),
            waba_id: waba_id.into(),
        }
    }
}

/// An authentication template: Meta's preset text ("*{{1}}* is your
/// verification code."), an optional security recommendation, an optional
/// expiry footer and one OTP button. URLs, media and emojis are not
/// supported, so there is nothing else to set.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthenticationTemplate {
    /// Template name.
    pub name: String,
    /// Language code.
    pub language: String,
    /// The button: copy code, one-tap or zero-tap.
    pub otp: OtpButton,
    /// Append "For your security, do not share this code."
    pub add_security_recommendation: Option<bool>,
    /// Show "This code expires in N minutes." (1–90). Also disables one-tap
    /// buttons after N minutes (10 when absent). Keep it equal to the
    /// [`OtpConfig::ttl`] you verify with.
    pub code_expiration_minutes: Option<u32>,
    /// Message time-to-live, 30–900 seconds or `-1` (30 days). Meta's
    /// default is 10 minutes; the TTL page advises at most the code expiry.
    pub message_send_ttl_seconds: Option<i64>,
}

impl AuthenticationTemplate {
    fn with(name: impl Into<String>, language: impl Into<String>, otp: OtpButton) -> Self {
        Self {
            name: name.into(),
            language: language.into(),
            otp,
            add_security_recommendation: None,
            code_expiration_minutes: None,
            message_send_ttl_seconds: None,
        }
    }

    /// Copy-code button template.
    pub fn copy_code(name: impl Into<String>, language: impl Into<String>) -> Self {
        Self::with(name, language, OtpButton::CopyCode { text: None })
    }

    /// One-tap autofill template for up to 5 Android apps.
    pub fn one_tap(
        name: impl Into<String>,
        language: impl Into<String>,
        apps: impl IntoIterator<Item = SupportedApp>,
    ) -> Self {
        Self::with(
            name,
            language,
            OtpButton::OneTap {
                text: None,
                autofill_text: None,
                supported_apps: apps.into_iter().collect(),
            },
        )
    }

    /// Zero-tap template for up to 5 Android apps. `terms_accepted` states
    /// that you accept the zero-tap terms of the WhatsApp Business Terms of
    /// Service and that your users expect automatic code entry; Meta will not
    /// create the template unless it is `true`.
    pub fn zero_tap(
        name: impl Into<String>,
        language: impl Into<String>,
        apps: impl IntoIterator<Item = SupportedApp>,
        terms_accepted: bool,
    ) -> Self {
        Self::with(
            name,
            language,
            OtpButton::ZeroTap {
                text: None,
                autofill_text: None,
                zero_tap_terms_accepted: terms_accepted,
                supported_apps: apps.into_iter().collect(),
            },
        )
    }

    /// Set `add_security_recommendation`.
    #[must_use]
    pub fn security_recommendation(mut self, add: bool) -> Self {
        self.add_security_recommendation = Some(add);
        self
    }

    /// Set `code_expiration_minutes`.
    #[must_use]
    pub fn code_expiration_minutes(mut self, minutes: u32) -> Self {
        self.code_expiration_minutes = Some(minutes);
        self
    }

    /// Set `message_send_ttl_seconds`.
    #[must_use]
    pub fn message_send_ttl_seconds(mut self, seconds: i64) -> Self {
        self.message_send_ttl_seconds = Some(seconds);
        self
    }

    /// Copy-code button label (fallback label for one-tap / zero-tap).
    #[must_use]
    pub fn button_text(mut self, label: impl Into<String>) -> Self {
        match &mut self.otp {
            OtpButton::CopyCode { text }
            | OtpButton::OneTap { text, .. }
            | OtpButton::ZeroTap { text, .. } => *text = Some(label.into()),
        }
        self
    }

    /// Autofill button label (one-tap / zero-tap; ignored for copy code).
    #[must_use]
    pub fn autofill_text(mut self, label: impl Into<String>) -> Self {
        match &mut self.otp {
            OtpButton::OneTap { autofill_text, .. } | OtpButton::ZeroTap { autofill_text, .. } => {
                *autofill_text = Some(label.into());
            }
            OtpButton::CopyCode { .. } => {}
        }
        self
    }

    /// The `components` array.
    pub fn components(&self) -> Vec<TemplateComponent> {
        components(
            self.add_security_recommendation,
            self.code_expiration_minutes,
            &self.otp,
        )
    }

    /// As a generic [`TemplateDefinition`] (category `AUTHENTICATION`).
    pub fn to_definition(&self) -> TemplateDefinition {
        let mut d = TemplateDefinition::new(
            self.name.clone(),
            self.language.clone(),
            TemplateCategory::Authentication,
        );
        d.message_send_ttl_seconds = self.message_send_ttl_seconds;
        d.components = self.components();
        d
    }

    /// Check the documented limits.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        self.to_definition().validate()
    }
}

fn components(
    add_security_recommendation: Option<bool>,
    code_expiration_minutes: Option<u32>,
    otp: &OtpButton,
) -> Vec<TemplateComponent> {
    let mut v = vec![TemplateComponent::Body(BodyComponent {
        add_security_recommendation,
        ..BodyComponent::default()
    })];
    if code_expiration_minutes.is_some() {
        v.push(TemplateComponent::Footer(FooterComponent {
            text: None,
            code_expiration_minutes,
        }));
    }
    v.push(TemplateComponent::buttons([Button::Otp(otp.clone())]));
    v
}

/// Create or update one authentication template in several languages at
/// once (`POST /{waba}/upsert_message_templates`,
/// `templates/authentication-templates/bulk-management`). Button labels
/// (`text`, `autofill_text`) are not supported by this endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthenticationUpsert {
    /// Template name.
    pub name: String,
    /// Language codes; existing name+language pairs are updated, others
    /// created.
    pub languages: Vec<String>,
    /// The button.
    pub otp: OtpButton,
    /// Append the security recommendation.
    pub add_security_recommendation: Option<bool>,
    /// Expiry footer minutes (1–90).
    pub code_expiration_minutes: Option<u32>,
    /// Message time-to-live (30–900 seconds or `-1`).
    pub message_send_ttl_seconds: Option<i64>,
}

impl AuthenticationUpsert {
    /// Upsert `template` (its `language` is ignored) in `languages`.
    pub fn from_template<I, S>(template: &AuthenticationTemplate, languages: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            name: template.name.clone(),
            languages: languages.into_iter().map(Into::into).collect(),
            otp: template.otp.clone(),
            add_security_recommendation: template.add_security_recommendation,
            code_expiration_minutes: template.code_expiration_minutes,
            message_send_ttl_seconds: template.message_send_ttl_seconds,
        }
    }

    /// Check the documented limits.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        if self.languages.is_empty() {
            return Err(ValidationError::new("languages", "at least one language"));
        }
        for (i, l) in self.languages.iter().enumerate() {
            crate::templates::not_empty(l, &format!("languages[{i}]"))?;
        }
        // bulk-management: "The `text` property is not supported. The
        // `autofill_text` property is not supported."
        let labelled = match &self.otp {
            OtpButton::CopyCode { text } => text.is_some(),
            OtpButton::OneTap {
                text,
                autofill_text,
                ..
            }
            | OtpButton::ZeroTap {
                text,
                autofill_text,
                ..
            } => text.is_some() || autofill_text.is_some(),
        };
        if labelled {
            return Err(ValidationError::new(
                "components.buttons[0]",
                "text and autofill_text are not supported by upsert",
            ));
        }
        let mut d = TemplateDefinition::new(
            self.name.clone(),
            self.languages[0].clone(),
            TemplateCategory::Authentication,
        );
        d.message_send_ttl_seconds = self.message_send_ttl_seconds;
        d.components = components(
            self.add_security_recommendation,
            self.code_expiration_minutes,
            &self.otp,
        );
        d.validate()
    }
}

#[derive(Serialize)]
struct UpsertBody<'a> {
    name: &'a str,
    languages: &'a [String],
    category: TemplateCategory,
    components: Vec<TemplateComponent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_send_ttl_seconds: Option<i64>,
}

/// One template created or updated by an upsert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpsertedTemplate {
    /// Template id.
    pub id: TemplateId,
    /// Status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TemplateStatus>,
    /// Language code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Parameters of `GET /{waba}/message_template_previews`
/// (`templates/authentication-templates/template-preview`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewQuery {
    /// Languages to preview; empty for all supported languages.
    pub languages: Vec<String>,
    /// Include the security recommendation string.
    pub add_security_recommendation: Option<bool>,
    /// Include the expiry footer for this many minutes (1–90).
    pub code_expiration_minutes: Option<u32>,
}

/// One language's preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplatePreview {
    /// Language code.
    pub language: String,
    /// Body text with `{{1}}` for the code.
    pub body: String,
    /// Expiry footer, when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footer: Option<String>,
    /// Button labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<PreviewButton>,
}

/// Button labels of a preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewButton {
    /// Copy-code label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Autofill label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autofill_text: Option<String>,
}

impl Authentication {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaId {
        &self.waba_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Create an authentication template (`POST /{waba}/message_templates`).
    /// Meta turns the OTP button into a `URL` button on creation.
    pub async fn create(&self, template: &AuthenticationTemplate) -> Result<TemplateCreated> {
        self.client
            .templates(self.waba_id.clone())
            .create(&template.to_definition())
            .await
    }

    /// Preview Meta's preset authentication text per language.
    ///
    /// The page's request syntax names the language parameter `language`,
    /// its example `languages`; `languages` is sent. `button_types=OTP` is
    /// always sent (the table marks it required and the only value for
    /// authentication templates).
    pub async fn previews(&self, query: &PreviewQuery) -> Result<Vec<TemplatePreview>> {
        if let Some(minutes) = query.code_expiration_minutes {
            validate::code_expiration_minutes(minutes, "code_expiration_minutes")?;
        }
        let mut req = self
            .client
            .get_at(&[self.waba_id.as_str(), "message_template_previews"])
            .query("category", TemplateCategory::Authentication);
        if !query.languages.is_empty() {
            req = req.query("languages", query.languages.join(","));
        }
        let list: DataList<TemplatePreview> = req
            .query_opt(
                "add_security_recommendation",
                query.add_security_recommendation,
            )
            .query_opt("code_expiration_minutes", query.code_expiration_minutes)
            .query("button_types", "OTP")
            .context("message template previews")
            .send()
            .await?;
        Ok(list.data)
    }

    /// Create or update `upsert.name` in every language of `upsert.languages`.
    /// Not replayed on timeouts: updating an approved template counts
    /// against its edit limit.
    pub async fn upsert(&self, upsert: &AuthenticationUpsert) -> Result<Vec<UpsertedTemplate>> {
        upsert.validate()?;
        let list: DataList<UpsertedTemplate> = self
            .client
            .post_at(&[self.waba_id.as_str(), "upsert_message_templates"])
            .json(&UpsertBody {
                name: &upsert.name,
                languages: &upsert.languages,
                category: TemplateCategory::Authentication,
                components: components(
                    upsert.add_security_recommendation,
                    upsert.code_expiration_minutes,
                    &upsert.otp,
                ),
                message_send_ttl_seconds: upsert.message_send_ttl_seconds,
            })
            .context("upsert message templates response")
            .send()
            .await?;
        Ok(list.data)
    }
}

/// The `template` object that delivers `code` with the authentication
/// template `name`: the code as the body parameter and as the parameter of
/// the button at index 0 (a `url` sub-type: Meta stores OTP buttons as URL
/// buttons). This is the exact payload of the send examples on the
/// copy-code, one-tap and zero-tap pages.
///
/// The code must be at most 15 characters, and URLs, media and emojis are
/// not allowed (`templates/template-categorization`); [`OtpService`] only
/// produces 4–8 digit codes. The returned value's `Debug` output redacts the
/// code.
pub fn otp_template_message(
    name: impl Into<String>,
    language: impl Into<String>,
    code: &str,
) -> TemplateMessage {
    TemplateMessage::new(name, language)
        .body([Parameter::text(code)])
        .url_button(0, code)
}
