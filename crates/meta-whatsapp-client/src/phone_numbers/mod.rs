//! One business phone number: details, request/verify code,
//! register/deregister, two-step verification PIN, local storage and
//! identity-check settings, display name change, per-number webhook
//! override, search visibility, security notifications and number-change
//! notices, conversational components, the coexistence data sync, the
//! business username and the contact book, the Official Business Account
//! request and status, business compliance information (India), and
//! health status; plus bot details ([`Client::waba_bot`]).
//!
//! Docs: `business-phone-numbers/phone-numbers`,
//! `business-phone-numbers/registration`,
//! `business-phone-numbers/two-step-verification`,
//! `business-phone-numbers/conversational-components`, `display-names`,
//! `local-storage`, `webhooks/override`,
//! `solution-providers/registering-phone-numbers`,
//! `embedded-signup/onboarding-business-app-users` (`smb_app_data`),
//! `business-scoped-user-ids` (§ Business usernames, § Contact book),
//! `official-business-accounts`, `support/health-status`,
//! `reference/whatsapp-business-phone-number/{whatsapp-business-account-phone-number-api,
//! phone-number-registration, phone-number-deregister-api,
//! phone-number-verification-request-code-api, verify-code-api, settings-api,
//! whatsapp-business-account-official-business-account-status-api,
//! business-compliance-information-api}`,
//! `reference/whatsapp-business-account/conversational-automation-api`,
//! `reference/whatsapp-business-bot/bot-details-api`.
//!
//! Meta paths: `GET`/`POST /{PHONE_NUMBER_ID}`, `POST …/request_code`,
//! `…/verify_code`, `…/register`, `…/deregister`, `GET`/`POST …/settings`,
//! `…/conversational_automation`, `…/smb_app_data`, `GET`/`POST`/`DELETE
//! …/username`, `GET …/username_suggestions`, `DELETE …/contact_book`,
//! `POST …/official_business_account`, `GET`/`POST
//! …/business_compliance_info`, and `GET /{WABA-Bot-ID}`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! # Lifecycle
//!
//! ```text
//! waba.create_phone_number  (skipped when Embedded Signup added the number)
//!   → request_code(SMS|VOICE) → verify_code(code)   (ES does these too)
//!   → register(pin)                                  (always yours to do)
//!   → … → deregister()
//! ```
//!
//! A number added through Embedded Signup must be registered within 14 days
//! (`solution-providers/manage-phone-numbers`). `register` and `deregister`
//! share a budget of 10 requests per number per 72 hours; the 11th fails with
//! `133016` ([`meta_whatsapp_core::ErrorKind::Registration`], deliberately not a
//! retryable rate limit) and locks the number for 72 hours, so neither is
//! marked idempotent here and neither should be put in a retry loop by
//! callers.
//!
//! Every path is built from segments ([`Client::post_at`] and friends), so a
//! phone number id containing `/` or `..` cannot address another object
//! with the token.
//!
//! # Where Meta's pages disagree
//!
//! - `request_code`/`verify_code`: the guides pass `code_method`, `language`
//!   and `code` as query parameters (one example uses a form field); the
//!   references define a JSON body. This client sends JSON, which keeps the
//!   verification code out of the URL (URLs end up in proxy and transport
//!   error logs).
//! - Identity change setting: guide example `enable_identity_key_check` vs
//!   reference `enabled`; the example is used.
//! - Local storage status casing: see [`StorageStatus`].
//! - Search visibility, security notifications and number-change notices
//!   are fields of the number's own `POST` in the reference
//!   (`PhoneNumberStatusUpdateRequest`: `search_visibility`, and
//!   `whatsapp_business_api_data.show_security_notifications` /
//!   `notify_user_change_number`), not settings; the page has no example,
//!   so the bodies follow its schema, one field per call.
//! - Official Business Account status: the guide reads the number's
//!   `official_business_account` field (with an example), the reference an
//!   edge, `GET /{PHONE_NUMBER_ID}/official_business_account` (without
//!   one); [`PhoneNumber::official_business_account`] follows the guide.
//!   The guide's example answers `NOT_STARTED`, which the reference's list
//!   of statuses lacks; [`ObaStatus`] has both.
//! - Business compliance information: the update takes an uppercase
//!   `entity_type` ([`BusinessEntityType`]), the read's schema a free
//!   string ("e.g., Partnership, Private Limited Company"), kept as a
//!   string in [`BusinessComplianceInfo::entity_type`].
//!
//! # Where the docs stop
//!
//! - Username errors `147001`–`147005` are not classified in
//!   [`meta_whatsapp_core::ErrorKind`] yet (they are `Unknown`, with the
//!   code in [`meta_whatsapp_core::Error::graph`]). The number's `POST`
//!   also lists a `username` field, without an example; the documented
//!   `/username` edge is used instead.
//! - The Official Business Account reference says its endpoint also
//!   withdraws or resubmits an application, but documents no field for
//!   either: not offered.
//! - The compliance and bot details pages have no example at all; their
//!   types follow the schemas. No mirrored page says where a bot id
//!   ([`meta_whatsapp_core::ids::WabaBotId`]) comes from.
//! - `health_status` is documented on numbers, WABAs and templates only:
//!   a business portfolio's status is its [`HealthEntityType::Business`]
//!   entry in any of them ([`HealthStatus::entity`]).
//!
//! Calling settings share `/{PHONE_NUMBER_ID}/settings` but are typed in
//! [`crate::calling`]; payload encryption is not wrapped yet.

mod automation;
mod bot;
mod compliance;
mod official;
mod secrets;
mod settings;
mod types;
mod username;

pub use automation::{
    BotCommand, ConversationalAutomation, ConversationalAutomationConfig,
    MAX_COMMAND_DESCRIPTION_CHARS, MAX_COMMAND_NAME_CHARS, MAX_COMMANDS, MAX_PROMPT_CHARS,
    MAX_PROMPTS,
};
pub use bot::{WabaBot, WabaBotInfo};
pub use compliance::{
    BusinessComplianceInfo, BusinessEntityType, ComplianceInfoUpdate, CustomerCare,
    GrievanceOfficer, MAX_ENTITY_NAME_CHARS, MIN_ENTITY_NAME_CHARS,
};
pub use official::{
    MAX_SUPPORTING_LINKS, MIN_SUPPORTING_LINKS, ObaApplication, ObaApplicationResponse, ObaStatus,
    OfficialBusinessAccount,
};
pub use secrets::{TwoStepPin, VerificationCode};
pub use settings::{
    DataLocalizationRegion, PhoneNumberSettings, StorageConfiguration, StorageStatus,
};
pub use types::{
    AccountMode, CodeVerificationStatus, CreatedPhoneNumber, MessagingLimitTier, NameStatus,
    PhoneNumberInfo, PhoneNumberStatus, PlatformType, QualityRating, SearchVisibility, Throughput,
    WebhookConfiguration,
};
pub use username::{
    BusinessUsername, MAX_USERNAME_CHARS, MIN_USERNAME_CHARS, TransferAction, UsernameStatus,
    validate_username,
};

/// Messaging health of a number: the same types WABAs and templates use
/// (see [`crate::common`]).
pub use crate::common::{HealthEntity, HealthEntityType, HealthError, HealthState, HealthStatus};

use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::PhoneNumberId;
use serde::{Deserialize, Serialize};

use crate::Client;
use crate::waba::CallbackOverride;

use settings::{
    IdentityChange, IdentitySettingsRequest, StorageConfigurationRequest, StorageSettingsRequest,
};

/// Entry point, see [`Client::phone_number`].
#[derive(Debug, Clone)]
pub struct PhoneNumber {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`PhoneNumber`] API for `phone_number_id`.
    ///
    /// Also works for a pre-verified phone number id
    /// (`embedded-signup/pre-verified-numbers`): its `request_code` and
    /// `verify_code` endpoints have the same shape.
    pub fn phone_number(&self, phone_number_id: impl Into<PhoneNumberId>) -> PhoneNumber {
        PhoneNumber {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

/// How the verification code is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CodeMethod {
    /// Text message.
    Sms,
    /// Voice call.
    Voice,
}

/// Which WhatsApp Business app data a coexistence sync requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmbSyncType {
    /// Contacts, delivered as `smb_app_state_sync` webhooks.
    SmbAppStateSync,
    /// Message history, delivered as `history` webhooks (or one `history`
    /// webhook with error `2593109` if the business declined to share).
    History,
}

/// Response of `POST /{PHONE_NUMBER_ID}/smb_app_data`. It only confirms the
/// request was accepted; the data arrives by webhook.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SmbSyncResponse {
    /// Always `whatsapp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messaging_product: Option<String>,
    /// Keep it: Meta support asks for it.
    pub request_id: String,
}

#[derive(Serialize)]
struct RequestCodeBody<'a> {
    code_method: CodeMethod,
    language: &'a str,
}

#[derive(Serialize)]
struct VerifyCodeBody<'a> {
    code: &'a str,
}

#[derive(Serialize)]
struct RegisterBody<'a> {
    messaging_product: &'static str,
    pin: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    data_localization_region: Option<&'a DataLocalizationRegion>,
}

#[derive(Serialize)]
struct PinBody<'a> {
    pin: &'a str,
}

#[derive(Serialize)]
struct WebhookConfigurationBody<'a> {
    webhook_configuration: WebhookOverrideBody<'a>,
}

#[derive(Serialize)]
struct WebhookOverrideBody<'a> {
    override_callback_uri: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    verify_token: Option<&'a str>,
}

#[derive(Serialize)]
struct SmbSyncBody {
    messaging_product: &'static str,
    sync_type: SmbSyncType,
}

#[derive(Serialize)]
struct SearchVisibilityBody<'a> {
    search_visibility: &'a SearchVisibility,
}

/// `whatsapp_business_api_data` of the number's `POST`
/// (`WhatsAppBusinessApiData`). Its `pin` is not sent from here:
/// [`PhoneNumber::set_two_step_pin`] sets the PIN.
#[derive(Serialize)]
struct BusinessApiDataBody {
    whatsapp_business_api_data: BusinessApiData,
}

#[derive(Serialize)]
struct BusinessApiData {
    #[serde(skip_serializing_if = "Option::is_none")]
    show_security_notifications: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notify_user_change_number: Option<bool>,
}

#[derive(Deserialize)]
struct HealthEnvelope {
    health_status: HealthStatus,
}

impl PhoneNumber {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `GET /{PHONE_NUMBER_ID}` with `fields` (empty = Meta's defaults:
    /// id, display number, verified name, quality rating).
    ///
    /// Field names are passed through as-is, e.g. `status`, `platform_type`,
    /// `is_on_biz_app`, `throughput`, `name_status`,
    /// `code_verification_status`, `webhook_configuration`.
    pub async fn get(&self, fields: &[&str]) -> Result<PhoneNumberInfo> {
        self.client
            .get_at(&[self.phone_number_id.as_str()])
            .query_opt("fields", fields_param(fields))
            .context("phone number")
            .send()
            .await
    }

    /// `POST /{PHONE_NUMBER_ID}/request_code`: have a verification code sent
    /// by SMS or voice, in `language` (e.g. `en_US`).
    ///
    /// Fails with `136024` if the number is already verified; check
    /// `code_verification_status` first.
    pub async fn request_code(&self, method: CodeMethod, language: &str) -> Result<()> {
        if language.trim().is_empty() {
            return Err(ValidationError::new("language", "must not be empty").into());
        }
        self.client
            .post_at(&[self.phone_number_id.as_str(), "request_code"])
            .json(&RequestCodeBody {
                code_method: method,
                language,
            })
            .context("request code response")
            .send_success()
            .await
    }

    /// `POST /{PHONE_NUMBER_ID}/verify_code`: submit the code received.
    pub async fn verify_code(&self, code: &VerificationCode) -> Result<()> {
        self.client
            .post_at(&[self.phone_number_id.as_str(), "verify_code"])
            .json(&VerifyCodeBody {
                code: code.expose_secret(),
            })
            .context("verify code response")
            .send_success()
            .await
    }

    /// `POST /{PHONE_NUMBER_ID}/register`: register the (verified) number for
    /// Cloud API, setting (or proving) its two-step verification PIN.
    ///
    /// If the number already has two-step verification, `pin` must be the
    /// existing PIN (wrong PIN: `133005`,
    /// [`meta_whatsapp_core::ErrorKind::TwoStepVerification`]); otherwise it becomes the
    /// PIN. `data_localization_region` turns on local storage; it can only
    /// be changed by deregistering and registering again.
    ///
    /// Not replayed on transient errors: see the module docs on the
    /// 10-per-72-hours budget. Re-registering after a display name approval
    /// is documented and expected (`display-names`).
    pub async fn register(
        &self,
        pin: &TwoStepPin,
        data_localization_region: Option<&DataLocalizationRegion>,
    ) -> Result<()> {
        if let Some(region) = data_localization_region {
            region.validate()?;
        }
        self.client
            .post_at(&[self.phone_number_id.as_str(), "register"])
            .json(&RegisterBody {
                messaging_product: "whatsapp",
                pin: pin.expose_secret(),
                data_localization_region,
            })
            .context("register response")
            .send_success()
            .await
    }

    /// `POST /{PHONE_NUMBER_ID}/deregister`. Also turns local storage off.
    /// Not available for numbers in coexistence (Cloud API + WhatsApp
    /// Business app); those disconnect from the app.
    pub async fn deregister(&self) -> Result<()> {
        self.client
            .post_at(&[self.phone_number_id.as_str(), "deregister"])
            .context("deregister response")
            .send_success()
            .await
    }

    /// `POST /{PHONE_NUMBER_ID}` `{"pin": …}`: set a new two-step
    /// verification PIN. There is no API to turn two-step verification off.
    pub async fn set_two_step_pin(&self, pin: &TwoStepPin) -> Result<()> {
        self.client
            .post_at(&[self.phone_number_id.as_str()])
            .json(&PinBody {
                pin: pin.expose_secret(),
            })
            // Setting a value: replaying it cannot duplicate an effect.
            .idempotent(true)
            .context("two-step verification response")
            .send_success()
            .await
    }

    /// `POST /{PHONE_NUMBER_ID}?new_display_name=…`: request a display name
    /// change. The new name goes through review (`new_name_status`,
    /// `phone_number_name_update` webhook); once approved, [`Self::register`]
    /// again to apply it.
    pub async fn request_display_name_change(&self, new_display_name: &str) -> Result<()> {
        if new_display_name.trim().is_empty() {
            return Err(ValidationError::new("new_display_name", "must not be empty").into());
        }
        self.client
            .post_at(&[self.phone_number_id.as_str()])
            .query("new_display_name", new_display_name)
            .context("display name change response")
            .send_success()
            .await
    }

    /// `POST /{PHONE_NUMBER_ID}` `{"webhook_configuration": …}`: send this
    /// number's supported webhooks (`messages`, `smb_message_echoes`,
    /// `history`, … — not account or template webhooks) to another callback.
    /// The app must already be subscribed to the number's WABA.
    pub async fn set_webhook_override(&self, callback: &CallbackOverride) -> Result<()> {
        callback.validate()?;
        self.client
            .post_at(&[self.phone_number_id.as_str()])
            .json(&WebhookConfigurationBody {
                webhook_configuration: WebhookOverrideBody {
                    override_callback_uri: &callback.override_callback_uri,
                    verify_token: Some(callback.verify_token.expose_secret()),
                },
            })
            .idempotent(true)
            .context("webhook override response")
            .send_success()
            .await
    }

    /// Remove this number's callback override (Meta: set
    /// `override_callback_uri` to an empty string). Webhooks fall back to the
    /// WABA override, then the app's callback.
    pub async fn clear_webhook_override(&self) -> Result<()> {
        self.client
            .post_at(&[self.phone_number_id.as_str()])
            .json(&WebhookConfigurationBody {
                webhook_configuration: WebhookOverrideBody {
                    override_callback_uri: "",
                    verify_token: None,
                },
            })
            .idempotent(true)
            .context("webhook override response")
            .send_success()
            .await
    }

    /// `POST /{PHONE_NUMBER_ID}` `{"search_visibility": …}`: show or hide
    /// the number in WhatsApp's business search. A value is set, so the
    /// request is replayed on transient errors.
    pub async fn set_search_visibility(&self, visibility: &SearchVisibility) -> Result<()> {
        self.post_on_number(
            &SearchVisibilityBody {
                search_visibility: visibility,
            },
            "search visibility response",
        )
        .await
    }

    /// `POST /{PHONE_NUMBER_ID}`
    /// `{"whatsapp_business_api_data": {"show_security_notifications": …}}`:
    /// whether to show security notifications (Meta's reference says no
    /// more than that). A value is set, so the request is replayed on
    /// transient errors.
    pub async fn set_security_notifications(&self, show: bool) -> Result<()> {
        self.post_on_number(
            &BusinessApiDataBody {
                whatsapp_business_api_data: BusinessApiData {
                    show_security_notifications: Some(show),
                    notify_user_change_number: None,
                },
            },
            "security notifications response",
        )
        .await
    }

    /// `POST /{PHONE_NUMBER_ID}`
    /// `{"whatsapp_business_api_data": {"notify_user_change_number": …}}`:
    /// whether users are notified when the business changes its number
    /// (Meta's reference says no more than that). A value is set, so the
    /// request is replayed on transient errors.
    pub async fn set_notify_user_change_number(&self, notify: bool) -> Result<()> {
        self.post_on_number(
            &BusinessApiDataBody {
                whatsapp_business_api_data: BusinessApiData {
                    show_security_notifications: None,
                    notify_user_change_number: Some(notify),
                },
            },
            "number change notice response",
        )
        .await
    }

    async fn post_on_number(&self, body: &impl Serialize, context: &'static str) -> Result<()> {
        self.client
            .post_at(&[self.phone_number_id.as_str()])
            .json(body)
            // Setting a value: replaying it cannot duplicate an effect.
            .idempotent(true)
            .context(context)
            .send_success()
            .await
    }

    /// `GET /{PHONE_NUMBER_ID}?fields=health_status`: whether messages can
    /// be sent (and calls received over SIP) through this number, with the
    /// status of the number, its WABA, its business portfolio and the app
    /// (`support/health-status`).
    pub async fn health_status(&self) -> Result<HealthStatus> {
        let env: HealthEnvelope = self
            .client
            .get_at(&[self.phone_number_id.as_str()])
            .query("fields", "health_status")
            .context("phone number health status")
            .send()
            .await?;
        Ok(env.health_status)
    }

    /// `GET /{PHONE_NUMBER_ID}/settings`.
    pub async fn settings(&self) -> Result<PhoneNumberSettings> {
        self.client
            .get_at(&[self.phone_number_id.as_str(), "settings"])
            .context("phone number settings")
            .send()
            .await
    }

    /// Turn local storage on (data at rest kept in `region`). Only possible
    /// while the number is **not** registered: deregister, enable, register
    /// again (`local-storage`).
    pub async fn enable_local_storage(&self, region: &DataLocalizationRegion) -> Result<()> {
        region.validate()?;
        self.post_settings(&StorageSettingsRequest {
            storage_configuration: StorageConfigurationRequest {
                status: StorageStatus::InCountryStorageEnabled,
                data_localization_region: Some(region),
            },
        })
        .await
    }

    /// Turn local storage off (number must be unregistered, as for enabling).
    pub async fn disable_local_storage(&self) -> Result<()> {
        self.post_settings(&StorageSettingsRequest {
            storage_configuration: StorageConfigurationRequest {
                status: StorageStatus::InCountryStorageDisabled,
                data_localization_region: None,
            },
        })
        .await
    }

    /// Turn the identity change check on or off. When on, webhooks carry the
    /// user's `identity_key_hash`, and a send with a stale
    /// `recipient_identity_key_hash` fails with `137000` instead of reaching
    /// a possibly different person.
    pub async fn set_identity_key_check(&self, enabled: bool) -> Result<()> {
        self.post_settings(&IdentitySettingsRequest {
            user_identity_change: IdentityChange {
                enable_identity_key_check: enabled,
            },
        })
        .await
    }

    async fn post_settings(&self, body: &impl Serialize) -> Result<()> {
        self.client
            .post_at(&[self.phone_number_id.as_str(), "settings"])
            .json(body)
            // Settings are set to a value: safe to replay.
            .idempotent(true)
            .context("phone number settings response")
            .send_success()
            .await
    }

    /// Current conversational components
    /// (`GET /{PHONE_NUMBER_ID}?fields=conversational_automation`). Empty if
    /// none are configured.
    pub async fn conversational_automation(&self) -> Result<ConversationalAutomation> {
        let env: automation::AutomationEnvelope = self
            .client
            .get_at(&[self.phone_number_id.as_str()])
            .query("fields", "conversational_automation")
            .context("conversational automation")
            .send()
            .await?;
        Ok(env.conversational_automation.unwrap_or_default())
    }

    /// `POST /{PHONE_NUMBER_ID}/conversational_automation`: set the welcome
    /// message flag, ice breakers and/or commands. Limits are checked first
    /// (see [`ConversationalAutomationConfig::validate`]).
    pub async fn configure_conversational_automation(
        &self,
        config: &ConversationalAutomationConfig,
    ) -> Result<()> {
        config.validate()?;
        self.client
            .post_at(&[self.phone_number_id.as_str(), "conversational_automation"])
            .json(config)
            .idempotent(true)
            .context("conversational automation response")
            .send_success()
            .await
    }

    /// `POST /{PHONE_NUMBER_ID}/smb_app_data`: start the coexistence sync of
    /// the WhatsApp Business app's contacts or message history.
    ///
    /// Each sync type can be requested **once**, within 24 hours of
    /// onboarding; after that the business must offboard and go through
    /// Embedded Signup again. Meta refuses a second or late request with
    /// `2593107`/`2593108` ([`meta_whatsapp_core::ErrorKind::SyncNotAllowed`]). For that
    /// reason this is never replayed automatically: a timeout here must be
    /// resolved by looking at the webhooks, not by asking again.
    pub async fn sync_smb_app_data(&self, sync_type: SmbSyncType) -> Result<SmbSyncResponse> {
        self.client
            .post_at(&[self.phone_number_id.as_str(), "smb_app_data"])
            .json(&SmbSyncBody {
                messaging_product: "whatsapp",
                sync_type,
            })
            .context("smb_app_data response")
            .send()
            .await
    }
}

/// `fields=a,b,c`, or `None` for Meta's defaults.
pub(crate) fn fields_param(fields: &[&str]) -> Option<String> {
    (!fields.is_empty()).then(|| fields.join(","))
}

#[cfg(test)]
mod tests {
    use http::Method;
    use meta_whatsapp_core::ErrorKind;
    use meta_whatsapp_core::testing::{RecordedBody, ScriptedTransport};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::RetryPolicy;

    const ID: &str = "106540352242922";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn graph_error(code: i64) -> serde_json::Value {
        json!({"error": {"message": format!("(#{code}) x"), "type": "OAuthException", "code": code, "fbtrace_id": "A"}})
    }

    #[tokio::test]
    async fn get_passes_fields_and_parses_status_example() {
        // business-phone-numbers/phone-numbers, "Getting status via API".
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"status": "CONNECTED", "id": ID}));
        let info = client(&t).phone_number(ID).get(&["status"]).await.unwrap();
        assert_eq!(info.status, Some(PhoneNumberStatus::Connected));
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/106540352242922");
        assert_eq!(req.query("fields").as_deref(), Some("status"));
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(t.remaining(), 0);

        t.push_json(200, json!({"id": ID}));
        client(&t).phone_number(ID).get(&[]).await.unwrap();
        assert_eq!(t.last_request().unwrap().query("fields"), None);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn request_code_sends_method_and_language() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .phone_number("110200345501442")
            .request_code(CodeMethod::Sms, "en_US")
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/110200345501442/request_code");
        assert_eq!(
            req.json(),
            Some(json!({"code_method": "SMS", "language": "en_US"}))
        );
        assert_eq!(t.remaining(), 0);

        let err = client(&t)
            .phone_number(ID)
            .request_code(CodeMethod::Voice, " ")
            .await
            .unwrap_err();
        assert!(matches!(err, meta_whatsapp_core::Error::Validation(_)));
        assert_eq!(t.requests().len(), 1, "no request for invalid input");
    }

    #[tokio::test]
    async fn verify_code_keeps_the_code_out_of_the_url() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        let code = VerificationCode::new("123-830").unwrap();
        client(&t)
            .phone_number("110200345501442")
            .verify_code(&code)
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.path(), "/v25.0/110200345501442/verify_code");
        assert_eq!(req.url.query(), None);
        assert_eq!(req.json(), Some(json!({"code": "123830"})));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn register_with_and_without_local_storage() {
        // business-phone-numbers/registration examples.
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        t.push_json(200, json!({"success": true}));
        let pn = client(&t).phone_number(ID);
        let pin = TwoStepPin::new("212834").unwrap();
        pn.register(&pin, None).await.unwrap();
        pn.register(&pin, Some(&DataLocalizationRegion::Ch))
            .await
            .unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::POST);
        assert_eq!(reqs[0].path(), "/v25.0/106540352242922/register");
        assert_eq!(
            reqs[0].json(),
            Some(json!({"messaging_product": "whatsapp", "pin": "212834"}))
        );
        assert_eq!(
            reqs[1].json(),
            Some(
                json!({"messaging_product": "whatsapp", "pin": "212834", "data_localization_region": "CH"})
            )
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn register_rejects_a_malformed_region_and_maps_pin_errors() {
        let t = ScriptedTransport::new();
        let pn = client(&t).phone_number(ID);
        let pin = TwoStepPin::new("212834").unwrap();
        let err = pn
            .register(&pin, Some(&DataLocalizationRegion::Other("ch".into())))
            .await
            .unwrap_err();
        assert!(matches!(err, meta_whatsapp_core::Error::Validation(_)));
        assert!(t.requests().is_empty());

        t.push_json(400, graph_error(133005));
        let err = pn.register(&pin, None).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::TwoStepVerification);
        // 133016 locks the number for 72 hours: a registration problem, not
        // a throttle the retry policy may replay.
        t.push_json(400, graph_error(133016));
        let err = pn.register(&pin, None).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Registration);
        assert!(!err.is_retryable());
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn deregister_posts_without_body() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t).phone_number(ID).deregister().await.unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/106540352242922/deregister");
        assert_eq!(req.body, RecordedBody::Empty);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn set_two_step_pin_posts_on_the_number() {
        // business-phone-numbers/phone-numbers, "Changing your PIN via API".
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .phone_number(ID)
            .set_two_step_pin(&TwoStepPin::new("150954").unwrap())
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/106540352242922");
        assert_eq!(req.json(), Some(json!({"pin": "150954"})));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn display_name_change_uses_the_query_parameter() {
        // display-names, "Update display name via API".
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        client(&t)
            .phone_number(ID)
            .request_display_name_change("Lucky Shrub")
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/106540352242922");
        assert_eq!(
            req.query("new_display_name").as_deref(),
            Some("Lucky Shrub")
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn webhook_override_set_and_clear() {
        // webhooks/override, phone number examples.
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        t.push_json(200, json!({"success": true}));
        let pn = client(&t).phone_number(ID);
        pn.set_webhook_override(&CallbackOverride::new(
            "https://my-phone-alternate-callback.com/webhook",
            "myvoiceismypassport?",
        ))
        .await
        .unwrap();
        pn.clear_webhook_override().await.unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].path(), "/v25.0/106540352242922");
        assert_eq!(
            reqs[0].json(),
            Some(json!({"webhook_configuration": {
                "override_callback_uri": "https://my-phone-alternate-callback.com/webhook",
                "verify_token": "myvoiceismypassport?"
            }}))
        );
        assert_eq!(
            reqs[1].json(),
            Some(json!({"webhook_configuration": {"override_callback_uri": ""}}))
        );
        assert_eq!(t.remaining(), 0);

        let long = CallbackOverride::new(format!("https://x.example/{}", "a".repeat(200)), "t");
        assert!(pn.set_webhook_override(&long).await.is_err());
        assert_eq!(t.requests().len(), 2);
    }

    #[tokio::test]
    async fn number_fields_are_posted_on_the_number_one_per_call() {
        // reference/whatsapp-business-phone-number/whatsapp-business-account-phone-number-api,
        // `PhoneNumberStatusUpdateRequest` (no example on the page).
        let t = ScriptedTransport::new();
        for _ in 0..5 {
            t.push_json(200, json!({"success": true}));
        }
        let pn = client(&t).phone_number(ID);
        pn.set_search_visibility(&SearchVisibility::Visible)
            .await
            .unwrap();
        pn.set_search_visibility(&SearchVisibility::Hidden)
            .await
            .unwrap();
        pn.set_security_notifications(true).await.unwrap();
        pn.set_notify_user_change_number(true).await.unwrap();
        pn.set_notify_user_change_number(false).await.unwrap();
        let reqs = t.requests();
        for r in &reqs {
            assert_eq!(r.method, Method::POST);
            assert_eq!(r.path(), "/v25.0/106540352242922");
            assert_eq!(r.url.query(), None);
            assert_eq!(r.bearer(), Some("TOKEN"));
        }
        assert_eq!(
            reqs[0].json(),
            Some(json!({"search_visibility": "VISIBLE"}))
        );
        assert_eq!(reqs[1].json(), Some(json!({"search_visibility": "HIDDEN"})));
        assert_eq!(
            reqs[2].json(),
            Some(json!({"whatsapp_business_api_data": {"show_security_notifications": true}}))
        );
        assert_eq!(
            reqs[3].json(),
            Some(json!({"whatsapp_business_api_data": {"notify_user_change_number": true}}))
        );
        assert_eq!(
            reqs[4].json(),
            Some(json!({"whatsapp_business_api_data": {"notify_user_change_number": false}}))
        );
        assert_eq!(t.remaining(), 0);

        t.push_json(200, json!({"success": false}));
        assert!(pn.set_security_notifications(false).await.is_err());
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn number_fields_are_replayed_on_a_transient_error() {
        let t = ScriptedTransport::new();
        t.push_json(500, graph_error(2));
        t.push_json(200, json!({"success": true}));
        let c = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy {
                max_retries: 1,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            })
            .build()
            .unwrap();
        c.phone_number(ID)
            .set_search_visibility(&SearchVisibility::Hidden)
            .await
            .unwrap();
        assert_eq!(t.requests().len(), 2, "setting a value is safe to replay");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn health_status_parses_the_page_examples() {
        // support/health-status, "Example response".
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
              "health_status": {
                "can_send_message": "AVAILABLE",
                "entities": [
                  {"entity_type": "PHONE_NUMBER", "id": "106540352242922", "can_send_message": "AVAILABLE", "can_receive_call_sip": "AVAILABLE"},
                  {"entity_type": "WABA", "id": "102290129340398", "can_send_message": "AVAILABLE"},
                  {"entity_type": "BUSINESS", "id": "506914307656634", "can_send_message": "AVAILABLE"},
                  {"entity_type": "APP", "id": "634974688087057", "can_send_message": "AVAILABLE", "can_receive_call_sip": "AVAILABLE"}
                ]
              },
              "id": "106540352242922"
            }),
        );
        // "Example limited response".
        t.push_json(
            200,
            json!({
              "health_status": {
                "can_send_message": "LIMITED",
                "entities": [
                  {"entity_type": "PHONE_NUMBER", "id": "106540352242922", "can_send_message": "LIMITED", "can_receive_call_sip": "AVAILABLE",
                   "additional_info": ["Your display name has not been approved yet. Your message limit will increase after the display name is approved."]},
                  {"entity_type": "WABA", "id": "102290129340398", "can_send_message": "AVAILABLE"},
                  {"entity_type": "BUSINESS", "id": "506914307656634", "can_send_message": "AVAILABLE"},
                  {"entity_type": "APP", "id": "634974688087057", "can_send_message": "AVAILABLE", "can_receive_call_sip": "AVAILABLE"}
                ]
              },
              "id": "105154286024403"
            }),
        );
        let pn = client(&t).phone_number(ID);
        let ok = pn.health_status().await.unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/106540352242922");
        assert_eq!(req.query("fields").as_deref(), Some("health_status"));
        assert_eq!(req.url.query_pairs().count(), 1);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(ok.can_send_message, Some(HealthState::Available));
        assert_eq!(ok.entities.len(), 4);
        let number = ok.entity(&HealthEntityType::PhoneNumber).unwrap();
        assert_eq!(number.id, "106540352242922");
        assert_eq!(number.can_receive_call_sip, Some(HealthState::Available));
        assert_eq!(
            ok.entity(&HealthEntityType::Business).unwrap().id,
            "506914307656634"
        );

        let limited = pn.health_status().await.unwrap();
        assert_eq!(limited.can_send_message, Some(HealthState::Limited));
        let number = limited.entity(&HealthEntityType::PhoneNumber).unwrap();
        assert_eq!(number.can_send_message, Some(HealthState::Limited));
        assert_eq!(number.additional_info.len(), 1);
        assert!(number.additional_info[0].starts_with("Your display name"));
        assert!(number.errors.is_empty());
        assert_eq!(t.remaining(), 0);

        // As a field of `get`, too.
        t.push_json(
            200,
            json!({"health_status": {"can_send_message": "BLOCKED"}, "id": ID}),
        );
        let info = pn.get(&["health_status"]).await.unwrap();
        assert_eq!(
            info.health_status.and_then(|h| h.can_send_message),
            Some(HealthState::Blocked)
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn local_storage_and_identity_settings_bodies() {
        let t = ScriptedTransport::new();
        for _ in 0..3 {
            t.push_json(200, json!({"success": true}));
        }
        let pn = client(&t).phone_number(ID);
        pn.enable_local_storage(&DataLocalizationRegion::Br)
            .await
            .unwrap();
        pn.disable_local_storage().await.unwrap();
        pn.set_identity_key_check(true).await.unwrap();
        let reqs = t.requests();
        for r in &reqs {
            assert_eq!(r.method, Method::POST);
            assert_eq!(r.path(), "/v25.0/106540352242922/settings");
        }
        // local-storage examples.
        assert_eq!(
            reqs[0].json(),
            Some(
                json!({"storage_configuration": {"status": "IN_COUNTRY_STORAGE_ENABLED", "data_localization_region": "BR"}})
            )
        );
        assert_eq!(
            reqs[1].json(),
            Some(json!({"storage_configuration": {"status": "IN_COUNTRY_STORAGE_DISABLED"}}))
        );
        // business-phone-numbers/phone-numbers, "Identity change check".
        assert_eq!(
            reqs[2].json(),
            Some(json!({"user_identity_change": {"enable_identity_key_check": true}}))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn settings_get_parses() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"storage_configuration": {"status": "IN_COUNTRY_STORAGE_ENABLED", "data_localization_region": "BR"}}),
        );
        let s = client(&t)
            .phone_number("179776755229976")
            .settings()
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().path(),
            "/v25.0/179776755229976/settings"
        );
        assert_eq!(
            s.storage_configuration.unwrap().data_localization_region,
            Some(DataLocalizationRegion::Br)
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn conversational_automation_round_trip() {
        // business-phone-numbers/conversational-components samples.
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        t.push_json(
            200,
            json!({
              "conversational_automation": {
                "prompts": ["Find the best hotels in the area", "Find deals on rental cars"],
                "commands": [
                  {"command_name": "tickets", "command_description": "Book flight tickets"},
                  {"command_name": "hotel", "command_description": "Book hotel"}
                ]
              },
              "id": "123456"
            }),
        );
        let pn = client(&t).phone_number("123456");
        pn.configure_conversational_automation(
            &ConversationalAutomationConfig::new()
                .commands([
                    BotCommand::new("tickets", "Book flight tickets"),
                    BotCommand::new("hotel", "Book hotel"),
                ])
                .prompts(["Book a flight", "plan a vacation"]),
        )
        .await
        .unwrap();
        let current = pn.conversational_automation().await.unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].path(), "/v25.0/123456/conversational_automation");
        assert_eq!(
            reqs[0].json(),
            Some(json!({
              "commands": [
                {"command_name": "tickets", "command_description": "Book flight tickets"},
                {"command_name": "hotel", "command_description": "Book hotel"}
              ],
              "prompts": ["Book a flight", "plan a vacation"]
            }))
        );
        assert_eq!(reqs[1].method, Method::GET);
        assert_eq!(reqs[1].path(), "/v25.0/123456");
        assert_eq!(
            reqs[1].query("fields").as_deref(),
            Some("conversational_automation")
        );
        assert_eq!(current.prompts.len(), 2);
        assert_eq!(current.commands[1].command_name, "hotel");
        assert_eq!(t.remaining(), 0);

        let err = pn
            .configure_conversational_automation(
                &ConversationalAutomationConfig::new().prompts(["a"; 5]),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, meta_whatsapp_core::Error::Validation(_)));
        assert_eq!(t.requests().len(), 2);
    }

    #[tokio::test]
    async fn smb_app_data_sync_requests_and_errors() {
        // embedded-signup/onboarding-business-app-users, steps 1 and 2.
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"messaging_product": "whatsapp", "request_id": "R1"}),
        );
        t.push_json(
            200,
            json!({"messaging_product": "whatsapp", "request_id": "R2"}),
        );
        let pn = client(&t).phone_number(ID);
        let a = pn
            .sync_smb_app_data(SmbSyncType::SmbAppStateSync)
            .await
            .unwrap();
        let b = pn.sync_smb_app_data(SmbSyncType::History).await.unwrap();
        assert_eq!(a.request_id, "R1");
        assert_eq!(b.request_id, "R2");
        let reqs = t.requests();
        assert_eq!(reqs[0].path(), "/v25.0/106540352242922/smb_app_data");
        assert_eq!(
            reqs[0].json(),
            Some(json!({"messaging_product": "whatsapp", "sync_type": "smb_app_state_sync"}))
        );
        assert_eq!(
            reqs[1].json(),
            Some(json!({"messaging_product": "whatsapp", "sync_type": "history"}))
        );
        assert_eq!(t.remaining(), 0);

        t.push_json(400, graph_error(2593107));
        let err = pn
            .sync_smb_app_data(SmbSyncType::History)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::SyncNotAllowed);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn smb_sync_is_not_replayed_after_a_timeout() {
        let t = ScriptedTransport::new();
        t.push_error(|| meta_whatsapp_core::error::TransportError::Timeout);
        let c = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy {
                max_retries: 3,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            })
            .build()
            .unwrap();
        assert!(
            c.phone_number(ID)
                .sync_smb_app_data(SmbSyncType::History)
                .await
                .is_err()
        );
        assert_eq!(t.requests().len(), 1);
    }

    /// The new calls build their paths from segments too.
    #[tokio::test]
    async fn new_calls_keep_the_id_in_one_segment() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"status": "approved"}));
        t.push_json(
            200,
            json!({"messaging_product": "whatsapp", "success": true, "deleted": true}),
        );
        t.push_json(200, json!({"success": true}));
        t.push_json(200, json!({"data": []}));
        let pn = client(&t).phone_number("OTHER/contact_book");
        pn.set_username("lucky_shrub", None).await.unwrap();
        pn.delete_contact_book_entry(&meta_whatsapp_core::ids::UserId::new(
            "US.13491208655302741918",
        ))
        .await
        .unwrap();
        pn.set_search_visibility(&SearchVisibility::Visible)
            .await
            .unwrap();
        pn.business_compliance_info(&[]).await.unwrap();
        let paths: Vec<String> = t.requests().iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(
            paths,
            vec![
                "/v25.0/OTHER%2Fcontact_book/username",
                "/v25.0/OTHER%2Fcontact_book/contact_book",
                "/v25.0/OTHER%2Fcontact_book",
                "/v25.0/OTHER%2Fcontact_book/business_compliance_info",
            ]
        );
        assert!(client(&t).phone_number("..").health_status().await.is_err());
        assert_eq!(t.requests().len(), 4);
        assert_eq!(t.remaining(), 0);
    }

    /// An id from a database or a webhook must not be able to address a
    /// different Graph object with this number's token.
    #[tokio::test]
    async fn ids_cannot_escape_their_path_segment() {
        let t = ScriptedTransport::new();
        let pin = TwoStepPin::new("212834").unwrap();
        for _ in 0..4 {
            t.push_json(200, json!({"success": true}));
        }
        let c = client(&t);
        // `/` stays inside the segment (percent-encoded) on every path shape.
        c.phone_number("OTHER_WABA/subscribed_apps")
            .register(&pin, None)
            .await
            .unwrap();
        c.phone_number("999/register")
            .set_two_step_pin(&pin)
            .await
            .unwrap();
        c.phone_number("1?fields=x").deregister().await.unwrap();
        c.phone_number("1/settings")
            .set_identity_key_check(true)
            .await
            .unwrap();
        let paths: Vec<String> = t.requests().iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(
            paths,
            vec![
                "/v25.0/OTHER_WABA%2Fsubscribed_apps/register",
                "/v25.0/999%2Fregister",
                "/v25.0/1%3Ffields=x/deregister",
                "/v25.0/1%2Fsettings/settings",
            ]
        );
        assert_eq!(t.requests()[2].url.query(), None);
        // `.` and `..` are refused before anything is sent.
        for id in ["..", ".", ""] {
            let err = c.phone_number(id).register(&pin, None).await.unwrap_err();
            assert!(
                matches!(err, meta_whatsapp_core::Error::Validation(_)),
                "{id:?}"
            );
            assert!(c.phone_number(id).get(&[]).await.is_err());
        }
        assert_eq!(t.requests().len(), 4, "invalid ids never reach the wire");
        assert_eq!(t.remaining(), 0);
    }
}
