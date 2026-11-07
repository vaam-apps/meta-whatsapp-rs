//! The idempotency engine (docs/design/server.md, section 5.4): claiming
//! a caller's `Idempotency-Key`, settling it by what the answer proves,
//! and answering a repeat. The records are an [`IdempotencyRecords`]
//! port; an API adapter reads the key from its request, runs the
//! operation under it ([`run`]: an [`Answer`], a status and a body, and
//! whether it was replayed) and renders the answers (the HTTP one:
//! `meta_whatsapp_server::idempotency`).
//!
//! The key is **scoped to the tenant**: two tenants may use the same one.
//! An operator's key ([`run_admin`], roadmap M5j2) is scoped to its admin
//! key, in a namespace no tenant can be named: `admin/<key id>`.
//! A request carrying one:
//!
//! 1. is checked locally first (body, recipient, limits): a request that
//!    fails there never touches its key;
//! 2. claims the key, `in_progress`, with the SHA-256 of its operation
//!    and its canonical input ([`Fingerprint`]: never the request's
//!    path, so a key means the same through every API adapter and every
//!    spelling of a path), a lease and an expiry ([`claim`]);
//! 3. runs, then settles the key by what the answer proves
//!    ([`Claim::settle`]):
//!    - **nothing was sent** (`may_have_been_sent: false`: a 4xx from
//!      Meta, throttling, a refusal before the request): the key is
//!      **released**, so the caller may repeat the request with it;
//!    - anything else (a success, a timeout, a 5xx): the answer is
//!      **kept**, byte for byte, and a repeat gets it back without a
//!      second request to Meta.
//!
//! A repeat meeting the key finds ([`Repeat`]):
//!
//! | Record | Answer |
//! | --- | --- |
//! | another request's (another operation or input) | `422 idempotency_key_reused` |
//! | `in_progress`, lease running | `409 idempotency_in_progress`, `retryable`, `may_have_been_sent: true` (the other request may be sending) |
//! | `in_progress`, lease over (the process died, or the request was cut without settling) | `409 outcome_unknown`, `may_have_been_sent: true`: never a new send |
//! | completed | the kept answer |
//!
//! A request cut before it settled its key (a [`Claim`] dropped: the
//! deadline, or the client leaving) settles it as a `504 timeout` that may
//! have been sent, from a task of its own; if that fails too, the lease
//! runs out and the key reads as unknown. The key is never logged: it
//! names the caller's records.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use meta_whatsapp_rs::core::error::StorageError;
use meta_whatsapp_rs::core::ids::{PhoneNumberId, WabaId};
use sha2::{Digest, Sha256};

use crate::authz::{AdminCaller, Authorizer, Caller};
use crate::error::{ErrorCode, ServiceError};
use crate::media::{Conversion, Upload, convert_name};
use crate::model::{
    IdempotencyClaim, IdempotencyKey, IdempotencyRecord, IdempotencyState, TenantId,
};
use crate::observe::Observer;
use crate::operation::{AdminOperation, Operation};
use crate::otp::OtpKeys;
use crate::store::IdempotencyRecords;
use crate::uploads::NewUpload;

/// SHA-256 of what a request asks: its operation ([`Operation`], by id)
/// and its **canonical input**, never the request's method or path
/// (roadmap S6, design D27). Each idempotent operation's input is defined
/// here, once, so that every API adapter derives the same fingerprint for
/// the same request:
///
/// | Operation | Canonical input |
/// | --- | --- |
/// | `send_message` ([`Self::send_message`]) | the number's id, as a [`PhoneNumberId`] (the path parameter decoded: `%31` and `1` are one number); the JSON body as the request carried it, parsed (an escape is the character it spells; of a key given twice, the last, as the operation reads it), its object keys sorted and its whitespace ignored; nothing else is folded (`1` and `1.0` are two bodies) |
/// | `upload_media` ([`Self::upload_media`]) | the number's id; the checked [`Upload`]'s MIME type, the file name sent to Meta, and the file's SHA-256 |
/// | `upload_media`, converted ([`Self::upload_converted_media`], roadmap M5c1) | the number's id; the format wanted, the file name that will be sent and the **source** file's SHA-256: the conversion runs after the key is claimed, so a repeat never converts again |
/// | `start_media_upload` ([`Self::start_media_upload`], roadmap M5c1) | the number's id; the file name sent to Meta, the declared length and the declared MIME type, as a JSON object, canonical as above |
/// | `create_template` ([`Self::create_template`]) | the WABA's id, as a [`WabaId`]; the JSON body, canonical as above |
/// | `reply_to_conversation` ([`Self::reply_to_conversation`]) | the number's id; the conversation's contact, as the path named it decoded; the JSON body, canonical as above |
/// | `mark_conversation_read` ([`Self::mark_conversation_read`]) | the number's id; the conversation's contact, as the path named it decoded; the JSON body, canonical as above (`null` for an empty one) |
/// | `create_authentication_template` ([`Self::create_authentication_template`]) | the WABA's id; the JSON body, canonical as above |
/// | `issue_otp` ([`Self::issue_otp`]) | the JSON body, canonical as above; the digest then keyed with the OTP pepper's fingerprint key (an HMAC-SHA256): the body names a phone number |
/// | `request_verification_code` ([`Self::request_verification_code`]) | the number's id; the delivery method and the language, as a JSON object, canonical as above |
/// | `register_number` ([`Self::register_number`]) | the number's id; the data localization region, or none: **never the PIN** (roadmap M3d, decisions D6 and D67), so no record holds anything derived from it |
/// | `deregister_number` ([`Self::deregister_number`]) | the number's id (the body's confirmation is that id again) |
/// | `create_group` ([`Self::create_group`]; roadmap M5f) | the number's id; the JSON body, canonical as above |
/// | `upload_call_voicemail_announcement` ([`Self::upload_call_voicemail_announcement`]; roadmap M5e2) | the number's id; the file's SHA-256 (the file itself is not kept) |
/// | `create_library_template`, `migrate_templates`, `create_template_group` ([`Self::create_library_template`], [`Self::migrate_templates`], [`Self::create_template_group`]; roadmap M5b) | the WABA's id; the JSON body, canonical as above |
/// | `request_display_name_change` ([`Self::request_display_name_change`]; roadmap M5c2) | the number's id; the JSON body, canonical as above |
/// | `request_official_business_account` ([`Self::request_official_business_account`]; roadmap M5c3) | the number's id; the JSON body, canonical as above |
/// | `create_qr_code`, `block_users`, `unblock_users` ([`Self::create_qr_code`], [`Self::block_users`], [`Self::unblock_users`]; roadmap M5g) | the number's id; the JSON body, canonical as above |
/// | `enable_template_insights` ([`Self::enable_template_insights`]; roadmap M5g2) | the WABA's id (the request has no body) |
/// | `set_template_click_tracking` ([`Self::set_template_click_tracking`]; roadmap M5g2) | the WABA's id; the template's id; the JSON body, canonical as above |
/// | `send_marketing_message` ([`Self::send_marketing_message`]; roadmap M5h) | the number's id; the JSON body, canonical as above |
/// | `duplicate_marketing_template`, `create_welcome_sequence` ([`Self::duplicate_marketing_template`], [`Self::create_welcome_sequence`]; roadmap M5h) | the WABA's id; the JSON body, canonical as above |
/// | `pass_thread`, `take_thread`, `release_thread` ([`Self::pass_thread`], [`Self::take_thread`], [`Self::release_thread`]; roadmap M5l) | the number's id; the JSON body, canonical as above |
/// | `create_broadcast` ([`Self::create_broadcast`]; roadmap M5k) | the number's id; the JSON body, canonical as above |
/// | `create_flow` ([`Self::create_flow`]; roadmap M5d) | the WABA's id; the JSON body, canonical as above |
/// | `set_flow_key` ([`Self::set_flow_key`]; roadmap M5d2) | the WABA's id; the SHA-256 of the key's **public** half: **never the private key** (nor anything derived from it), so no record holds any |
/// | `submit_business_verification`, `create_partner_solution` ([`Self::submit_business_verification`], [`Self::create_partner_solution`]; roadmap M5j2) | the JSON body, canonical as above; the operator's, so under [`run_admin`]: the admin key's id is the records' scope, not an input |
/// | `add_pre_verified_number`, `request_pre_verified_code`, `complete_hosted_signup` ([`Self::add_pre_verified_number`], [`Self::request_pre_verified_code`], [`Self::complete_hosted_signup`]; roadmap M5j3) | the JSON body, canonical as above (the hosted claim's `pin` only as present); the pool number's id for the code request; the operator's, as above |
///
/// The same request always has the same fingerprint; another operation,
/// another number or WABA, or another body has another. Fingerprints are
/// stored with their key (for `WA_SERVER_IDEMPOTENCY_TTL`).
///
/// **The fingerprint before roadmap S6, matched for one TTL.** A key
/// claimed by a release before S6 was stored with the SHA-256 of the
/// request's method, its path as sent and its body
/// ([`Self::legacy_v1_json`], [`Self::legacy_v1_upload`]). So that such a
/// key keeps replaying after the upgrade (a `422` there would invite a
/// resend under a new key, and a second message to the customer), an HTTP
/// adapter adds the request's legacy fingerprint ([`Self::or_legacy`]): a
/// record matches a request when its fingerprint is the request's, or the
/// request's legacy one. A claim always stores the new one
/// ([`Self::as_bytes`]). The legacy constructors take an HTTP method and
/// path, so they are not adapter-neutral: an adapter other than `/v1`'s,
/// which no record before S6 was made through, has no reason to call
/// them. They go after the first release, once its records have expired
/// (roadmap S7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    digest: [u8; 32],
    /// The legacy fingerprint of the same request, also matched.
    legacy: Option<[u8; 32]>,
}

impl Fingerprint {
    /// `send_message` on `number`, with the JSON body the request carried.
    pub fn send_message(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::SendMessage, &[("pn", number.as_str())], body)
    }

    /// `upload_media` of `upload` on `number`: its MIME type, its file name
    /// and its file's SHA-256 (the file itself is not kept).
    pub fn upload_media(number: &PhoneNumberId, upload: &Upload) -> Self {
        let digest = Sha256::digest(upload.data());
        Self::parts(
            Operation::UploadMedia,
            &[("pn", number.as_str())],
            &[
                ("type", upload.mime_type().as_bytes()),
                ("filename", upload.filename().as_bytes()),
                ("file", digest.as_slice()),
            ],
        )
    }

    /// `upload_call_voicemail_announcement` of `audio` on `number` (roadmap
    /// M5e2; no legacy fingerprint): the file's SHA-256 (the file itself is
    /// not kept).
    pub fn upload_call_voicemail_announcement(
        number: &PhoneNumberId,
        audio: &crate::calls::VoicemailAudio,
    ) -> Self {
        let digest = Sha256::digest(audio.data());
        Self::parts(
            Operation::UploadCallVoicemailAnnouncement,
            &[("pn", number.as_str())],
            &[("file", digest.as_slice())],
        )
    }

    /// `upload_media` of `conversion` on `number` (roadmap M5c1): the
    /// format wanted, the file name that will be sent and the SHA-256 of
    /// the **source** file. The converted bytes are not an input: the
    /// conversion runs after the key is claimed.
    pub fn upload_converted_media(number: &PhoneNumberId, conversion: &Conversion) -> Self {
        let digest = Sha256::digest(conversion.data());
        Self::parts(
            Operation::UploadMedia,
            &[("pn", number.as_str())],
            &[
                ("convert", convert_name(conversion.target()).as_bytes()),
                ("filename", conversion.file_name().as_bytes()),
                ("file", digest.as_slice()),
            ],
        )
    }

    /// `start_media_upload` on `number` for `upload` (roadmap M5c1): the
    /// file name that will be sent to Meta, the declared length and type.
    pub fn start_media_upload(number: &PhoneNumberId, upload: &NewUpload) -> Self {
        Self::json(
            Operation::StartMediaUpload,
            &[("pn", number.as_str())],
            &serde_json::json!({
                "file_name": upload.file_name(),
                "file_length": upload.file_length(),
                "file_type": upload.file_type(),
            }),
        )
    }

    /// `create_template` in `waba`, with the JSON definition the request
    /// carried.
    pub fn create_template(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::CreateTemplate,
            &[("waba_id", waba.as_str())],
            body,
        )
    }

    /// `reply_to_conversation` on `number`, to the conversation of
    /// `contact` (as the path named it, decoded), with the JSON body the
    /// request carried (roadmap M2a). A release before it had no such
    /// operation, so there is no legacy fingerprint to match.
    pub fn reply_to_conversation(
        number: &PhoneNumberId,
        contact: &str,
        body: &serde_json::Value,
    ) -> Self {
        Self::json(
            Operation::ReplyToConversation,
            &[("pn", number.as_str()), ("contact", contact)],
            body,
        )
    }

    /// `mark_conversation_read` on `number`, of the conversation of
    /// `contact` (as the path named it, decoded), with the JSON body the
    /// request carried, `null` when it had none (roadmap M2g). A release
    /// before it had no such operation, so there is no legacy fingerprint
    /// to match.
    pub fn mark_conversation_read(
        number: &PhoneNumberId,
        contact: &str,
        body: &serde_json::Value,
    ) -> Self {
        Self::json(
            Operation::MarkConversationRead,
            &[("pn", number.as_str()), ("contact", contact)],
            body,
        )
    }

    /// `create_authentication_template` in `waba`, with the JSON body the
    /// request carried (roadmap M3c). No release before it had the
    /// operation: no legacy fingerprint.
    pub fn create_authentication_template(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::CreateAuthenticationTemplate,
            &[("waba_id", waba.as_str())],
            body,
        )
    }

    /// `issue_otp` with the JSON body the request carried (roadmap M3c),
    /// **keyed**: the body names a phone number, and design section 6 keeps
    /// numbers only as keyed hashes, so the stored digest is an HMAC-SHA256
    /// of the canonical input's SHA-256 under a key derived from the OTP
    /// pepper ([`OtpKeys`]), never a bare SHA-256 a list of phone numbers
    /// would invert. No release before it had the operation: no legacy
    /// fingerprint.
    pub fn issue_otp(keys: &OtpKeys, body: &serde_json::Value) -> Self {
        let plain = Self::json(Operation::IssueOtp, &[], body);
        Self::of(keys.fingerprint(&plain.digest))
    }

    /// `request_verification_code` on `number`, by `method` (Meta's name,
    /// `SMS` or `VOICE`) in `language` (roadmap M3d).
    pub fn request_verification_code(number: &PhoneNumberId, method: &str, language: &str) -> Self {
        Self::json(
            Operation::RequestVerificationCode,
            &[("pn", number.as_str())],
            &serde_json::json!({"code_method": method, "language": language}),
        )
    }

    /// `register_number` on `number`, with the data localization region
    /// the request named, if any (roadmap M3d). The PIN is no input: a
    /// 6-digit secret's digest is the secret (a million guesses), so the
    /// stored fingerprint holds nothing of it. The cost: a repeat with the
    /// same key and another PIN is the same request (it gets the kept
    /// answer, or `409 idempotency_in_progress`); a wrong PIN Meta refused
    /// releases the key (nothing was registered), so the corrected PIN
    /// may reuse it.
    pub fn register_number(number: &PhoneNumberId, region: Option<&str>) -> Self {
        Self::json(
            Operation::RegisterNumber,
            &[("pn", number.as_str())],
            &serde_json::json!({"data_localization_region": region}),
        )
    }

    /// `deregister_number` on `number` (roadmap M3d).
    pub fn deregister_number(number: &PhoneNumberId) -> Self {
        Self::json(
            Operation::DeregisterNumber,
            &[("pn", number.as_str())],
            &serde_json::json!({}),
        )
    }

    /// `create_payment_configuration` in `waba`, with the JSON body the
    /// request carried (roadmap P2; no legacy fingerprint).
    pub fn create_payment_configuration(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::CreatePaymentConfiguration,
            &[("waba_id", waba.as_str())],
            body,
        )
    }

    /// `regenerate_payment_oauth_link` of the configuration
    /// `configuration_name` (as the path named it, decoded) in `waba`, with
    /// the JSON body the request carried, `null` for none (roadmap P2).
    pub fn regenerate_payment_oauth_link(
        waba: &WabaId,
        configuration_name: &str,
        body: &serde_json::Value,
    ) -> Self {
        Self::json(
            Operation::RegeneratePaymentOauthLink,
            &[
                ("waba_id", waba.as_str()),
                ("configuration_name", configuration_name),
            ],
            body,
        )
    }

    /// `refund_payment` on `number`, with the JSON body the request carried
    /// (roadmap P2).
    pub fn refund_payment(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::RefundPayment, &[("pn", number.as_str())], body)
    }

    /// `create_library_template` in `waba`, with the JSON body the request
    /// carried (roadmap M5b; no legacy fingerprint).
    pub fn create_library_template(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::CreateLibraryTemplate,
            &[("waba_id", waba.as_str())],
            body,
        )
    }

    /// `migrate_templates` into `waba`, with the JSON body the request
    /// carried (roadmap M5b; no legacy fingerprint).
    pub fn migrate_templates(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::MigrateTemplates,
            &[("waba_id", waba.as_str())],
            body,
        )
    }

    /// `create_template_group` in `waba`, with the JSON body the request
    /// carried (roadmap M5b; no legacy fingerprint).
    pub fn create_template_group(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::CreateTemplateGroup,
            &[("waba_id", waba.as_str())],
            body,
        )
    }

    /// `create_qr_code` on `number`, with the JSON body the request carried
    /// (roadmap M5g; no legacy fingerprint).
    pub fn create_qr_code(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::CreateQrCode, &[("pn", number.as_str())], body)
    }

    /// `block_users` on `number`, with the JSON body the request carried
    /// (roadmap M5g; no legacy fingerprint).
    pub fn block_users(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::BlockUsers, &[("pn", number.as_str())], body)
    }

    /// `unblock_users` on `number`, with the JSON body the request carried
    /// (roadmap M5g; no legacy fingerprint).
    pub fn unblock_users(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::UnblockUsers, &[("pn", number.as_str())], body)
    }

    /// `pass_thread` on `number`, with the JSON body the request carried
    /// (roadmap M5l; no legacy fingerprint).
    pub fn pass_thread(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::PassThread, &[("pn", number.as_str())], body)
    }

    /// `take_thread` on `number`, with the JSON body the request carried
    /// (roadmap M5l; no legacy fingerprint).
    pub fn take_thread(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::TakeThread, &[("pn", number.as_str())], body)
    }

    /// `release_thread` on `number`, with the JSON body the request carried
    /// (roadmap M5l; no legacy fingerprint).
    pub fn release_thread(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::ReleaseThread, &[("pn", number.as_str())], body)
    }

    /// `create_broadcast` on `number`, with the JSON body the request
    /// carried (roadmap M5k; no legacy fingerprint).
    pub fn create_broadcast(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::CreateBroadcast, &[("pn", number.as_str())], body)
    }

    /// `create_group` on `number`, with the JSON body the request carried
    /// (roadmap M5f; no legacy fingerprint).
    pub fn create_group(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::CreateGroup, &[("pn", number.as_str())], body)
    }

    /// `connect_call` on `number`, with the JSON body the request carried
    /// (roadmap M5e; no legacy fingerprint). The body holds the SDP offer:
    /// the fingerprint is a hash of it, as every body's, and the offer is
    /// stored nowhere.
    pub fn connect_call(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(Operation::ConnectCall, &[("pn", number.as_str())], body)
    }

    /// `create_solution_migration_intent` on `waba`, with the JSON body the
    /// request carried (roadmap M5j; no legacy fingerprint).
    pub fn create_solution_migration_intent(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::CreateSolutionMigrationIntent,
            &[("waba_id", waba.as_str())],
            body,
        )
    }

    /// `create_billing_migration` on `waba`, with the JSON body the request
    /// carried (roadmap M5j; no legacy fingerprint).
    pub fn create_billing_migration(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::CreateBillingMigration,
            &[("waba_id", waba.as_str())],
            body,
        )
    }

    /// `request_display_name_change` on `number`, with the JSON body the
    /// request carried (roadmap M5c2; no legacy fingerprint).
    pub fn request_display_name_change(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::RequestDisplayNameChange,
            &[("pn", number.as_str())],
            body,
        )
    }

    /// `request_official_business_account` on `number`, with the JSON
    /// body the request carried (roadmap M5c3; no legacy fingerprint).
    pub fn request_official_business_account(
        number: &PhoneNumberId,
        body: &serde_json::Value,
    ) -> Self {
        Self::json(
            Operation::RequestOfficialBusinessAccount,
            &[("pn", number.as_str())],
            body,
        )
    }

    /// `send_marketing_message` on `number`, with the JSON body the request
    /// carried (roadmap M5h; no legacy fingerprint).
    pub fn send_marketing_message(number: &PhoneNumberId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::SendMarketingMessage,
            &[("pn", number.as_str())],
            body,
        )
    }

    /// `enable_template_insights` in `waba` (roadmap M5g2; the request has
    /// no body; no legacy fingerprint).
    pub fn enable_template_insights(waba: &WabaId) -> Self {
        Self::json(
            Operation::EnableTemplateInsights,
            &[("waba_id", waba.as_str())],
            &serde_json::Value::Null,
        )
    }

    /// `set_template_click_tracking` of `template` in `waba`, with the JSON
    /// body the request carried (roadmap M5g2; no legacy fingerprint).
    pub fn set_template_click_tracking(
        waba: &WabaId,
        template: &str,
        body: &serde_json::Value,
    ) -> Self {
        Self::json(
            Operation::SetTemplateClickTracking,
            &[("waba_id", waba.as_str()), ("template_id", template)],
            body,
        )
    }

    /// `duplicate_marketing_template` in `waba`, with the JSON body the
    /// request carried (roadmap M5h; no legacy fingerprint).
    pub fn duplicate_marketing_template(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::DuplicateMarketingTemplate,
            &[("waba_id", waba.as_str())],
            body,
        )
    }

    /// `create_welcome_sequence` in `waba`, with the JSON body the request
    /// carried (roadmap M5h; no legacy fingerprint).
    pub fn create_welcome_sequence(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(
            Operation::CreateWelcomeSequence,
            &[("waba_id", waba.as_str())],
            body,
        )
    }

    /// `create_flow` in `waba`, with the JSON body the request carried
    /// (roadmap M5d; no legacy fingerprint).
    pub fn create_flow(waba: &WabaId, body: &serde_json::Value) -> Self {
        Self::json(Operation::CreateFlow, &[("waba_id", waba.as_str())], body)
    }

    /// `set_flow_key` in `waba`, with the SHA-256 (hex) of the new key's
    /// public half (roadmap M5d2; no legacy fingerprint). The private key
    /// is no input and nothing derived from it but its public half, which
    /// is public: a repeat with the same key and another key is
    /// `idempotency_key_reused`, and no record holds secret material.
    pub fn set_flow_key(waba: &WabaId, public_key_sha256: &str) -> Self {
        Self::json(
            Operation::SetFlowKey,
            &[("waba_id", waba.as_str())],
            &serde_json::json!({"public_key_sha256": public_key_sha256}),
        )
    }

    /// `submit_business_verification` with the JSON body the request
    /// carried (roadmap M5j2; no legacy fingerprint). The body holds the
    /// documents: the fingerprint is a hash of it, as every body's, and
    /// the documents are stored nowhere.
    pub fn submit_business_verification(body: &serde_json::Value) -> Self {
        Self::admin_json(AdminOperation::SubmitBusinessVerification.id(), body)
    }

    /// `create_partner_solution` with the JSON body the request carried
    /// (roadmap M5j2; no legacy fingerprint). Not a legal act, so not an
    /// [`AdminOperation`]: the id is the route's `operationId`.
    pub fn create_partner_solution(body: &serde_json::Value) -> Self {
        Self::admin_json("create_partner_solution", body)
    }

    /// `add_pre_verified_number` with the JSON body the request carried
    /// (roadmap M5j3; no legacy fingerprint): the number added to the
    /// pool, so a retry does not add it twice.
    pub fn add_pre_verified_number(body: &serde_json::Value) -> Self {
        Self::admin_json("add_pre_verified_number", body)
    }

    /// `request_pre_verified_code` for the pool's number `id` (as the path
    /// spelled it, decoded) with the JSON body the request carried
    /// (roadmap M5j3; no legacy fingerprint): each request sends the
    /// number a message. The number is an input: the same body for
    /// another number is another request.
    pub fn request_pre_verified_code(id: &str, body: &serde_json::Value) -> Self {
        Self::admin_json_for("request_pre_verified_code", id, body)
    }

    /// `complete_hosted_signup` with the JSON body the request carried
    /// (roadmap M5j3; no legacy fingerprint). The body's `pin` is a
    /// secret of six digits, so its value is not hashed (a digest of a
    /// million values is a guess away, and the record outlives the
    /// request): only its presence is. A repeat with another PIN under the
    /// same key replays the first answer.
    pub fn complete_hosted_signup(body: &serde_json::Value) -> Self {
        let mut body = body.clone();
        if let Some(pin) = body.get_mut("pin")
            && pin.is_string()
        {
            *pin = serde_json::Value::Bool(true);
        }
        Self::admin_json("complete_hosted_signup", &body)
    }

    /// An operator's operation by id and one named target, with a JSON
    /// body: the kind `admin-json-for`, apart from [`Self::admin_json`].
    fn admin_json_for(operation: &str, target: &str, body: &serde_json::Value) -> Self {
        let mut hasher = Sha256::new();
        framed(&mut hasher, b"admin-json-for");
        framed(&mut hasher, operation.as_bytes());
        framed(&mut hasher, target.as_bytes());
        canonical(&mut hasher, body);
        Self::of(hasher.finalize().into())
    }

    /// An operator's operation by id, with a JSON body. Another kind than
    /// a tenant's ([`Self::json`]), so that no admin fingerprint equals a
    /// tenant's whatever the ids are. The admin key is in the records'
    /// scope ([`run_admin`]), not in the digest.
    fn admin_json(operation: &str, body: &serde_json::Value) -> Self {
        let mut hasher = Sha256::new();
        framed(&mut hasher, b"admin-json");
        framed(&mut hasher, operation.as_bytes());
        canonical(&mut hasher, body);
        Self::of(hasher.finalize().into())
    }

    /// `operation` with its named parameters `params` (in any order: they
    /// are hashed sorted by name) and a JSON body.
    fn json(operation: Operation, params: &[(&str, &str)], body: &serde_json::Value) -> Self {
        let mut hasher = Self::begin(b"json", operation, params);
        canonical(&mut hasher, body);
        Self::of(hasher.finalize().into())
    }

    /// `operation` with its named parameters `params` and a body of named
    /// parts, in the order given.
    fn parts(operation: Operation, params: &[(&str, &str)], parts: &[(&str, &[u8])]) -> Self {
        let mut hasher = Self::begin(b"parts", operation, params);
        hasher.update((parts.len() as u64).to_be_bytes());
        for (name, value) in parts {
            framed(&mut hasher, name.as_bytes());
            framed(&mut hasher, value);
        }
        Self::of(hasher.finalize().into())
    }

    /// The body's kind, the operation's id and the parameters, sorted by
    /// name, each framed.
    fn begin(kind: &[u8], operation: Operation, params: &[(&str, &str)]) -> Sha256 {
        let mut hasher = Sha256::new();
        framed(&mut hasher, kind);
        framed(&mut hasher, operation.id().as_bytes());
        let mut params = params.to_vec();
        params.sort_unstable();
        hasher.update((params.len() as u64).to_be_bytes());
        for (name, value) in params {
            framed(&mut hasher, name.as_bytes());
            framed(&mut hasher, value.as_bytes());
        }
        hasher
    }

    /// The fingerprint `digest`, with no legacy one.
    fn of(digest: [u8; 32]) -> Self {
        Self {
            digest,
            legacy: None,
        }
    }

    /// The fingerprint a release before roadmap S6 stored for a request
    /// with a JSON body: the SHA-256 of `method` (`POST`), `path` as the
    /// request carried it, and the canonical body, exactly as it was
    /// computed then (its known answers are pinned). Only for
    /// [`Self::or_legacy`]; never stored.
    pub fn legacy_v1_json<M: AsRef<str> + ?Sized>(
        method: &M,
        path: &str,
        body: &serde_json::Value,
    ) -> Self {
        let mut hasher = Sha256::new();
        framed(&mut hasher, b"json");
        framed(&mut hasher, method.as_ref().as_bytes());
        framed(&mut hasher, path.as_bytes());
        canonical(&mut hasher, body);
        Self::of(hasher.finalize().into())
    }

    /// The fingerprint a release before roadmap S6 stored for an upload:
    /// `method`, `path` as the request carried it, and the parts `type`,
    /// `filename` (the file name sent to Meta) and `file` (its SHA-256),
    /// exactly as it was computed then. Only for [`Self::or_legacy`].
    pub fn legacy_v1_upload<M: AsRef<str> + ?Sized>(
        method: &M,
        path: &str,
        upload: &Upload,
    ) -> Self {
        let digest = Sha256::digest(upload.data());
        Self::legacy_v1_parts(
            method,
            path,
            &[
                ("type", upload.mime_type().as_bytes()),
                ("filename", upload.filename().as_bytes()),
                ("file", digest.as_slice()),
            ],
        )
    }

    /// The legacy fingerprint of a request made of named parts, in the
    /// order given, as it was computed before roadmap S6.
    fn legacy_v1_parts<M: AsRef<str> + ?Sized>(
        method: &M,
        path: &str,
        parts: &[(&str, &[u8])],
    ) -> Self {
        let mut hasher = Sha256::new();
        framed(&mut hasher, b"parts");
        framed(&mut hasher, method.as_ref().as_bytes());
        framed(&mut hasher, path.as_bytes());
        for (name, value) in parts {
            framed(&mut hasher, name.as_bytes());
            framed(&mut hasher, value);
        }
        Self::of(hasher.finalize().into())
    }

    /// This fingerprint, also matching a record stored under `legacy`, the
    /// same request's fingerprint before roadmap S6 (see the type's
    /// documentation). What a claim stores is still this one.
    #[must_use]
    pub fn or_legacy(mut self, legacy: Fingerprint) -> Self {
        self.legacy = Some(legacy.digest);
        self
    }

    /// Whether a record stored with `stored` is this request's: its
    /// fingerprint, or its legacy one.
    pub fn matches(&self, stored: &[u8; 32]) -> bool {
        self.digest == *stored || self.legacy.as_ref() == Some(stored)
    }

    /// The digest, what a claim stores.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// `bytes` with its length first, so that no two sequences of fields hash
/// alike.
fn framed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

/// A JSON value, with object keys in sorted order whatever the parser
/// kept.
fn canonical(hasher: &mut Sha256, value: &serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::Null => hasher.update(b"n"),
        Value::Bool(b) => hasher.update(if *b { b"t" } else { b"f" }),
        Value::Number(n) => {
            hasher.update(b"#");
            framed(hasher, n.to_string().as_bytes());
        }
        Value::String(s) => {
            hasher.update(b"s");
            framed(hasher, s.as_bytes());
        }
        Value::Array(items) => {
            hasher.update(b"[");
            hasher.update((items.len() as u64).to_be_bytes());
            for item in items {
                canonical(hasher, item);
            }
        }
        Value::Object(map) => {
            hasher.update(b"{");
            hasher.update((map.len() as u64).to_be_bytes());
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (key, value) in entries {
                framed(hasher, key.as_bytes());
                canonical(hasher, value);
            }
        }
    }
}

/// The body an API adapter answers `error` with, byte for byte: what a
/// key keeps for a failure that may have been sent.
pub type RenderError = fn(&ServiceError) -> Vec<u8>;

/// What claiming a key found.
#[derive(Debug)]
pub enum Admission {
    /// The key was free (or its record had expired): run the request,
    /// then [`Claim::settle`] it.
    Claimed(Claim),
    /// Another request holds, or held, the key: answer this, without
    /// running the request.
    Repeat(Repeat),
}

/// The answer to a request meeting another request's record. `Debug`
/// shows a kept answer's length (`body_len`), never its bytes.
#[derive(Clone, PartialEq, Eq)]
pub enum Repeat {
    /// Another operation or input: `422 idempotency_key_reused`.
    Reused,
    /// Still running: `409 idempotency_in_progress`, `retryable`,
    /// `may_have_been_sent: true`.
    InProgress,
    /// Its lease ended without an answer: `409 outcome_unknown`,
    /// `may_have_been_sent: true`. Never a new send.
    OutcomeUnknown,
    /// Completed: its kept answer, again.
    Replay {
        /// Its status.
        status: u16,
        /// Its body, byte for byte.
        body: Vec<u8>,
    },
}

impl std::fmt::Debug for Repeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reused => f.write_str("Reused"),
            Self::InProgress => f.write_str("InProgress"),
            Self::OutcomeUnknown => f.write_str("OutcomeUnknown"),
            Self::Replay { status, body } => f
                .debug_struct("Replay")
                .field("status", status)
                .field("body_len", &body.len())
                .finish(),
        }
    }
}

impl Repeat {
    /// The answer to a request with `fingerprint` meeting `record`.
    pub fn of(record: IdempotencyRecord, fingerprint: &Fingerprint) -> Self {
        if !fingerprint.matches(&record.fingerprint) {
            return Self::Reused;
        }
        match record.state {
            IdempotencyState::InProgress {
                lease_expired: false,
            } => Self::InProgress,
            IdempotencyState::InProgress {
                lease_expired: true,
            } => Self::OutcomeUnknown,
            IdempotencyState::Completed { status, body } => Self::Replay { status, body },
        }
    }

    /// Its name, for metrics: `reused`, `in_progress`, `outcome_unknown`
    /// or `replayed`.
    pub fn outcome(&self) -> &'static str {
        match self {
            Self::Reused => "reused",
            Self::InProgress => "in_progress",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Replay { .. } => "replayed",
        }
    }

    /// The kept answer, `(status, body)`, or the error to answer instead.
    pub fn into_answer(self) -> Result<(u16, Vec<u8>), ServiceError> {
        match self {
            Self::Reused => Err(ServiceError::new(ErrorCode::IDEMPOTENCY_KEY_REUSED)),
            Self::InProgress => Err(ServiceError::new(ErrorCode::IDEMPOTENCY_IN_PROGRESS)
                .retryable(true)
                .with_may_have_been_sent(true)),
            Self::OutcomeUnknown => {
                Err(ServiceError::new(ErrorCode::OUTCOME_UNKNOWN).with_may_have_been_sent(true))
            }
            Self::Replay { status, body } => Ok((status, body)),
        }
    }
}

/// Claim `key` for `tenant`'s request with `fingerprint`: a lease of
/// `lease` (twice the Graph timeout, section 5.4) and an expiry after
/// `ttl`. `render` is how the adapter answers an error, for the answer a
/// failure keeps.
///
/// # Errors
///
/// No random claim id (`500 internal`), or the store failing (`503
/// storage_unavailable`).
pub async fn claim(
    records: &Arc<dyn IdempotencyRecords>,
    tenant: &TenantId,
    key: IdempotencyKey,
    fingerprint: &Fingerprint,
    lease: Duration,
    ttl: Duration,
    render: RenderError,
) -> Result<Admission, ServiceError> {
    let Some(claim) = claim_id() else {
        return Err(ServiceError::internal());
    };
    let claimed = records
        .claim_idempotency_key(tenant, &key, fingerprint.as_bytes(), &claim, lease, ttl)
        .await?;
    Ok(match claimed {
        IdempotencyClaim::Existing(record) => Admission::Repeat(Repeat::of(record, fingerprint)),
        IdempotencyClaim::Claimed => Admission::Claimed(Claim {
            records: records.clone(),
            tenant: tenant.clone(),
            key,
            id: claim,
            render,
            armed: true,
        }),
    })
}

/// A random claim id: which request holds a key.
fn claim_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(hex::encode(bytes))
}

/// How the request holding a key ended. `Debug` shows an answer's length
/// (`body_len`), never its bytes.
#[derive(Clone, Copy)]
pub enum Outcome<'a> {
    /// It answered this: kept, byte for byte.
    Answered {
        /// The status.
        status: u16,
        /// The body.
        body: &'a [u8],
    },
    /// It failed: kept (as the adapter renders it) when it may have been
    /// sent, else the key is released.
    Failed(&'a ServiceError),
}

impl std::fmt::Debug for Outcome<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Answered { status, body } => f
                .debug_struct("Answered")
                .field("status", status)
                .field("body_len", &body.len())
                .finish(),
            Self::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
        }
    }
}

/// A key this request claimed, until it settles it. Dropped unsettled (the
/// request cut), it settles it as a `504 timeout` that may have been sent,
/// from a task of its own.
pub struct Claim {
    records: Arc<dyn IdempotencyRecords>,
    tenant: TenantId,
    key: IdempotencyKey,
    /// The claim id: which request holds the key.
    id: String,
    render: RenderError,
    /// Not settled yet: dropping it settles it as a timeout.
    armed: bool,
}

impl std::fmt::Debug for Claim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key names the caller's records.
        f.debug_struct("Claim")
            .field("tenant", &self.tenant)
            .finish_non_exhaustive()
    }
}

impl Claim {
    /// Keep or release the key by what `outcome` proves. A key that
    /// cannot be settled is logged, and stays `in_progress` until its
    /// lease ends, then reads as unknown (never a second send).
    pub async fn settle(mut self, outcome: Outcome<'_>) {
        let records = self.records.clone();
        match outcome {
            Outcome::Answered { status, body } => {
                let kept = records
                    .complete_idempotency_key(&self.tenant, &self.key, &self.id, status, body)
                    .await;
                note(kept, "kept");
            }
            Outcome::Failed(error) if error.may_have_been_sent() => {
                let body = (self.render)(error);
                let kept = records
                    .complete_idempotency_key(
                        &self.tenant,
                        &self.key,
                        &self.id,
                        error.status(),
                        &body,
                    )
                    .await;
                note(kept, "kept");
            }
            Outcome::Failed(_) => {
                let released = records
                    .release_idempotency_key(&self.tenant, &self.key, &self.id)
                    .await;
                note(released, "released");
            }
        }
        self.armed = false;
    }
}

/// Log a key that could not be settled: it stays `in_progress` until its
/// lease ends, then reads as unknown (never a second send).
fn note(settled: Result<bool, StorageError>, what: &str) {
    match settled {
        Ok(true) => {}
        Ok(false) => tracing::warn!(outcome = what, "an idempotency key was no longer held"),
        Err(error) => {
            tracing::warn!(outcome = what, error = %error, "an idempotency key could not be settled");
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Cut before it settled (the request deadline, or the client
        // leaving): whatever was under way may have been sent.
        let answer = ServiceError::new(ErrorCode::TIMEOUT)
            .retryable(true)
            .with_may_have_been_sent(true);
        let status = answer.status();
        let body = (self.render)(&answer);
        let (records, tenant, key, claim) = (
            self.records.clone(),
            self.tenant.clone(),
            self.key.clone(),
            std::mem::take(&mut self.id),
        );
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let kept = records
                    .complete_idempotency_key(&tenant, &key, &claim, status, &body)
                    .await;
                note(kept, "kept after a cut");
            });
        }
    }
}

// ─── Running an operation under a key ────────────────────────────────────

/// A successful answer of an idempotent operation, as the adapter sends
/// it and a key keeps it: a status and a body (the adapter's bytes: the
/// HTTP one's JSON), and whether it is a kept answer given again
/// ([`Self::replayed`]), which a kept failure's may be too. `Debug` shows
/// the body's length (`body_len`), never its bytes: a kept answer names
/// the tenant's customers.
#[derive(Clone, PartialEq, Eq)]
pub struct Answer {
    status: u16,
    body: Vec<u8>,
    /// What a key keeps for a repeat, when it is not `body`
    /// ([`Self::keeping`]).
    kept: Option<Vec<u8>>,
    replayed: bool,
}

impl std::fmt::Debug for Answer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Answer")
            .field("status", &self.status)
            .field("body_len", &self.body.len())
            .field("kept_len", &self.kept.as_ref().map(Vec::len))
            .field("replayed", &self.replayed)
            .finish()
    }
}

impl Answer {
    /// An operation's answer: `status` and `body`, not replayed.
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            body,
            kept: None,
            replayed: false,
        }
    }

    /// Keep `kept` for a repeat instead of the body answered now: for an
    /// answer holding a credential the records must not store (a payment
    /// configuration's OAuth link, roadmap P2), the same answer without it.
    /// A repeat then gets `kept`, never the body.
    #[must_use]
    pub fn keeping(mut self, kept: Vec<u8>) -> Self {
        self.kept = Some(kept);
        self
    }

    /// The status.
    pub fn status(&self) -> u16 {
        self.status
    }

    /// The body, byte for byte.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The body.
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }

    /// Whether this is an earlier request's kept answer, given again
    /// without running the operation (the HTTP adapter says so with
    /// `Idempotent-Replayed: true`).
    pub fn replayed(&self) -> bool {
        self.replayed
    }
}

/// Where and how [`run`] keeps keys: the records, the lease (twice the
/// Graph timeout, section 5.4), the expiry (24 hours by default,
/// `WA_SERVER_IDEMPOTENCY_TTL`), how the adapter renders an error (the
/// body a failure that may have been sent keeps), and who is told a
/// repeat's outcome ([`Observer::idempotency`], once
/// [`Self::observed_by`]). Cheap: a few references.
#[derive(Clone, Copy)]
pub struct Keeper<'a> {
    records: &'a Arc<dyn IdempotencyRecords>,
    lease: Duration,
    ttl: Duration,
    render: RenderError,
    observer: &'a dyn Observer,
}

impl std::fmt::Debug for Keeper<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keeper")
            .field("lease", &self.lease)
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl<'a> Keeper<'a> {
    /// Keys in `records`, leased for `lease` and kept for `ttl`, a failure
    /// kept as `render` writes it; repeats are told to nobody until
    /// [`Self::observed_by`].
    pub fn new(
        records: &'a Arc<dyn IdempotencyRecords>,
        lease: Duration,
        ttl: Duration,
        render: RenderError,
    ) -> Self {
        Self {
            records,
            lease,
            ttl,
            render,
            observer: &(),
        }
    }

    /// Tell `observer` (the adapter's metrics) each repeat's outcome.
    #[must_use]
    pub fn observed_by(mut self, observer: &'a dyn Observer) -> Self {
        self.observer = observer;
        self
    }
}

/// Run `operation` for `caller`'s tenant under `key` (see the
/// [module](self)), or plainly without one. `caller` must be one `authz`
/// made ([`Authorizer::admit`]): the key is scoped to its tenant, which a
/// forged [`Caller`] could name at will. `fingerprint` is what the request
/// asks ([`Fingerprint`]: its operation and canonical input, the same
/// through every adapter); `operation` must do nothing before it is
/// polled: a repeat of a kept or running request never polls it.
///
/// With a key:
///
/// - free: the operation runs, and its answer is kept (and returned), or
///   its error is returned, kept as `keeper` renders it when it may have
///   been sent, the key released when it proves nothing was;
/// - another request's record: the operation never runs; the kept answer
///   comes back [`Answer::replayed`] (a kept failure's status and body
///   included), or the repeat's error (`422 idempotency_key_reused`,
///   `409 idempotency_in_progress`, `409 outcome_unknown`), its outcome
///   told to `keeper`'s observer either way.
///
/// Dropped while the operation runs (the request cut at its deadline),
/// the key is settled as a `504 timeout` that may have been sent, from a
/// task of its own ([`Claim`]).
///
/// # Errors
///
/// `caller` another [`Authorizer`] made: `403 forbidden`, before the key
/// is read. A repeat's error, as above; no random claim id (`500
/// internal`) or the store failing (`503 storage_unavailable`), before
/// the operation runs; the operation's own error.
pub async fn run<F>(
    keeper: &Keeper<'_>,
    authz: &Authorizer,
    caller: &Caller,
    key: Option<IdempotencyKey>,
    fingerprint: &Fingerprint,
    operation: F,
) -> Result<Answer, ServiceError>
where
    F: Future<Output = Result<Answer, ServiceError>>,
{
    authz.admit(caller)?;
    Box::pin(run_scoped(
        keeper,
        caller.tenant(),
        key,
        fingerprint,
        operation,
    ))
    .await
}

/// [`run`] for an operator: `operation` for `admin` under `key`, in the
/// records of the admin key's own namespace (roadmap M5j2, design D290):
/// the stored name is `admin/<key id>` (`TenantId::admin_scope`, crate-private), which
/// no tenant id can spell, so an admin key's record is never a tenant's
/// and a tenant's never an admin key's, with the same key string; two
/// admin keys do not share theirs either. `admin` must be one `authz`
/// made. Everything else is [`run`]'s: the fingerprint ([`Fingerprint`]'s
/// admin constructors: the operation and the canonical body, never the
/// request's path), the kept answers (the adapter's answer only, never the
/// request: a submission's documents are in no record), the settling and
/// the cut request.
///
/// # Errors
///
/// `admin` another [`Authorizer`] made: `403 forbidden`, before the key is
/// read. As [`run`] otherwise.
pub async fn run_admin<F>(
    keeper: &Keeper<'_>,
    authz: &Authorizer,
    admin: &AdminCaller,
    key: Option<IdempotencyKey>,
    fingerprint: &Fingerprint,
    operation: F,
) -> Result<Answer, ServiceError>
where
    F: Future<Output = Result<Answer, ServiceError>>,
{
    authz.admit_admin(admin)?;
    let scope = TenantId::admin_scope(admin.key_id());
    Box::pin(run_scoped(keeper, &scope, key, fingerprint, operation)).await
}

/// [`run`] and [`run_admin`] after their admission, in `scope`'s records.
async fn run_scoped<F>(
    keeper: &Keeper<'_>,
    scope: &TenantId,
    key: Option<IdempotencyKey>,
    fingerprint: &Fingerprint,
    operation: F,
) -> Result<Answer, ServiceError>
where
    F: Future<Output = Result<Answer, ServiceError>>,
{
    let Some(key) = key else {
        return operation.await;
    };
    let admission = claim(
        keeper.records,
        scope,
        key,
        fingerprint,
        keeper.lease,
        keeper.ttl,
        keeper.render,
    )
    .await?;
    match admission {
        Admission::Repeat(repeat) => {
            keeper.observer.idempotency(repeat.outcome());
            let (status, body) = repeat.into_answer()?;
            Ok(Answer {
                status,
                body,
                kept: None,
                replayed: true,
            })
        }
        Admission::Claimed(claim) => match operation.await {
            Ok(answer) => {
                claim
                    .settle(Outcome::Answered {
                        status: answer.status,
                        body: answer.kept.as_deref().unwrap_or(&answer.body),
                    })
                    .await;
                Ok(Answer {
                    kept: None,
                    replayed: false,
                    ..answer
                })
            }
            Err(error) => {
                claim.settle(Outcome::Failed(&error)).await;
                Err(error)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::store::StoreResult;

    /// What a settle did to the records.
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Complete(u16, Vec<u8>),
        Release,
    }

    /// Records that claim every key and remember how each was settled,
    /// and the fingerprint each claim stored.
    #[derive(Debug, Default)]
    struct Recorded(Mutex<Vec<Call>>, Mutex<Vec<[u8; 32]>>);

    #[async_trait]
    impl IdempotencyRecords for Recorded {
        async fn claim_idempotency_key(
            &self,
            _: &TenantId,
            _: &IdempotencyKey,
            fingerprint: &[u8; 32],
            _: &str,
            _: Duration,
            _: Duration,
        ) -> StoreResult<IdempotencyClaim> {
            self.1.lock().unwrap().push(*fingerprint);
            Ok(IdempotencyClaim::Claimed)
        }

        async fn complete_idempotency_key(
            &self,
            _: &TenantId,
            _: &IdempotencyKey,
            _: &str,
            status: u16,
            body: &[u8],
        ) -> StoreResult<bool> {
            self.0
                .lock()
                .unwrap()
                .push(Call::Complete(status, body.to_vec()));
            Ok(true)
        }

        async fn release_idempotency_key(
            &self,
            _: &TenantId,
            _: &IdempotencyKey,
            _: &str,
        ) -> StoreResult<bool> {
            self.0.lock().unwrap().push(Call::Release);
            Ok(true)
        }

        async fn purge_idempotency_keys(&self) -> StoreResult<u64> {
            Ok(0)
        }
    }

    fn render(error: &ServiceError) -> Vec<u8> {
        error.code().as_bytes().to_vec()
    }

    fn pn(id: &str) -> PhoneNumberId {
        PhoneNumberId::new(id).unwrap()
    }

    fn waba(id: &str) -> WabaId {
        WabaId::new(id).unwrap()
    }

    async fn claimed(records: &Arc<Recorded>) -> Claim {
        let records: Arc<dyn IdempotencyRecords> = records.clone();
        let admission = claim(
            &records,
            &TenantId::parse("tenant-a").unwrap(),
            IdempotencyKey::parse("order:1").unwrap(),
            &Fingerprint::send_message(&pn("1"), &serde_json::json!({})),
            Duration::from_secs(60),
            Duration::from_secs(3600),
            render,
        )
        .await
        .unwrap();
        let Admission::Claimed(claim) = admission else {
            panic!("not claimed")
        };
        claim
    }

    /// A success and a failure that may have been sent are kept; a failure
    /// that proves nothing was sent releases the key. Decisive: the
    /// `may_have_been_sent` guard.
    #[tokio::test]
    async fn a_claim_settles_by_what_the_outcome_proves() {
        let records = Arc::new(Recorded::default());
        claimed(&records)
            .await
            .settle(Outcome::Answered {
                status: 202,
                body: b"{}",
            })
            .await;
        let sent = ServiceError::new(ErrorCode::TIMEOUT).with_may_have_been_sent(true);
        claimed(&records).await.settle(Outcome::Failed(&sent)).await;
        let refused = ServiceError::new(ErrorCode::CUSTOMER_SERVICE_WINDOW_CLOSED);
        claimed(&records)
            .await
            .settle(Outcome::Failed(&refused))
            .await;
        assert_eq!(
            *records.0.lock().unwrap(),
            [
                Call::Complete(202, b"{}".to_vec()),
                Call::Complete(504, b"timeout".to_vec()),
                Call::Release,
            ]
        );
    }

    /// A claim stores the request's own fingerprint, never the legacy one
    /// it also matches (`or_legacy`): a record claimed after the upgrade
    /// is the new fingerprint's, and replays through every spelling of its
    /// path. Decisive: `claim` passing `Fingerprint::as_bytes` (the legacy
    /// digest stored instead is a key that only its first spelling
    /// replays).
    #[tokio::test]
    async fn a_claim_stores_the_new_fingerprint_never_the_legacy_one() {
        let records = Arc::new(Recorded::default());
        let ports: Arc<dyn IdempotencyRecords> = records.clone();
        let body = serde_json::json!({"to": {"phone": "+1"}, "type": "text"});
        let new = Fingerprint::send_message(&pn("1"), &body);
        let legacy = Fingerprint::legacy_v1_json("POST", "/v1/numbers/%31/messages", &body);
        let admission = claim(
            &ports,
            &TenantId::parse("tenant-a").unwrap(),
            IdempotencyKey::parse("order:1").unwrap(),
            &new.or_legacy(legacy),
            Duration::from_secs(60),
            Duration::from_secs(3600),
            render,
        )
        .await
        .unwrap();
        let Admission::Claimed(claim) = admission else {
            panic!("not claimed")
        };
        claim
            .settle(Outcome::Answered {
                status: 202,
                body: b"{}",
            })
            .await;
        assert_eq!(*records.1.lock().unwrap(), [*new.as_bytes()]);
        assert_ne!(new.as_bytes(), legacy.as_bytes());
    }

    /// A claim dropped before it settled keeps a `504 timeout`, from a task
    /// of its own. Decisive: the `Drop`.
    #[tokio::test]
    async fn a_claim_cut_before_it_settled_is_kept_as_a_timeout() {
        let records = Arc::new(Recorded::default());
        drop(claimed(&records).await);
        for _ in 0..100 {
            if !records.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            *records.0.lock().unwrap(),
            [Call::Complete(504, b"timeout".to_vec())]
        );
    }

    /// A repeat's answer follows the record: another request's fingerprint
    /// first, then its state.
    #[test]
    fn a_repeat_is_answered_by_the_record_it_met() {
        let mine = Fingerprint::send_message(&pn("1"), &serde_json::json!({}));
        let record = |fingerprint: &Fingerprint, state| IdempotencyRecord {
            fingerprint: *fingerprint.as_bytes(),
            state,
        };
        let other = Fingerprint::send_message(&pn("2"), &serde_json::json!({}));
        let done = IdempotencyState::Completed {
            status: 202,
            body: b"{}".to_vec(),
        };
        assert_eq!(
            Repeat::of(record(&other, done.clone()), &mine),
            Repeat::Reused
        );
        assert_eq!(
            Repeat::of(
                record(
                    &mine,
                    IdempotencyState::InProgress {
                        lease_expired: false
                    }
                ),
                &mine
            ),
            Repeat::InProgress
        );
        assert_eq!(
            Repeat::of(
                record(
                    &mine,
                    IdempotencyState::InProgress {
                        lease_expired: true
                    }
                ),
                &mine
            ),
            Repeat::OutcomeUnknown
        );
        let replay = Repeat::of(record(&mine, done), &mine);
        assert_eq!(replay.outcome(), "replayed");
        assert_eq!(replay.into_answer().unwrap(), (202, b"{}".to_vec()));
        let in_progress = Repeat::InProgress.into_answer().unwrap_err();
        assert_eq!(in_progress.code(), "idempotency_in_progress");
        assert!(in_progress.is_retryable() && in_progress.may_have_been_sent());
        let unknown = Repeat::OutcomeUnknown.into_answer().unwrap_err();
        assert_eq!(unknown.code(), "outcome_unknown");
        assert!(!unknown.is_retryable() && unknown.may_have_been_sent());
        assert_eq!(
            Repeat::Reused.into_answer().unwrap_err().code(),
            "idempotency_key_reused"
        );
    }

    /// Fingerprints are stored with their key (`wa_server_idempotency`,
    /// kept `WA_SERVER_IDEMPOTENCY_TTL`): derived otherwise after an
    /// upgrade, a caller repeating a request across it with the same key
    /// gets `422 idempotency_key_reused` until the record expires (roadmap
    /// S6 changed them so: the operation and its canonical input, no
    /// method and no path). Known answers, computed apart from this code:
    //
    // python3 - <<'EOF'
    // import hashlib, struct
    // def framed(h, b): h.update(struct.pack('>Q', len(b))); h.update(b)
    // def canonical(h, v):
    //     if v is None: h.update(b'n')
    //     elif isinstance(v, bool): h.update(b't' if v else b'f')
    //     elif isinstance(v, int): h.update(b'#'); framed(h, str(v).encode())
    //     elif isinstance(v, str): h.update(b's'); framed(h, v.encode())
    //     elif isinstance(v, list):
    //         h.update(b'[' + struct.pack('>Q', len(v)))
    //         for x in v: canonical(h, x)
    //     else:
    //         h.update(b'{' + struct.pack('>Q', len(v)))
    //         for k in sorted(v, key=str.encode): framed(h, k.encode()); canonical(h, v[k])
    // def begin(kind, operation, params):
    //     h = hashlib.sha256(); framed(h, kind); framed(h, operation)
    //     params = sorted(params); h.update(struct.pack('>Q', len(params)))
    //     for name, value in params: framed(h, name); framed(h, value)
    //     return h
    // body = {"to": {"phone": "+15551234567"}, "type": "text",
    //     "text": {"body": "hi", "preview_url": False}, "tags": [None, True, 1, "1"]}
    // h = begin(b'json', b'send_message', [(b'pn', b'106540352242922')])
    // canonical(h, body); print(h.hexdigest())
    // h = begin(b'json', b'create_template', [(b'waba_id', b'102290129340398')])
    // canonical(h, body); print(h.hexdigest())
    // parts = [(b'type', b'image/png'), (b'filename', b'photo.png'),
    //     (b'file', hashlib.sha256(b'\x89PNG').digest())]
    // h = begin(b'parts', b'upload_media', [(b'pn', b'106540352242922')])
    // h.update(struct.pack('>Q', len(parts)))
    // for name, value in parts: framed(h, name); framed(h, value)
    // print(h.hexdigest())
    // EOF
    //
    // Decisive: each tag (`json`, `parts`, `n`, `t`, `#`, `s`, `[`, `{`),
    // the operation's id, the parameters' names and values, the counts,
    // the length framing, and the order of an object's keys.
    #[test]
    fn fingerprints_are_pinned() {
        let body = serde_json::json!({
            "to": {"phone": "+15551234567"},
            "type": "text",
            "text": {"body": "hi", "preview_url": false},
            "tags": [null, true, 1, "1"],
        });
        let sent = Fingerprint::send_message(&pn("106540352242922"), &body);
        assert_eq!(
            hex::encode(sent.as_bytes()),
            "e28c48a4f2d9f179aa84d67b123d9909bd2c5600e2c4b2a691eb11ce289827af"
        );
        let created = Fingerprint::create_template(&waba("102290129340398"), &body);
        assert_eq!(
            hex::encode(created.as_bytes()),
            "051c1640403757307a991cd1de00f7fbd28f488cb988ea9fad7ff4e62d44258a"
        );
        let upload =
            Upload::new("image/png", Some("photo.png"), b"\x89PNG".to_vec().into()).unwrap();
        let uploaded = Fingerprint::upload_media(&pn("106540352242922"), &upload);
        assert_eq!(
            hex::encode(uploaded.as_bytes()),
            "05c407ac85b1fd62964b4d98cc2263326cbd8c2aa43a4718f9bec755a68c200d"
        );
    }

    /// Roadmap M3c: an OTP issue's fingerprint is keyed. Its body names a
    /// phone number, which a bare SHA-256 would give away to whoever reads
    /// the idempotency records and tries the numbers of a country (design
    /// section 6: numbers only as keyed hashes). It is the HMAC-SHA256,
    /// under HMAC-SHA256(pepper, `meta-whatsapp-server/otp-fingerprint/v1`),
    /// of the canonical input's digest (derived here apart from the code:
    /// the label is a stored name, pending the owner). Decisive: an unkeyed
    /// fingerprint, or one keyed otherwise, fails it.
    #[test]
    fn the_otp_fingerprint_is_keyed() {
        use hmac::{Hmac, Mac};
        use meta_whatsapp_rs::core::secret::SecretBytes;
        let body = serde_json::json!({"phone": "+16505551234", "purpose": "login"});
        let pepper = [7u8; 32];
        let keys = OtpKeys::new(SecretBytes::new(pepper.to_vec())).unwrap();
        let other = OtpKeys::new(SecretBytes::new(vec![8u8; 32])).unwrap();
        let keyed = Fingerprint::issue_otp(&keys, &body);
        let plain = Fingerprint::json(Operation::IssueOtp, &[], &body);
        assert_ne!(keyed.as_bytes(), plain.as_bytes(), "keyed");
        assert_ne!(
            keyed.as_bytes(),
            Fingerprint::issue_otp(&other, &body).as_bytes(),
            "by the pepper"
        );
        let mac = |key: &[u8], message: &[u8]| -> [u8; 32] {
            let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
            mac.update(message);
            mac.finalize().into_bytes().into()
        };
        let key = mac(&pepper, b"meta-whatsapp-server/otp-fingerprint/v1");
        assert_eq!(*keyed.as_bytes(), mac(&key, plain.as_bytes()));
        // Another body, another fingerprint; the same body, the same one.
        let again = serde_json::json!({"purpose": "login", "phone": "+16505551234"});
        assert_eq!(keyed, Fingerprint::issue_otp(&keys, &again));
        let another = serde_json::json!({"phone": "+16505551235", "purpose": "login"});
        assert_ne!(keyed, Fingerprint::issue_otp(&keys, &another));
    }

    /// The legacy fingerprint is exactly the one stored before roadmap S6:
    /// main's known answers (`fingerprints_are_pinned` before S6, computed
    /// apart from the code, at f1d125b), for a JSON body and for parts; an
    /// upload's is its three parts, in main's order. Decisive: any change
    /// to the legacy algorithm (a record stored before the upgrade would
    /// stop replaying).
    #[test]
    fn the_legacy_fingerprint_is_mains() {
        let body = serde_json::json!({
            "to": {"phone": "+15551234567"},
            "type": "text",
            "text": {"body": "hi", "preview_url": false},
            "tags": [null, true, 1, "1"],
        });
        let json =
            Fingerprint::legacy_v1_json("POST", "/v1/numbers/106540352242922/messages", &body);
        assert_eq!(
            hex::encode(json.as_bytes()),
            "e83cc62715cb87890e03ddaa5d57b655ced695738930fc8c2ca164af6139f7d4"
        );
        let parts = Fingerprint::legacy_v1_parts(
            "POST",
            "/v1/numbers/106540352242922/media",
            &[("type", b"image/png"), ("file", b"\x89PNG")],
        );
        assert_eq!(
            hex::encode(parts.as_bytes()),
            "04edb1c222d03832376711c1ba9450a54e08c14793501b90e48af0fc13788db9"
        );
        let upload = Upload::new("image/png", Some("a.png"), b"\x89PNG".to_vec().into()).unwrap();
        let digest = Sha256::digest(b"\x89PNG");
        assert_eq!(
            Fingerprint::legacy_v1_upload("POST", "/v1/numbers/1/media", &upload),
            Fingerprint::legacy_v1_parts(
                "POST",
                "/v1/numbers/1/media",
                &[
                    ("type", b"image/png"),
                    ("filename", b"a.png"),
                    ("file", digest.as_slice()),
                ],
            )
        );
        // The method's name, whatever type carries it.
        assert_eq!(
            Fingerprint::legacy_v1_json("POST", "/p", &body),
            Fingerprint::legacy_v1_json(&String::from("POST"), "/p", &body)
        );
    }

    /// A record stored under the legacy fingerprint (a key claimed before
    /// the upgrade) is the same request's when the request adds its legacy
    /// fingerprint: replayed, in progress or unknown as its state says.
    /// Without it, or with another body's legacy fingerprint, it is another
    /// request's (`Reused`). What the request stores is its new
    /// fingerprint. Decisive: the legacy arm of `Fingerprint::matches`, and
    /// its comparison with the record.
    #[test]
    fn a_record_under_the_legacy_fingerprint_is_the_same_requests() {
        let body = serde_json::json!({"to": {"phone": "+1"}, "type": "text"});
        let path = "/v1/numbers/1/messages";
        let new = Fingerprint::send_message(&pn("1"), &body);
        let legacy = Fingerprint::legacy_v1_json("POST", path, &body);
        let request = new.or_legacy(legacy);
        assert_eq!(
            request.as_bytes(),
            new.as_bytes(),
            "a claim stores the new one"
        );
        assert_ne!(new.as_bytes(), legacy.as_bytes());
        let stored = |state| IdempotencyRecord {
            fingerprint: *legacy.as_bytes(),
            state,
        };
        let done = IdempotencyState::Completed {
            status: 202,
            body: b"{}".to_vec(),
        };
        assert_eq!(
            Repeat::of(stored(done.clone()), &request),
            Repeat::Replay {
                status: 202,
                body: b"{}".to_vec()
            }
        );
        assert_eq!(
            Repeat::of(
                stored(IdempotencyState::InProgress {
                    lease_expired: false
                }),
                &request
            ),
            Repeat::InProgress
        );
        assert_eq!(Repeat::of(stored(done.clone()), &new), Repeat::Reused);
        let other = serde_json::json!({"to": {"phone": "+2"}, "type": "text"});
        let elsewhere = Fingerprint::send_message(&pn("1"), &other)
            .or_legacy(Fingerprint::legacy_v1_json("POST", path, &other));
        assert_eq!(Repeat::of(stored(done.clone()), &elsewhere), Repeat::Reused);
        // A new record still matches the request, legacy or not.
        let fresh = IdempotencyRecord {
            fingerprint: *new.as_bytes(),
            state: done,
        };
        assert!(matches!(Repeat::of(fresh, &request), Repeat::Replay { .. }));
    }

    /// A fingerprint is the operation and its input, whatever spelled
    /// them: key order and whitespace do not change it, nor the order the
    /// parameters are given in; another operation, another number or
    /// WABA, another value does. Parts are framed (moving a byte between
    /// them changes the digest), and a string never collides with a
    /// number.
    #[test]
    fn a_fingerprint_is_the_operation_and_its_input() {
        let a: serde_json::Value = serde_json::from_str(
            r#"{"to": {"phone": "+1"}, "type": "text", "text": {"body": "hi"}}"#,
        )
        .unwrap();
        let b: serde_json::Value =
            serde_json::from_str(r#"{"text":{"body":"hi"},"type":"text","to":{"phone":"+1"}}"#)
                .unwrap();
        let one = pn("1");
        assert_eq!(
            Fingerprint::send_message(&one, &a),
            Fingerprint::send_message(&one, &b)
        );
        let ho = serde_json::json!({"to": {"phone": "+1"}, "type": "text", "text": {"body": "ho"}});
        assert_ne!(
            Fingerprint::send_message(&one, &a),
            Fingerprint::send_message(&one, &ho)
        );
        assert_ne!(
            Fingerprint::send_message(&one, &a),
            Fingerprint::send_message(&pn("2"), &a)
        );
        // Another operation on the same id and body.
        assert_ne!(
            Fingerprint::send_message(&one, &a),
            Fingerprint::create_template(&waba("1"), &a)
        );
        assert_ne!(
            Fingerprint::json(Operation::SendMessage, &[("pn", "1")], &a),
            Fingerprint::json(Operation::CreateTemplate, &[("pn", "1")], &a)
        );
        // A conversation's read and a reply to it, on one contact and body
        // (roadmap M2g); another contact is another read.
        let read = serde_json::json!({"notify_customer": true});
        assert_ne!(
            Fingerprint::mark_conversation_read(&one, "US.1", &read),
            Fingerprint::reply_to_conversation(&one, "US.1", &read)
        );
        assert_ne!(
            Fingerprint::mark_conversation_read(&one, "US.1", &read),
            Fingerprint::mark_conversation_read(&one, "US.2", &read)
        );
        assert_ne!(
            Fingerprint::mark_conversation_read(&one, "US.1", &read),
            Fingerprint::mark_conversation_read(&one, "US.1", &serde_json::Value::Null)
        );
        // The parameters, by name, in any order.
        assert_eq!(
            Fingerprint::json(Operation::SendMessage, &[("a", "1"), ("b", "2")], &a),
            Fingerprint::json(Operation::SendMessage, &[("b", "2"), ("a", "1")], &a)
        );
        assert_ne!(
            Fingerprint::json(Operation::SendMessage, &[("a", "1"), ("b", "2")], &a),
            Fingerprint::json(Operation::SendMessage, &[("a", "2"), ("b", "1")], &a)
        );
        assert_ne!(
            Fingerprint::json(Operation::SendMessage, &[("pn", "12")], &a),
            Fingerprint::json(Operation::SendMessage, &[("pn", "1"), ("n", "2")], &a)
        );
        // Framing: moving a byte between parts changes the digest.
        let op = Operation::UploadMedia;
        assert_ne!(
            Fingerprint::parts(op, &[], &[("type", b"ab"), ("file", b"c")]),
            Fingerprint::parts(op, &[], &[("type", b"a"), ("file", b"bc")])
        );
        // Strings and numbers never collide.
        assert_ne!(
            Fingerprint::send_message(&one, &serde_json::json!("1")),
            Fingerprint::send_message(&one, &serde_json::json!(1))
        );
        // An upload's type, file name and file each count.
        let png = |name: &str, data: &[u8]| {
            let upload = Upload::new("image/png", Some(name), data.to_vec().into()).unwrap();
            Fingerprint::upload_media(&one, &upload)
        };
        assert_eq!(png("a.png", b"x"), png("a.png", b"x"));
        assert_ne!(png("a.png", b"x"), png("b.png", b"x"));
        assert_ne!(png("a.png", b"x"), png("a.png", b"y"));
        let jpeg = Upload::new("image/jpeg", Some("a.png"), b"x".to_vec().into()).unwrap();
        assert_ne!(png("a.png", b"x"), Fingerprint::upload_media(&one, &jpeg));
    }

    /// Roadmap M5b's keyed creations are scoped to the WABA, the body and
    /// the operation: the same key and body in another WABA, with another
    /// body, or on another of the three, is another request (so a reused
    /// key is `idempotency_key_reused`, never a replay of another WABA's
    /// answer). Decisive: the `waba_id` parameter and the operation of
    /// each constructor, and the body's canonical form.
    #[test]
    fn the_template_lifecycle_fingerprints_are_per_waba_body_and_operation() {
        let body = serde_json::json!({"source_waba_id": "1", "name": "x"});
        let other = serde_json::json!({"source_waba_id": "2", "name": "x"});
        let reordered = serde_json::json!({"name": "x", "source_waba_id": "1"});
        let constructors: [fn(&WabaId, &serde_json::Value) -> Fingerprint; 3] = [
            Fingerprint::create_library_template,
            Fingerprint::migrate_templates,
            Fingerprint::create_template_group,
        ];
        for build in constructors {
            assert_eq!(build(&waba("1"), &body), build(&waba("1"), &reordered));
            assert_ne!(build(&waba("1"), &body), build(&waba("2"), &body));
            assert_ne!(build(&waba("1"), &body), build(&waba("1"), &other));
            assert_ne!(
                build(&waba("1"), &body),
                Fingerprint::create_template(&waba("1"), &body)
            );
        }
        for (i, one) in constructors.iter().enumerate() {
            for other_op in &constructors[i + 1..] {
                assert_ne!(one(&waba("1"), &body), other_op(&waba("1"), &body));
            }
        }
    }

    /// Roadmap M5g's keyed writes are scoped to the number, the body and
    /// the operation: the same key and body on another number, with
    /// another body, or on another of the three, is another request (a
    /// reused key is `idempotency_key_reused`, never a replay of another
    /// number's answer). Decisive: the `pn` parameter and the operation of
    /// each constructor, and the body's canonical form.
    #[test]
    fn the_qr_code_and_block_list_fingerprints_are_per_number_body_and_operation() {
        let body = serde_json::json!({"users": [{"phone": "+1"}], "n": 1});
        let other = serde_json::json!({"users": [{"phone": "+2"}], "n": 1});
        let reordered = serde_json::json!({"n": 1, "users": [{"phone": "+1"}]});
        let constructors: [fn(&PhoneNumberId, &serde_json::Value) -> Fingerprint; 3] = [
            Fingerprint::create_qr_code,
            Fingerprint::block_users,
            Fingerprint::unblock_users,
        ];
        for build in constructors {
            assert_eq!(build(&pn("1"), &body), build(&pn("1"), &reordered));
            assert_ne!(build(&pn("1"), &body), build(&pn("2"), &body));
            assert_ne!(build(&pn("1"), &body), build(&pn("1"), &other));
            assert_ne!(
                build(&pn("1"), &body),
                Fingerprint::send_message(&pn("1"), &body)
            );
        }
        for (i, one) in constructors.iter().enumerate() {
            for other_op in &constructors[i + 1..] {
                assert_ne!(one(&pn("1"), &body), other_op(&pn("1"), &body));
            }
        }
    }

    /// Roadmap M5l's keyed thread actions are scoped to the number, the body
    /// and the operation: the same key and body on another number, with
    /// another body, or as another action, is another request (a pass is
    /// never replayed as a release). Decisive: the `pn` parameter and the
    /// operation of each constructor, and the body's canonical form.
    #[test]
    fn the_thread_action_fingerprints_are_per_number_body_and_operation() {
        let body = serde_json::json!({"user": {"phone": "+1"}, "target_role": "ctwa"});
        let other = serde_json::json!({"user": {"phone": "+2"}, "target_role": "ctwa"});
        let reordered = serde_json::json!({"target_role": "ctwa", "user": {"phone": "+1"}});
        let constructors: [fn(&PhoneNumberId, &serde_json::Value) -> Fingerprint; 3] = [
            Fingerprint::pass_thread,
            Fingerprint::take_thread,
            Fingerprint::release_thread,
        ];
        for build in constructors {
            assert_eq!(build(&pn("1"), &body), build(&pn("1"), &reordered));
            assert_ne!(build(&pn("1"), &body), build(&pn("2"), &body));
            assert_ne!(build(&pn("1"), &body), build(&pn("1"), &other));
            assert_ne!(
                build(&pn("1"), &body),
                Fingerprint::send_message(&pn("1"), &body)
            );
        }
        for (i, one) in constructors.iter().enumerate() {
            for other_op in &constructors[i + 1..] {
                assert_ne!(one(&pn("1"), &body), other_op(&pn("1"), &body));
            }
        }
    }

    /// Roadmap M5h's three keyed creations are scoped to the number or the
    /// WABA, the body and the operation: the same key and body on another
    /// number or WABA, with another body, or as another operation (a
    /// duplicate is never a sequence's creation) is another request.
    /// Decisive: the parameter and the operation of each constructor, and
    /// the body's canonical form.
    #[test]
    fn the_marketing_fingerprints_are_per_number_or_waba_body_and_operation() {
        let body = serde_json::json!({"name": "x", "template": {"a": 1}});
        let other = serde_json::json!({"name": "y", "template": {"a": 1}});
        let reordered = serde_json::json!({"template": {"a": 1}, "name": "x"});
        assert_eq!(
            Fingerprint::send_marketing_message(&pn("1"), &body),
            Fingerprint::send_marketing_message(&pn("1"), &reordered)
        );
        assert_ne!(
            Fingerprint::send_marketing_message(&pn("1"), &body),
            Fingerprint::send_marketing_message(&pn("2"), &body)
        );
        assert_ne!(
            Fingerprint::send_marketing_message(&pn("1"), &body),
            Fingerprint::send_marketing_message(&pn("1"), &other)
        );
        assert_ne!(
            Fingerprint::send_marketing_message(&pn("1"), &body),
            Fingerprint::send_message(&pn("1"), &body)
        );
        let wabas: [fn(&WabaId, &serde_json::Value) -> Fingerprint; 2] = [
            Fingerprint::duplicate_marketing_template,
            Fingerprint::create_welcome_sequence,
        ];
        for build in wabas {
            assert_eq!(build(&waba("1"), &body), build(&waba("1"), &reordered));
            assert_ne!(build(&waba("1"), &body), build(&waba("2"), &body));
            assert_ne!(build(&waba("1"), &body), build(&waba("1"), &other));
            assert_ne!(
                build(&waba("1"), &body),
                Fingerprint::create_flow(&waba("1"), &body)
            );
        }
        assert_ne!(
            Fingerprint::duplicate_marketing_template(&waba("1"), &body),
            Fingerprint::create_welcome_sequence(&waba("1"), &body)
        );
    }

    /// Roadmap M5f's keyed creation is scoped to the number, the body and
    /// the operation: the same key and body on another number, with another
    /// body, or as another operation, is another request. Decisive: the
    /// `pn` parameter and the operation of `Fingerprint::create_group`, and
    /// the body's canonical form.
    #[test]
    fn the_group_creation_fingerprint_is_per_number_body_and_operation() {
        let body = serde_json::json!({"subject": "Cars", "description": "d"});
        let other = serde_json::json!({"subject": "Cars", "description": "e"});
        let reordered = serde_json::json!({"description": "d", "subject": "Cars"});
        let build = Fingerprint::create_group;
        assert_eq!(build(&pn("1"), &body), build(&pn("1"), &reordered));
        assert_ne!(build(&pn("1"), &body), build(&pn("2"), &body));
        assert_ne!(build(&pn("1"), &body), build(&pn("1"), &other));
        assert_ne!(
            build(&pn("1"), &body),
            Fingerprint::send_message(&pn("1"), &body)
        );
    }

    /// Roadmap M5e's keyed call is scoped to the number, the body and the
    /// operation: the same key and offer on another number, with another
    /// body, or as another operation, is another request (never a second
    /// ring from a replayed key). Decisive: the `pn` parameter and the
    /// operation of `Fingerprint::connect_call`, and the body's canonical
    /// form.
    #[test]
    fn the_call_fingerprint_is_per_number_body_and_operation() {
        let body = serde_json::json!({"to": {"phone": "+1"}, "sdp_offer": "v=0"});
        let other = serde_json::json!({"to": {"phone": "+2"}, "sdp_offer": "v=0"});
        let reordered = serde_json::json!({"sdp_offer": "v=0", "to": {"phone": "+1"}});
        let build = Fingerprint::connect_call;
        assert_eq!(build(&pn("1"), &body), build(&pn("1"), &reordered));
        assert_ne!(build(&pn("1"), &body), build(&pn("2"), &body));
        assert_ne!(build(&pn("1"), &body), build(&pn("1"), &other));
        assert_ne!(
            build(&pn("1"), &body),
            Fingerprint::create_group(&pn("1"), &body)
        );
    }

    /// Roadmap M5c2's keyed display name request is scoped to the number,
    /// the body and the operation. Decisive: the `pn` parameter and the
    /// operation of `Fingerprint::request_display_name_change`, and the
    /// body's canonical form.
    #[test]
    fn the_display_name_request_fingerprint_is_per_number_body_and_operation() {
        let body = serde_json::json!({"new_display_name": "Lucky Shrub", "x": 1});
        let other = serde_json::json!({"new_display_name": "Lucky Shrubs", "x": 1});
        let reordered = serde_json::json!({"x": 1, "new_display_name": "Lucky Shrub"});
        let build = Fingerprint::request_display_name_change;
        assert_eq!(build(&pn("1"), &body), build(&pn("1"), &reordered));
        assert_ne!(build(&pn("1"), &body), build(&pn("2"), &body));
        assert_ne!(build(&pn("1"), &body), build(&pn("1"), &other));
        assert_ne!(
            build(&pn("1"), &body),
            Fingerprint::create_group(&pn("1"), &body)
        );
    }

    /// Roadmap M5c3's keyed application is scoped to the number, the body
    /// and the operation. Decisive: the `pn` parameter, the operation and
    /// the body's canonical form.
    #[test]
    fn the_oba_application_fingerprint_is_per_number_body_and_operation() {
        let body = serde_json::json!({"business_website_url": "https://a.example", "x": 1});
        let other = serde_json::json!({"business_website_url": "https://b.example", "x": 1});
        let reordered = serde_json::json!({"x": 1, "business_website_url": "https://a.example"});
        let build = Fingerprint::request_official_business_account;
        assert_eq!(build(&pn("1"), &body), build(&pn("1"), &reordered));
        assert_ne!(build(&pn("1"), &body), build(&pn("2"), &body));
        assert_ne!(build(&pn("1"), &body), build(&pn("1"), &other));
        assert_ne!(
            build(&pn("1"), &body),
            Fingerprint::request_display_name_change(&pn("1"), &body)
        );
    }

    /// Roadmap M5d's keyed creation is scoped to the WABA, the body and the
    /// operation. Decisive: the `waba_id` parameter, the operation, and the
    /// body's canonical form.
    #[test]
    fn the_flow_creation_fingerprint_is_per_waba_body_and_operation() {
        let body = serde_json::json!({"name": "x", "categories": ["OTHER"]});
        let other = serde_json::json!({"name": "y", "categories": ["OTHER"]});
        let reordered = serde_json::json!({"categories": ["OTHER"], "name": "x"});
        assert_eq!(
            Fingerprint::create_flow(&waba("1"), &body),
            Fingerprint::create_flow(&waba("1"), &reordered)
        );
        assert_ne!(
            Fingerprint::create_flow(&waba("1"), &body),
            Fingerprint::create_flow(&waba("2"), &body)
        );
        assert_ne!(
            Fingerprint::create_flow(&waba("1"), &body),
            Fingerprint::create_flow(&waba("1"), &other)
        );
        assert_ne!(
            Fingerprint::create_flow(&waba("1"), &body),
            Fingerprint::create_template(&waba("1"), &body)
        );
    }

    /// Roadmap M5j2's operator fingerprints are the operation and the body's
    /// canonical form, of a kind of their own. Decisive: the operation id
    /// (dropped, the two routes share one for one body), the body (dropped,
    /// any body replays), and the `admin-json` kind (the pinned digests).
    #[test]
    fn the_operator_fingerprints_are_per_body_operation_and_kind() {
        let body = serde_json::json!({"name": "x", "partner_app_id": "1"});
        let other = serde_json::json!({"name": "y", "partner_app_id": "1"});
        let reordered = serde_json::json!({"partner_app_id": "1", "name": "x"});
        assert_eq!(
            Fingerprint::create_partner_solution(&body),
            Fingerprint::create_partner_solution(&reordered)
        );
        assert_ne!(
            Fingerprint::create_partner_solution(&body),
            Fingerprint::create_partner_solution(&other)
        );
        assert_ne!(
            Fingerprint::create_partner_solution(&body),
            Fingerprint::submit_business_verification(&body)
        );
        assert_ne!(
            Fingerprint::submit_business_verification(&body),
            Fingerprint::submit_business_verification(&other)
        );
        // Pinned, derived apart from the code as `fingerprints_are_pinned`
        // is (kind `admin-json`, the operation's id, no parameters, the
        // canonical body): the kind and the id are stored in no record but
        // decide which requests share a key's record.
        let pinned = serde_json::json!({
            "name": "x",
            "partner_app_id": "1",
            "owner_permissions": ["MESSAGING"],
        });
        assert_eq!(
            hex::encode(Fingerprint::create_partner_solution(&pinned).as_bytes()),
            "5176d82d503b69851b68b3addf4de70e8ee123334ac434c462b06ec51fe5f1eb"
        );
        assert_eq!(
            hex::encode(Fingerprint::submit_business_verification(&pinned).as_bytes()),
            "25f1ffc279026c81159aff939cd6fdf2466aa85a8c719d195b60c61c9e84a53a"
        );
    }

    /// Roadmap M5j3's fingerprints, one per route: the operation (shared
    /// between two routes, one body replays the other's answer), the body,
    /// the pool number of a code request, and the hosted claim's PIN,
    /// which is in the digest only as present. Pinned as above.
    #[test]
    fn the_pool_and_hosted_fingerprints_are_per_route_body_and_number() {
        let code = serde_json::json!({"method": "SMS", "language": "en_US"});
        let other = serde_json::json!({"method": "VOICE", "language": "en_US"});
        assert_eq!(
            Fingerprint::request_pre_verified_code("1", &code),
            Fingerprint::request_pre_verified_code("1", &code)
        );
        assert_ne!(
            Fingerprint::request_pre_verified_code("1", &code),
            Fingerprint::request_pre_verified_code("1", &other)
        );
        assert_ne!(
            Fingerprint::request_pre_verified_code("1", &code),
            Fingerprint::request_pre_verified_code("2", &code)
        );
        assert_ne!(
            Fingerprint::request_pre_verified_code("1", &code),
            Fingerprint::add_pre_verified_number(&code)
        );
        assert_ne!(
            Fingerprint::add_pre_verified_number(&code),
            Fingerprint::complete_hosted_signup(&code)
        );
        let claim = |pin: Option<&str>, tenant: &str| {
            let mut body = serde_json::json!({
                "tenant_id": tenant,
                "waba_id": "102290129340398",
                "owner_business_id": "2729063490586005",
            });
            if let Some(pin) = pin {
                body["pin"] = pin.into();
            }
            Fingerprint::complete_hosted_signup(&body)
        };
        assert_ne!(claim(None, "merchant-a"), claim(None, "merchant-b"));
        // The PIN is not hashed: another PIN is the same request; no PIN is
        // another.
        assert_eq!(
            claim(Some("123456"), "merchant-a"),
            claim(Some("654321"), "merchant-a")
        );
        assert_ne!(
            claim(Some("123456"), "merchant-a"),
            claim(None, "merchant-a")
        );
        assert_eq!(
            hex::encode(
                Fingerprint::add_pre_verified_number(
                    &serde_json::json!({"phone_number": "+1 555 078 3881"})
                )
                .as_bytes()
            ),
            "37bae751f17a79ec3c70dc60898fa8023265e05067f94cae092c2e0bf8d870dc"
        );
        assert_eq!(
            hex::encode(
                Fingerprint::request_pre_verified_code("1972385232742999", &code).as_bytes()
            ),
            "84034253ad4f9392363edfd5dc7f86656dca23c45c046f0f7168d874bf00b472"
        );
        assert_eq!(
            hex::encode(claim(Some("123456"), "merchant-a").as_bytes()),
            "8049abea2b852eef5268ca18e72cace7a3a138303a7956f8bc8fbbafdf6112c1"
        );
    }

    /// The canonical input is the JSON as parsed (D27: the request's JSON
    /// as it came, canonicalized), so two bodies share a fingerprint only
    /// when they parse to one value, which is what the operation then
    /// sends: an escape and the character it spells are one string, and
    /// of a key given twice the parser keeps the last, both for the
    /// fingerprint and for the request. What parses to two values never
    /// shares one, even when Meta might read them alike: `1` and `1.0`,
    /// a precomposed `é` and `e` with a combining accent, `0` and `-0`,
    /// `null` and an absent key. Decisive: `canonical`'s number arm
    /// reading the value as a float (`1` and `1.0` collide), and its
    /// string arm normalizing text.
    #[test]
    fn two_bodies_share_a_fingerprint_only_when_they_parse_alike() {
        let one = pn("1");
        let fp = |json: &str| {
            let body: serde_json::Value = serde_json::from_str(json).unwrap();
            Fingerprint::send_message(&one, &body)
        };
        // One value, spelled two ways.
        assert_eq!(fp(r#"{"a":"é"}"#), fp("{\"a\":\"\u{e9}\"}"));
        assert_eq!(fp(r#"{"a":1}"#), fp(r#"{"a":1}"#));
        assert_eq!(fp(r#"{"a":1,"a":2}"#), fp(r#"{"a":2}"#));
        assert_ne!(fp(r#"{"a":1,"a":2}"#), fp(r#"{"a":1}"#));
        assert_eq!(fp(r#"{ "a" : [ 1 , 2 ] }"#), fp(r#"{"a":[1,2]}"#));
        // Two values, however close.
        for (a, b) in [
            (r#"{"a":1}"#, r#"{"a":1.0}"#),
            (r#"{"a":0}"#, r#"{"a":-0}"#),
            (r#"{"a":100}"#, r#"{"a":1e2}"#),
            ("{\"a\":\"\u{e9}\"}", "{\"a\":\"e\u{301}\"}"),
            (r#"{"a":"A"}"#, r#"{"a":"a"}"#),
            (r#"{"a":null}"#, "{}"),
            (r#"{"a":[]}"#, r#"{"a":{}}"#),
            (r#"{"a":[1,2]}"#, r#"{"a":[2,1]}"#),
            (r#"{"a":[[1],2]}"#, r#"{"a":[1,[2]]}"#),
            (r#"{"a":[[1],2]}"#, r#"{"a":[[1,2]]}"#),
            (r#"{"a":{"b":1},"c":2}"#, r#"{"a":{"b":1,"c":2}}"#),
            (r#"{"a":{"b":1}}"#, r#"{"a.b":1}"#),
            (r#"{"a":"1"}"#, r#"{"a":1}"#),
            (r#"{"a":true}"#, r#"{"a":"t"}"#),
            (r#"{"ab":"c"}"#, r#"{"a":"bc"}"#),
        ] {
            assert_ne!(fp(a), fp(b), "{a} and {b}");
        }
    }

    /// `Debug` of a kept answer shows its length, never its bytes: the
    /// body is the caller's data (a recipient, a message), and a `{:?}`
    /// in a log line or a panic would otherwise print it. Decisive: each
    /// hand-written `Debug`.
    #[test]
    fn debug_shows_a_kept_bodys_length_never_its_bytes() {
        let body = br#"{"messages":[{"id":"wamid.SECRET-BODY-7Q"}],"to":"+15551234567"}"#.to_vec();
        let text = String::from_utf8(body.clone()).unwrap();
        let bytes = format!("{body:?}");
        let bytes = &bytes[1..bytes.len() - 1];
        let state = IdempotencyState::Completed {
            status: 200,
            body: body.clone(),
        };
        let record = IdempotencyRecord {
            fingerprint: [7; 32],
            state: state.clone(),
        };
        let printed = [
            format!("{state:?}"),
            format!("{record:?}"),
            format!("{:?}", IdempotencyClaim::Existing(record.clone())),
            format!("{:?}", Repeat::of(record, &Fingerprint::of([7; 32]))),
            format!(
                "{:?}",
                Outcome::Answered {
                    status: 200,
                    body: &body,
                }
            ),
            format!("{:?}", Answer::new(200, body.clone())),
        ];
        for debug in &printed {
            assert!(debug.contains("status: 200"), "{debug}");
            assert!(
                debug.contains(&format!("body_len: {}", body.len())),
                "{debug}"
            );
            assert!(!debug.contains("SECRET-BODY"), "the text: {debug}");
            assert!(!debug.contains(&text), "the text: {debug}");
            assert!(!debug.contains(bytes), "the bytes: {debug}");
            assert!(!debug.contains("7, 7, 7"), "the fingerprint: {debug}");
        }
        assert!(printed[3].starts_with("Replay"), "{}", printed[3]);
    }

    /// The registration operations' fingerprints (roadmap M3d): each its
    /// operation's and its number's, the region and the code's delivery
    /// counting, and the PIN no input at all (`register_number` takes none:
    /// what the record keeps holds nothing derived from it). The HTTP
    /// tests decide the rest (`tests/registration.rs`: a repeat under the
    /// same key with another PIN replays).
    #[test]
    fn the_registration_fingerprints_are_their_operations_and_hold_no_pin() {
        let one = pn("106540352242922");
        let registered = Fingerprint::register_number(&one, None);
        assert_eq!(
            registered,
            Fingerprint::json(
                Operation::RegisterNumber,
                &[("pn", one.as_str())],
                &serde_json::json!({"data_localization_region": null}),
            )
        );
        assert_ne!(registered, Fingerprint::register_number(&one, Some("CH")));
        assert_ne!(
            registered,
            Fingerprint::register_number(&pn("106540352242923"), None)
        );
        assert_ne!(registered, Fingerprint::deregister_number(&one));
        let sms = Fingerprint::request_verification_code(&one, "SMS", "en_US");
        assert_ne!(
            sms,
            Fingerprint::request_verification_code(&one, "VOICE", "en_US")
        );
        assert_ne!(
            sms,
            Fingerprint::request_verification_code(&one, "SMS", "fr_FR")
        );
        assert_eq!(
            sms,
            Fingerprint::request_verification_code(&one, "SMS", "en_US")
        );
    }
}
