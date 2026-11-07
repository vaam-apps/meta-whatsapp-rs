//! A number's settings (docs/design/server.md, section 4.2; roadmap
//! M5c3), as services an API adapter calls, with the tenant's WABA token,
//! over the library's `PhoneNumber`:
//!
//! | Operation | Graph call |
//! | --- | --- |
//! | [`NumberSettings::settings`] | `GET /{pn}/settings` (the key material dropped) |
//! | [`NumberSettings::change`] | one of `POST /{pn}/settings` (identity check, local storage), `POST /{pn}` (search visibility, `whatsapp_business_api_data`) |
//! | [`NumberSettings::enable_payload_encryption`], [`NumberSettings::disable_payload_encryption`] | `POST /{pn}/settings` `{"payload_encryption": …}`, never replayed after a timeout |
//! | [`NumberSettings::username`], [`NumberSettings::username_suggestions`], [`NumberSettings::set_username`], [`NumberSettings::delete_username`] | `GET`, `GET …/username_suggestions`, `POST`, `DELETE /{pn}/username` |
//! | [`NumberSettings::official_business_account`], [`NumberSettings::request_official_business_account`] | `GET /{pn}?fields=official_business_account`, `POST /{pn}/official_business_account` |
//! | [`NumberSettings::compliance_info`], [`NumberSettings::set_compliance_info`] | `GET`, `POST /{pn}/business_compliance_info` |
//! | [`NumberSettings::health_status`] | `GET /{pn}?fields=health_status` |
//! | [`NumberSettings::delete_contact_book_entry`] | `DELETE /{pn}/contact_book` |
//! | [`NumberSettings::conversational_components`], [`NumberSettings::set_conversational_components`] | `GET /{pn}?fields=conversational_automation`, `POST /{pn}/conversational_automation` |
//!
//! **One setting per change.** The library sets one value per Graph call
//! and Meta's pages document no combined call: [`SettingChange`] is one
//! setting, so a change either went through or did not, and the audit
//! names which. Local storage can be changed only while the number is not
//! registered (Meta's page `local-storage`): Meta's refusal is passed on.
//!
//! **Key material.** A payload encryption key is handled as a secret: the
//! business's key is read from the request into a [`ClientKey`] (its
//! `Debug` shows a length), checked by the library (base64, not an obvious
//! private key), sent in the body only; Meta's texts of a failed call are
//! in no log and no answer; the settings read drops Meta's
//! `cloud_encryption_key` and answers the status and the key's
//! fingerprint only.
//!
//! **Legal acts** (AGENTS.md, Legal acts; design D192). One operation is a
//! legal act, taken only when the deployment enables it
//! (`WA_SERVER_LEGAL_ACTS`, off by default;
//! [`NumberSettings::with_legal_acts`]): the compliance information
//! update (the business's statutory declaration of its legal entity and
//! grievance officer, India). Off, it is `403 feature_not_available` with
//! nothing asked of Meta; on, it is audited. Nothing here takes one
//! automatically. The set is [`is_legal_act`], the one place to add an
//! operation to it. The Official Business Account application is not in
//! it (the library classifies it as an application for review, not a
//! legal act) and nor is the contact book erasure (a data-subject erasure
//! the tenant performs explicitly under the scope `erasure`, audited,
//! never automatic): both are audited and reach Meta with the setting off.
//!
//! **Bounds.** Meta's documented limits are the library's (a username of 3
//! to 35 characters, 5 to 10 supporting links, 2 to 128 characters of
//! entity name, at most 4 ice breakers, 30 commands, …). Where Meta states
//! none, the service's own, named at the constants below.
//!
//! The services open on a capability their own [`Authorizer`] made, and
//! admit it first. A failed Graph call is told to the [`Observer`], a `190`
//! marks the WABA's numbers `reconnect_required`, and Meta's `details` are
//! kept, except on the payload encryption calls.

use std::fmt;

use meta_whatsapp_rs::Error;
use meta_whatsapp_rs::client::common::HealthStatus;
use meta_whatsapp_rs::client::phone_numbers::{
    BusinessComplianceInfo, ClientEncryptionKey, ComplianceInfoUpdate, ConversationalAutomation,
    ConversationalAutomationConfig, DataLocalizationRegion, ObaApplication, SearchVisibility,
    TransferAction, validate_username,
};
use meta_whatsapp_rs::client::{Client, RetryPolicy};
use meta_whatsapp_rs::core::ids::UserId;

use crate::audit::{Outcome, Subject, number_setting_audit};
use crate::authz::{Authorizer, Caller, OwnedNumber};
use crate::display_names::is_invisible_or_bidi;
use crate::error::{ErrorCode, ServiceError};
use crate::numbers::{EMAIL_MAX_CHARS, profile_email, profile_website};
use crate::observe::Observer;
use crate::operation::{AdminOperation, Operation};
use crate::request::wire;

/// Whether `operation` is a legal act: off until the deployment sets
/// `WA_SERVER_LEGAL_ACTS=on` (design D192). The one predicate; adding an
/// operation to the gate is one more arm here.
#[must_use]
pub fn is_legal_act(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::SetComplianceInfo
            | Operation::CreateSolutionMigrationIntent
            | Operation::CreateBillingMigration
            | Operation::ResumeBillingMigration
    )
}

/// [`is_legal_act`] for an operator's operations
/// ([`AdminOperation`]; roadmap M5h, design D231 and M5j, design D250): the
/// max price beta agreement and the four partner acts of the operator (a
/// business verification submission; accepting or rejecting a solution;
/// accepting a deactivation). The same setting gates them, and adding an
/// admin act to the gate is one more arm here.
#[must_use]
pub fn is_legal_admin_act(operation: AdminOperation) -> bool {
    matches!(
        operation,
        AdminOperation::SignMaxPriceAgreement
            | AdminOperation::SubmitBusinessVerification
            | AdminOperation::AcceptPartnerSolution
            | AdminOperation::RejectPartnerSolution
            | AdminOperation::AcceptPartnerSolutionDeactivation
    )
}

/// The client every legal act is sent through: `client` with
/// [`RetryPolicy::NONE`], so that a legal act reaches Meta exactly once.
/// The library's policy replays what it deems safe (an `idempotent`
/// request after a timeout or a `5xx`, any request after a `429`); that is
/// right for a library user and wrong for an act that cannot be undone,
/// so the guarantee is the service's, here, not a marking in the library
/// (design D192). Every path gated by [`is_legal_act`] or
/// [`is_legal_admin_act`] sends through this; the walk
/// `every_legal_act_reaches_meta_once_whatever_the_client_would_retry`
/// (`tests/legal_acts_once.rs`) walks both predicates and fails a path that
/// does not.
#[must_use]
pub(crate) fn legal_client(client: &Client) -> Client {
    client.clone().with_retry(RetryPolicy::NONE)
}

/// The audit action of [`NumberSettings::change`]: a stored name in the
/// logs, pending the owner's confirmation before the first release.
pub const SETTING_CHANGED: &str = "number_setting_changed";
/// The audit action of the payload encryption calls.
pub const PAYLOAD_ENCRYPTION_CHANGED: &str = "payload_encryption_changed";
/// The audit action of [`NumberSettings::set_username`].
pub const USERNAME_SET: &str = "username_set";
/// The audit action of [`NumberSettings::delete_username`].
pub const USERNAME_DELETED: &str = "username_deleted";
/// The audit action of [`NumberSettings::request_official_business_account`].
pub const OBA_REQUESTED: &str = "official_business_account_requested";
/// The audit action of [`NumberSettings::set_compliance_info`].
pub const COMPLIANCE_INFO_SET: &str = "compliance_info_set";
/// The audit action of [`NumberSettings::delete_contact_book_entry`].
pub const CONTACT_BOOK_ENTRY_DELETED: &str = "contact_book_entry_deleted";
/// The audit action of [`NumberSettings::set_conversational_components`].
pub const CONVERSATIONAL_COMPONENTS_SET: &str = "conversational_components_set";

/// The fields [`NumberSettings::compliance_info`] asks Meta for: the
/// ones of the page's `BusinessComplianceInfo`.
pub const COMPLIANCE_FIELDS: [&str; 6] = [
    "entity_name",
    "entity_type",
    "entity_type_custom",
    "is_registered",
    "grievance_officer_details",
    "customer_care_details",
];

/// The longest free text of an application or a declaration the service
/// sends, in characters: the service's own (Meta's pages state none).
pub const TEXT_MAX_CHARS: usize = 1024;
/// The longest short value (a country, a language, a name, a phone
/// number) of an application or a declaration, in characters: the
/// service's own.
pub const SHORT_MAX_CHARS: usize = 128;

/// One setting of a number, set on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingChange {
    /// The identity change check (`POST /{pn}/settings`).
    IdentityKeyCheck(bool),
    /// Local storage: `Some(region)` turns it on, `None` off.
    LocalStorage(Option<String>),
    /// Whether the number shows in WhatsApp's business search: `true`
    /// `VISIBLE`, `false` `HIDDEN`.
    SearchVisible(bool),
    /// Whether security notifications are shown.
    SecurityNotifications(bool),
    /// Whether users are told when the business changes its number.
    NumberChangeNotifications(bool),
}

impl SettingChange {
    /// The setting's name in the audit event and the answer.
    pub fn name(&self) -> &'static str {
        match self {
            Self::IdentityKeyCheck(_) => "identity_key_check",
            Self::LocalStorage(_) => "local_storage",
            Self::SearchVisible(_) => "search_visibility",
            Self::SecurityNotifications(_) => "security_notifications",
            Self::NumberChangeNotifications(_) => "notify_user_change_number",
        }
    }
}

/// A business public key for payload encryption, as the request carried
/// it. `Debug` shows a length; there is no `Display` or `Serialize`.
pub struct ClientKey(String);

impl ClientKey {
    /// Wrap the request's key.
    pub fn new(key: String) -> Self {
        Self(key)
    }
}

impl fmt::Debug for ClientKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClientKey(<{} bytes>)", self.0.len())
    }
}

/// A number's local storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalStorage {
    /// Meta's status (`IN_COUNTRY_STORAGE_ENABLED`, …).
    pub status: Option<String>,
    /// The region, when enabled.
    pub data_localization_region: Option<String>,
}

/// A number's payload encryption: the status and the key's fingerprint,
/// never a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadEncryptionState {
    /// `enabled` or `disabled`.
    pub status: Option<String>,
    /// The fingerprint of the business's key.
    pub client_encryption_key_fingerprint: Option<String>,
}

/// A number's settings, as Meta answers them, without key material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsView {
    /// Local storage.
    pub local_storage: Option<LocalStorage>,
    /// Payload encryption.
    pub payload_encryption: Option<PayloadEncryptionState>,
}

/// A number's business username.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsernameView {
    /// The username; none when the number has none.
    pub username: Option<String>,
    /// `approved` or `reserved`.
    pub status: Option<String>,
}

/// A number's Official Business Account status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObaView {
    /// Where the application stands.
    pub oba_status: Option<String>,
    /// Meta's explanation.
    pub status_message: Option<String>,
}

/// What Meta answered an application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObaRequested {
    /// Meta's description of the result.
    pub message: Option<String>,
    /// The status after the request.
    pub updated_status: Option<ObaView>,
    /// An id to follow the application with.
    pub tracking_id: Option<String>,
}

/// Whether `text` is fit to send as a customer- or Meta-facing text: not
/// blank, at most `max` characters, no control, bidi override or
/// zero-width character.
fn short_text(field: &str, text: &str, max: usize) -> Result<(), ServiceError> {
    if text.trim().is_empty()
        || text.chars().count() > max
        || text
            .chars()
            .any(|c| c.is_control() || is_invisible_or_bidi(c))
    {
        return Err(ServiceError::invalid(field));
    }
    Ok(())
}

/// A phone number as the compliance page takes one ("international"): an
/// optional `+`, then digits, spaces, `-` and parentheses, 5 to 32
/// characters. Meta judges the rest; the shape is the service's own.
fn phone(field: &str, number: &str) -> Result<(), ServiceError> {
    let body = number.strip_prefix('+').unwrap_or(number);
    let ok = (5..=32).contains(&number.chars().count())
        && body
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '-' | '(' | ')'))
        && body.chars().any(|c| c.is_ascii_digit());
    if ok {
        Ok(())
    } else {
        Err(ServiceError::invalid(field))
    }
}

fn email(field: &'static str, address: &str) -> Result<(), ServiceError> {
    if address.is_empty() || address.chars().count() > EMAIL_MAX_CHARS {
        return Err(ServiceError::invalid(field));
    }
    profile_email(address).map_err(|_| ServiceError::invalid(field))
}

/// A link of an application: the profile's website rule, named for its
/// field.
fn link(field: &'static str, url: &str) -> Result<(), ServiceError> {
    profile_website(url).map_err(|_| ServiceError::invalid(field))
}

/// An application, checked: the library's rules (the two required
/// fields, 5 to 10 supporting links) and the service's own (links
/// `http`/`https` of at most [`crate::numbers::WEBSITE_MAX_CHARS`], texts without control
/// or invisible characters, at most [`SHORT_MAX_CHARS`] or
/// [`TEXT_MAX_CHARS`]).
///
/// # Errors
///
/// `422 invalid_request` on the field.
pub fn check_application(application: &ObaApplication) -> Result<(), ServiceError> {
    application
        .validate()
        .map_err(|e| ServiceError::invalid(e.field))?;
    link("business_website_url", &application.business_website_url)?;
    short_text(
        "primary_country_of_operation",
        &application.primary_country_of_operation,
        SHORT_MAX_CHARS,
    )?;
    if let Some(language) = &application.primary_language {
        short_text("primary_language", language, SHORT_MAX_CHARS)?;
    }
    if let Some(parent) = &application.parent_business_or_brand {
        short_text("parent_business_or_brand", parent, SHORT_MAX_CHARS)?;
    }
    for item in application.supporting_links.iter().flatten() {
        link("supporting_links", item)?;
    }
    if let Some(info) = &application.additional_supporting_information {
        short_text("additional_supporting_information", info, TEXT_MAX_CHARS)?;
    }
    Ok(())
}

/// A compliance update, checked: the library's rules, and the service's
/// own for texts, emails and phone numbers.
///
/// # Errors
///
/// `422 invalid_request` on the field.
pub fn check_compliance(update: &ComplianceInfoUpdate) -> Result<(), ServiceError> {
    update
        .validate()
        .map_err(|e| ServiceError::invalid(e.field))?;
    short_text("entity_name", &update.entity_name, SHORT_MAX_CHARS)?;
    if let Some(custom) = &update.entity_type_custom {
        short_text("entity_type_custom", custom, SHORT_MAX_CHARS)?;
    }
    let officer = &update.grievance_officer_details;
    short_text(
        "grievance_officer_details.name",
        officer.name.as_deref().unwrap_or_default(),
        SHORT_MAX_CHARS,
    )?;
    email(
        "grievance_officer_details.email",
        officer.email.as_deref().unwrap_or_default(),
    )?;
    let care = &update.customer_care_details;
    email(
        "customer_care_details.email",
        care.email.as_deref().unwrap_or_default(),
    )?;
    for (field, number) in [
        (
            "grievance_officer_details.mobile_number",
            &officer.mobile_number,
        ),
        (
            "grievance_officer_details.landline_number",
            &officer.landline_number,
        ),
        ("customer_care_details.mobile_number", &care.mobile_number),
        (
            "customer_care_details.landline_number",
            &care.landline_number,
        ),
    ] {
        if let Some(number) = number {
            phone(field, number)?;
        }
    }
    Ok(())
}

/// Conversational components, checked: the library's documented limits
/// (4 ice breakers of 80 characters, 30 commands, …) and no control or
/// invisible character in any text.
///
/// # Errors
///
/// `422 invalid_request` on the field.
pub fn check_components(config: &ConversationalAutomationConfig) -> Result<(), ServiceError> {
    config
        .validate()
        .map_err(|e| ServiceError::invalid(e.field))?;
    let clean = |field: &str, text: &str| {
        if text
            .chars()
            .any(|c| c.is_control() || is_invisible_or_bidi(c))
        {
            Err(ServiceError::invalid(field))
        } else {
            Ok(())
        }
    };
    for (i, prompt) in config.prompts.iter().flatten().enumerate() {
        clean(&format!("prompts[{i}]"), prompt)?;
    }
    for (i, command) in config.commands.iter().flatten().enumerate() {
        clean(
            &format!("commands[{i}].command_name"),
            &command.command_name,
        )?;
        clean(
            &format!("commands[{i}].command_description"),
            &command.command_description,
        )?;
    }
    Ok(())
}

/// The operations on a number's settings the caller's tenant owns. Cheap:
/// a few references.
pub struct NumberSettings<'a> {
    authz: &'a Authorizer,
    caller: &'a Caller,
    number: &'a OwnedNumber,
    observer: &'a dyn Observer,
    legal_acts: bool,
}

impl fmt::Debug for NumberSettings<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NumberSettings")
            .field("number", self.number)
            .field("legal_acts", &self.legal_acts)
            .finish_non_exhaustive()
    }
}

impl<'a> NumberSettings<'a> {
    /// The operations on `number`, once `authz` admits it
    /// ([`Authorizer::admit_number`]) and `caller` (whom a change is
    /// audited for). The legal acts are off until [`Self::with_legal_acts`].
    ///
    /// # Errors
    ///
    /// `number` another [`Authorizer`] made: `403 forbidden`, and nothing
    /// is asked of Meta.
    pub fn new(
        authz: &'a Authorizer,
        caller: &'a Caller,
        number: &'a OwnedNumber,
    ) -> Result<Self, ServiceError> {
        authz.admit_number(number)?;
        Ok(Self {
            authz,
            caller,
            number,
            observer: &(),
            legal_acts: false,
        })
    }

    /// Report failed Graph calls to `observer` (the adapter's metrics).
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }

    /// Whether the deployment enables the legal acts (the setting
    /// `WA_SERVER_LEGAL_ACTS`).
    #[must_use]
    pub fn with_legal_acts(mut self, enabled: bool) -> Self {
        self.legal_acts = enabled;
        self
    }

    fn phone(&self) -> meta_whatsapp_rs::client::phone_numbers::PhoneNumber {
        self.number
            .client()
            .phone_number(self.number.phone_number_id().clone())
    }

    /// `403 feature_not_available` when `operation` is a legal act
    /// ([`is_legal_act`]) and the deployment has not enabled them.
    fn gate(&self, operation: Operation) -> Result<(), ServiceError> {
        if self.legal_acts || !is_legal_act(operation) {
            Ok(())
        } else {
            Err(ServiceError::new(ErrorCode::FEATURE_NOT_AVAILABLE))
        }
    }

    async fn failed(&self, error: &Error, withhold: bool) -> ServiceError {
        let api = if withhold {
            self.number
                .failed_withholding_texts(self.authz, error)
                .await
        } else {
            self.number.failed(self.authz, error).await
        };
        if error.graph().is_some() {
            self.observer.graph_error(api.code());
        }
        if withhold {
            api
        } else {
            api.with_details(error)
        }
    }

    fn audit(&self, action: &'static str, setting: Option<&'static str>, outcome: Outcome) {
        number_setting_audit(
            action,
            self.caller.key_id(),
            &Subject {
                tenant: Some(self.caller.tenant().as_str()),
                waba_id: Some(self.number.waba_id().as_str()),
                phone_number_id: Some(self.number.phone_number_id().as_str()),
                ..Subject::default()
            },
            setting,
            outcome,
        );
    }

    /// A read.
    async fn read<T>(&self, result: Result<T, Error>) -> Result<T, ServiceError> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => Err(self.failed(&error, false).await),
        }
    }

    /// A change: audited when Meta answered, and when it may have gone
    /// through unanswered.
    async fn changed<T>(
        &self,
        action: &'static str,
        setting: Option<&'static str>,
        withhold: bool,
        result: Result<T, Error>,
    ) -> Result<T, ServiceError> {
        match result {
            Ok(value) => {
                self.audit(action, setting, Outcome::Answered);
                Ok(value)
            }
            Err(error) => {
                let failed = self.failed(&error, withhold).await;
                if failed.may_have_been_sent() {
                    self.audit(action, setting, Outcome::Unanswered);
                }
                Err(failed)
            }
        }
    }

    // ─── Settings ────────────────────────────────────────────────────────

    /// `GET /{pn}/settings`, without key material.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn settings(&self) -> Result<SettingsView, ServiceError> {
        let settings = self.phone().settings().await;
        let settings = match settings {
            Ok(settings) => settings,
            // The answer may carry Meta's key: its texts stay out.
            Err(error) => return Err(self.failed(&error, true).await),
        };
        Ok(SettingsView {
            local_storage: settings.storage_configuration.map(|s| LocalStorage {
                status: wire(Some(&s.status)),
                data_localization_region: s.data_localization_region.map(|r| r.as_str().to_owned()),
            }),
            payload_encryption: settings.payload_encryption.map(|p| PayloadEncryptionState {
                status: wire(p.status.as_ref()),
                client_encryption_key_fingerprint: p.client_encryption_key_fingerprint,
            }),
        })
    }

    /// Set one setting. Audited as [`SETTING_CHANGED`] with the setting's
    /// name.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `local_storage` for a region that is not
    /// two uppercase letters, before any request; Meta's refusal or
    /// failure, with its `details`.
    pub async fn change(&self, change: &SettingChange) -> Result<(), ServiceError> {
        let phone = self.phone();
        let result = match change {
            SettingChange::IdentityKeyCheck(on) => phone.set_identity_key_check(*on).await,
            SettingChange::LocalStorage(Some(region)) => {
                let region = DataLocalizationRegion::from_code(region);
                if region.validate().is_err() {
                    return Err(ServiceError::invalid("local_storage"));
                }
                phone.enable_local_storage(&region).await
            }
            SettingChange::LocalStorage(None) => phone.disable_local_storage().await,
            SettingChange::SearchVisible(visible) => {
                let value = if *visible {
                    SearchVisibility::Visible
                } else {
                    SearchVisibility::Hidden
                };
                phone.set_search_visibility(&value).await
            }
            SettingChange::SecurityNotifications(show) => {
                phone.set_security_notifications(*show).await
            }
            SettingChange::NumberChangeNotifications(notify) => {
                phone.set_notify_user_change_number(*notify).await
            }
        };
        self.changed(SETTING_CHANGED, Some(change.name()), false, result)
            .await
    }

    /// Turn payload encryption on with the business's public key. Audited
    /// as [`PAYLOAD_ENCRYPTION_CHANGED`] (`enable`). Not replayed after a
    /// timeout: read [`Self::settings`].
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `client_encryption_key` (not base64, or a
    /// private key), before any request, quoting nothing; Meta's refusal
    /// or failure, without its texts.
    pub async fn enable_payload_encryption(&self, key: ClientKey) -> Result<(), ServiceError> {
        let Ok(key) = ClientEncryptionKey::new(key.0) else {
            return Err(ServiceError::invalid("client_encryption_key"));
        };
        let result = self.phone().enable_payload_encryption(&key).await;
        self.changed(PAYLOAD_ENCRYPTION_CHANGED, Some("enable"), true, result)
            .await
    }

    /// Turn payload encryption off. Audited as
    /// [`PAYLOAD_ENCRYPTION_CHANGED`] (`disable`).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, without its texts.
    pub async fn disable_payload_encryption(&self) -> Result<(), ServiceError> {
        let result = self.phone().disable_payload_encryption().await;
        self.changed(PAYLOAD_ENCRYPTION_CHANGED, Some("disable"), true, result)
            .await
    }

    // ─── Username ────────────────────────────────────────────────────────

    /// The number's business username.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn username(&self) -> Result<UsernameView, ServiceError> {
        let username = self.read(self.phone().username().await).await?;
        Ok(UsernameView {
            username: username.username,
            status: wire(username.status.as_ref()),
        })
    }

    /// The usernames WhatsApp reserved for the business portfolio.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn username_suggestions(&self) -> Result<Vec<String>, ServiceError> {
        self.read(self.phone().username_suggestions().await).await
    }

    /// Adopt or change the username (the library's format, checked first);
    /// `force_transfer` moves it from another number of the portfolio.
    /// Audited as [`USERNAME_SET`]; the status Meta answered.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `username`; Meta's refusal or failure.
    pub async fn set_username(
        &self,
        username: &str,
        force_transfer: bool,
    ) -> Result<Option<String>, ServiceError> {
        if validate_username(username).is_err() {
            return Err(ServiceError::invalid("username"));
        }
        let action = force_transfer.then_some(TransferAction::ForceTransfer);
        let result = self.phone().set_username(username, action.as_ref()).await;
        let status = self.changed(USERNAME_SET, None, false, result).await?;
        Ok(wire(Some(&status)))
    }

    /// Remove the username. Audited as [`USERNAME_DELETED`].
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure.
    pub async fn delete_username(&self) -> Result<(), ServiceError> {
        let result = self.phone().delete_username().await;
        self.changed(USERNAME_DELETED, None, false, result).await
    }

    // ─── Official Business Account ───────────────────────────────────────

    /// The number's Official Business Account status.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn official_business_account(&self) -> Result<ObaView, ServiceError> {
        let oba = self
            .read(self.phone().official_business_account().await)
            .await?;
        Ok(ObaView {
            oba_status: wire(oba.oba_status.as_ref()),
            status_message: oba.status_message,
        })
    }

    /// Apply for Official Business Account status. Not a legal act (an
    /// application for Meta's review): audited as [`OBA_REQUESTED`], not
    /// replayed after a timeout: read [`Self::official_business_account`].
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on the field ([`check_application`]);
    /// Meta's refusal or failure.
    pub async fn request_official_business_account(
        &self,
        application: &ObaApplication,
    ) -> Result<ObaRequested, ServiceError> {
        self.gate(Operation::RequestOfficialBusinessAccount)?;
        check_application(application)?;
        let result = self
            .phone()
            .request_official_business_account(application)
            .await;
        let answer = self.changed(OBA_REQUESTED, None, false, result).await?;
        Ok(ObaRequested {
            message: answer.message,
            updated_status: answer.updated_status.map(|s| ObaView {
                oba_status: wire(s.oba_status.as_ref()),
                status_message: s.status_message,
            }),
            tracking_id: answer.tracking_id,
        })
    }

    // ─── Compliance information ──────────────────────────────────────────

    /// The number's compliance information ([`COMPLIANCE_FIELDS`]); none
    /// when Meta holds none.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn compliance_info(&self) -> Result<Option<BusinessComplianceInfo>, ServiceError> {
        let list = self
            .read(
                self.phone()
                    .business_compliance_info(&COMPLIANCE_FIELDS)
                    .await,
            )
            .await?;
        Ok(list.into_iter().next())
    }

    /// Create or replace the compliance information. A legal act: off
    /// unless [`Self::with_legal_acts`]. Audited as [`COMPLIANCE_INFO_SET`],
    /// never what it says.
    ///
    /// # Errors
    ///
    /// `403 feature_not_available` with the legal acts off, before any
    /// request; `422 invalid_request` on the field ([`check_compliance`]);
    /// Meta's refusal or failure.
    pub async fn set_compliance_info(
        &self,
        update: &ComplianceInfoUpdate,
    ) -> Result<(), ServiceError> {
        self.gate(Operation::SetComplianceInfo)?;
        check_compliance(update)?;
        let result = legal_client(self.number.client())
            .phone_number(self.number.phone_number_id().clone())
            .set_business_compliance_info(update)
            .await;
        self.changed(COMPLIANCE_INFO_SET, None, false, result).await
    }

    // ─── Health ──────────────────────────────────────────────────────────

    /// The number's health status (`fields=health_status`).
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn health_status(&self) -> Result<HealthStatus, ServiceError> {
        self.read(self.phone().health_status().await).await
    }

    // ─── Contact book ────────────────────────────────────────────────────

    /// Erase one user's entry from the portfolio's contact book, at Meta:
    /// **irreversible**. Not gated by the legal acts setting: a
    /// data-subject erasure the tenant performs explicitly (scope
    /// `erasure`). Audited as [`CONTACT_BOOK_ENTRY_DELETED`], never the BSUID. `true`
    /// when an entry existed.
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on `bsuid` (not a BSUID: a phone
    /// number or a parent BSUID is refused), quoting nothing; Meta's
    /// refusal or failure.
    pub async fn delete_contact_book_entry(&self, bsuid: &str) -> Result<bool, ServiceError> {
        self.gate(Operation::DeleteContactBookEntry)?;
        let id = UserId::new(bsuid);
        if !id.is_bsuid() {
            return Err(ServiceError::invalid("bsuid"));
        }
        let result = self.phone().delete_contact_book_entry(&id).await;
        self.changed(CONTACT_BOOK_ENTRY_DELETED, None, false, result)
            .await
    }

    // ─── Conversational components ───────────────────────────────────────

    /// The welcome message flag, ice breakers and commands.
    ///
    /// # Errors
    ///
    /// Meta's refusal or failure, with its `details`.
    pub async fn conversational_components(
        &self,
    ) -> Result<ConversationalAutomation, ServiceError> {
        self.read(self.phone().conversational_automation().await)
            .await
    }

    /// Set the welcome message flag, ice breakers and/or commands
    /// ([`check_components`]). Audited as [`CONVERSATIONAL_COMPONENTS_SET`].
    ///
    /// # Errors
    ///
    /// `422 invalid_request` on the field; Meta's refusal or failure.
    pub async fn set_conversational_components(
        &self,
        config: &ConversationalAutomationConfig,
    ) -> Result<(), ServiceError> {
        check_components(config)?;
        let result = self
            .phone()
            .configure_conversational_automation(config)
            .await;
        self.changed(CONVERSATIONAL_COMPONENTS_SET, None, false, result)
            .await
    }
}

#[cfg(test)]
mod tests {
    use meta_whatsapp_rs::client::phone_numbers::{
        BotCommand, BusinessEntityType, CustomerCare, GrievanceOfficer,
    };

    use super::*;

    /// D192 and D250: the compliance declaration and the three partner acts
    /// of roadmap M5j (a migration intent to a solution, a billing
    /// migration and its resumption) are the legal acts among the
    /// operations; the application and the erasure are not.
    #[test]
    fn only_the_declaration_and_the_partner_acts_are_legal_acts() {
        let gated: Vec<_> = Operation::ALL
            .into_iter()
            .filter(|operation| is_legal_act(*operation))
            .collect();
        assert_eq!(
            gated,
            [
                Operation::SetComplianceInfo,
                Operation::CreateSolutionMigrationIntent,
                Operation::CreateBillingMigration,
                Operation::ResumeBillingMigration,
            ]
        );
    }

    /// D231 and D250: the max price agreement and the four partner acts of
    /// the operator are the admin legal acts, and no
    /// tenant operation of the marketing family is a legal act of either
    /// kind. Decisive: the arm of `is_legal_admin_act`, and a marketing
    /// operation added to `is_legal_act`.
    #[test]
    fn the_admin_legal_acts_are_the_agreement_and_the_partner_acts_and_no_marketing_operation_is_one()
     {
        let gated: Vec<_> = AdminOperation::ALL
            .into_iter()
            .filter(|operation| is_legal_admin_act(*operation))
            .collect();
        assert_eq!(gated, AdminOperation::ALL);
        assert_eq!(
            gated
                .iter()
                .map(|operation| operation.id())
                .collect::<Vec<_>>(),
            [
                "sign_max_price_agreement",
                "submit_business_verification",
                "accept_partner_solution",
                "reject_partner_solution",
                "accept_partner_solution_deactivation",
            ]
        );
        assert_eq!(
            AdminOperation::SignMaxPriceAgreement.id(),
            "sign_max_price_agreement"
        );
        for operation in Operation::ALL {
            if operation.scope() == crate::model::Scope::Marketing {
                assert!(!is_legal_act(operation), "{operation}");
            }
        }
    }

    fn invalid_field<T: fmt::Debug>(result: Result<T, ServiceError>) -> String {
        let error = result.unwrap_err();
        assert_eq!(error.code(), "invalid_request");
        error.field().unwrap().to_owned()
    }

    /// The key's `Debug` shows a length, and the setting names are the
    /// audit's.
    #[test]
    fn a_key_shows_a_length_and_the_settings_are_named() {
        let shown = format!("{:?}", ClientKey::new("MIIBIjANBgkqhkiG9w0B".into()));
        assert_eq!(shown, "ClientKey(<20 bytes>)");
        let names: Vec<_> = [
            SettingChange::IdentityKeyCheck(true),
            SettingChange::LocalStorage(None),
            SettingChange::SearchVisible(true),
            SettingChange::SecurityNotifications(true),
            SettingChange::NumberChangeNotifications(true),
        ]
        .iter()
        .map(SettingChange::name)
        .collect();
        assert_eq!(
            names,
            [
                "identity_key_check",
                "local_storage",
                "search_visibility",
                "security_notifications",
                "notify_user_change_number"
            ]
        );
    }

    /// An application: the library's rules, then the service's.
    #[test]
    fn an_application_is_bounded() {
        let links = || (1..=5).map(|i| format!("https://news.example/{i}"));
        let ok = ObaApplication::new("https://luckyshrub.example", "US").supporting_links(links());
        assert!(check_application(&ok).is_ok());
        let few = ObaApplication::new("https://luckyshrub.example", "US")
            .supporting_links(["https://a.example"]);
        assert_eq!(invalid_field(check_application(&few)), "supporting_links");
        let bad_site = ObaApplication::new("javascript:alert(1)", "US");
        assert_eq!(
            invalid_field(check_application(&bad_site)),
            "business_website_url"
        );
        let credentials = ObaApplication::new("https://good.example@evil.example/", "US");
        assert_eq!(
            invalid_field(check_application(&credentials)),
            "business_website_url"
        );
        let bad_link = ObaApplication::new("https://luckyshrub.example", "US")
            .supporting_links(links().take(4).chain(["ftp://x.example".to_owned()]));
        assert_eq!(
            invalid_field(check_application(&bad_link)),
            "supporting_links"
        );
        let long_country = ObaApplication::new("https://luckyshrub.example", "U".repeat(129));
        assert_eq!(
            invalid_field(check_application(&long_country)),
            "primary_country_of_operation"
        );
        let invisible = ObaApplication::new("https://luckyshrub.example", "US")
            .additional_supporting_information("a\u{202E}b");
        assert_eq!(
            invalid_field(check_application(&invisible)),
            "additional_supporting_information"
        );
        let long = ObaApplication::new("https://luckyshrub.example", "US")
            .additional_supporting_information("x".repeat(TEXT_MAX_CHARS + 1));
        assert_eq!(
            invalid_field(check_application(&long)),
            "additional_supporting_information"
        );
    }

    /// A declaration: the library's rules, then the service's.
    #[test]
    fn a_declaration_is_bounded() {
        let update = || {
            ComplianceInfoUpdate::new(
                "Lucky Shrub Pvt Ltd",
                BusinessEntityType::PrivateCompany,
                GrievanceOfficer::new("A. Rao", "grievance@luckyshrub.example"),
                CustomerCare::new("care@luckyshrub.example"),
            )
        };
        assert!(check_compliance(&update()).is_ok());
        let mut bad_phone = update();
        bad_phone.customer_care_details =
            CustomerCare::new("care@luckyshrub.example").mobile_number("call me maybe");
        assert_eq!(
            invalid_field(check_compliance(&bad_phone)),
            "customer_care_details.mobile_number"
        );
        let mut good_phone = update();
        good_phone.customer_care_details =
            CustomerCare::new("care@luckyshrub.example").mobile_number("+91 98765-43210");
        assert!(check_compliance(&good_phone).is_ok());
        let mut bad_mail = update();
        bad_mail.grievance_officer_details = GrievanceOfficer::new("A. Rao", "not-an-email");
        assert_eq!(
            invalid_field(check_compliance(&bad_mail)),
            "grievance_officer_details.email"
        );
        let mut hidden = update();
        hidden.entity_name = "Lucky\u{200B}Shrub".into();
        assert_eq!(invalid_field(check_compliance(&hidden)), "entity_name");
        let custom_missing = ComplianceInfoUpdate::new(
            "Lucky Shrub",
            BusinessEntityType::Custom,
            GrievanceOfficer::new("A. Rao", "g@luckyshrub.example"),
            CustomerCare::new("c@luckyshrub.example"),
        );
        assert_eq!(
            invalid_field(check_compliance(&custom_missing)),
            "entity_type_custom"
        );
    }

    /// Components: the library's limits, then no invisible character.
    #[test]
    fn components_are_bounded() {
        let ok = ConversationalAutomationConfig::new()
            .enable_welcome_message(true)
            .prompts(["Book a table"])
            .commands([BotCommand::new("book", "Book a table")]);
        assert!(check_components(&ok).is_ok());
        let too_many = ConversationalAutomationConfig::new().prompts(["a", "b", "c", "d", "e"]);
        assert_eq!(invalid_field(check_components(&too_many)), "prompts");
        let hidden = ConversationalAutomationConfig::new().prompts(["a\u{200B}b"]);
        assert_eq!(invalid_field(check_components(&hidden)), "prompts[0]");
        let slash = ConversationalAutomationConfig::new()
            .commands([BotCommand::new("/book", "Book a table")]);
        assert_eq!(
            invalid_field(check_components(&slash)),
            "commands[0].command_name"
        );
        let hidden_hint = ConversationalAutomationConfig::new()
            .commands([BotCommand::new("book", "Book\u{202E}")]);
        assert_eq!(
            invalid_field(check_components(&hidden_hint)),
            "commands[0].command_description"
        );
    }

    /// The audit actions are stored names.
    #[test]
    fn the_actions_are_pinned() {
        assert_eq!(SETTING_CHANGED, "number_setting_changed");
        assert_eq!(PAYLOAD_ENCRYPTION_CHANGED, "payload_encryption_changed");
        assert_eq!(USERNAME_SET, "username_set");
        assert_eq!(USERNAME_DELETED, "username_deleted");
        assert_eq!(OBA_REQUESTED, "official_business_account_requested");
        assert_eq!(COMPLIANCE_INFO_SET, "compliance_info_set");
        assert_eq!(CONTACT_BOOK_ENTRY_DELETED, "contact_book_entry_deleted");
        assert_eq!(
            CONVERSATIONAL_COMPONENTS_SET,
            "conversational_components_set"
        );
        assert_eq!(
            COMPLIANCE_FIELDS.join(","),
            "entity_name,entity_type,entity_type_custom,is_registered,grievance_officer_details,customer_care_details"
        );
    }
}
