//! Reference code for the partner-led business verification part of the
//! `meta-whatsapp-rs-embedded-signup` skill (approved Select and Premier
//! Solution Partners only): submit a merchant's business for verification
//! with their documents, read the decision from `account_update`, and
//! check the merchant business's status.
//!
//! meta-whatsapp-rs compiles this file and runs its tests in its own gate
//! (`crates/meta-whatsapp-rs/tests/skills.rs`).

use futures::TryStreamExt;
use meta_whatsapp_rs::client::business_verification::{
    ListVerificationSubmissions, MAX_SUBMISSIONS, RejectionReason, SubmissionReceipt,
    SubmissionStatus, VerificationDocument,
};
use meta_whatsapp_rs::client::embedded_signup::TokenVault;
use meta_whatsapp_rs::core::ids::{BusinessId, WabaId};
use meta_whatsapp_rs::prelude::*;
use meta_whatsapp_rs::webhooks::fields::{AccountUpdateEvent, CertificationStatus};

/// What `submit_for_verification` did.
#[derive(Debug, PartialEq)]
pub enum Submission {
    /// Submitted; Meta decides in minutes to hours, by webhook.
    Submitted(SubmissionReceipt),
    /// A submission is pending or approved already: nothing sent.
    AlreadySubmitted,
    /// Three submissions made: the merchant verifies on their own.
    MerchantMustVerify,
}

/// Submit `merchant` (their business portfolio) for verification, with
/// your system user token and your business portfolio. Each submission
/// counts (three per merchant) and is never retried, so look first: a
/// lost answer may have been received.
pub async fn submit_for_verification(
    client: &Client,
    system_token: AccessToken, // your system user's (admin, business_management)
    our_business: &BusinessId,
    merchant: &BusinessId,
    files: Vec<(String, Vec<u8>)>, // file name and content, from the merchant
) -> meta_whatsapp_rs::Result<Submission> {
    let system = client.with_token(system_token);
    let api = system.business_verification();
    let query = ListVerificationSubmissions::new().end_business_id(merchant.clone());
    let previous: Vec<_> = api
        .submissions_stream(our_business, &query)
        .try_collect()
        .await?;
    let live = |s: &Option<SubmissionStatus>| {
        matches!(
            s,
            Some(SubmissionStatus::Pending | SubmissionStatus::Approved)
        )
    };
    if previous.iter().any(|s| live(&s.verification_status)) {
        return Ok(Submission::AlreadySubmitted);
    }
    if previous.len() >= MAX_SUBMISSIONS as usize {
        return Ok(Submission::MerchantMustVerify);
    }
    // PDF, JPEG/JPG or PNG, at most 5 MB each, one to three: checked here,
    // before anything is sent.
    let documents = files
        .into_iter()
        .map(|(name, data)| VerificationDocument::from_file_name(name, data))
        .collect::<Result<Vec<_>, _>>()?;
    let receipt = api.submit(our_business, merchant, &documents).await?;
    Ok(Submission::Submitted(receipt))
}

/// What an `account_update` says about partner-led verification.
#[derive(Debug, PartialEq)]
pub enum VerificationEvent {
    /// The merchant skipped the website in Embedded Signup: they cannot
    /// send messages until you verify their business.
    Needed(BusinessId),
    /// Meta decided on a submission.
    Decided {
        /// The merchant's business portfolio.
        merchant: BusinessId,
        /// The outcome.
        status: CertificationStatus,
        /// Why, when rejected (`NONE` is dropped).
        reasons: Vec<RejectionReason>,
    },
}

/// From a signature-checked `account_update`, next to your other
/// `account_update` handling.
pub fn verification_event(event: &WebhookEvent) -> Option<VerificationEvent> {
    let WebhookEvent::AccountUpdated { update, .. } = event else {
        return None;
    };
    match update.event {
        AccountUpdateEvent::PartnerClientCertificationNeeded => {
            let info = update.partner_client_certification_needed_info.as_ref()?;
            Some(VerificationEvent::Needed(info.client_business_id.clone()?))
        }
        AccountUpdateEvent::PartnerClientCertificationStatusUpdate => {
            let info = update.partner_client_certification_info.as_ref()?;
            let reasons = info
                .rejection_reasons
                .iter()
                .map(|r| RejectionReason::parse(r))
                .filter(|r| *r != RejectionReason::None)
                .collect();
            Some(VerificationEvent::Decided {
                merchant: info.client_business_id.clone()?,
                status: info.status.clone()?,
                reasons,
            })
        }
        _ => None,
    }
}

/// Whether the merchant's business is verified, with THEIR business token
/// (from the vault), not your system user's.
pub async fn is_verified(
    client: &Client,
    vault: &TokenVault,
    waba_id: &WabaId,
    merchant: &BusinessId,
) -> meta_whatsapp_rs::Result<bool> {
    let Some(stored) = vault.get(waba_id).await? else {
        return Ok(false); // no token: onboard them first
    };
    let business = client.with_token(stored.token);
    let info = business.business_verification().status(merchant).await?;
    Ok(info.is_verified())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use meta_whatsapp_rs::adapters::store::MemoryKvStore;
    use meta_whatsapp_rs::client::embedded_signup::{StoredBusinessToken, VaultKey, VaultKeys};
    use meta_whatsapp_rs::core::testing::ScriptedTransport;
    use meta_whatsapp_rs::webhooks::WebhookPayload;
    use serde_json::json;

    use super::*;

    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="; // test only
    // The page's example ids.
    const US: &str = "506914307656634";
    const MERCHANT: &str = "2729063490586005";
    const WABA: &str = "486585971195941";

    fn client(transport: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(transport.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn files() -> Vec<(String, Vec<u8>)> {
        vec![("bank_statement.pdf".into(), b"%PDF-1.7\n".to_vec())]
    }

    async fn submit(transport: &ScriptedTransport) -> meta_whatsapp_rs::Result<Submission> {
        submit_for_verification(
            &client(transport),
            AccessToken::new("SYSTEM_TOKEN"),
            &BusinessId::new(US),
            &BusinessId::new(MERCHANT),
            files(),
        )
        .await
    }

    #[tokio::test]
    async fn submits_once_nothing_is_pending() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "1", "verification_status": "FAILED",
                             "rejection_reasons": ["MALFORMED DOCUMENTS"]}]}),
        );
        t.push_json(
            200,
            json!({"success": true, "message": "Your request has been received and will be reviewed shortly.", "verification_attempts": 2}),
        );
        let Submission::Submitted(receipt) = submit(&t).await.unwrap() else {
            panic!("expected a submission");
        };
        assert_eq!(receipt.attempts_left(), Some(1));
        let reqs = t.requests();
        assert_eq!(reqs[0].query("end_business_id").as_deref(), Some(MERCHANT));
        assert!(reqs.iter().all(|r| r.bearer() == Some("SYSTEM_TOKEN")));
        assert_eq!(
            reqs[1].path(),
            "/v25.0/506914307656634/self_certify_whatsapp_business"
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_pending_submission_or_three_attempts_send_nothing() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "1", "verification_status": "PENDING"}]}),
        );
        assert_eq!(submit(&t).await.unwrap(), Submission::AlreadySubmitted);
        let failed = json!({"id": "1", "verification_status": "FAILED"});
        t.push_json(200, json!({"data": [failed, failed, failed]}));
        assert_eq!(submit(&t).await.unwrap(), Submission::MerchantMustVerify);
        assert_eq!(t.requests().len(), 2, "no submission posted");
    }

    #[tokio::test]
    async fn an_unsupported_file_is_refused_before_posting() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"data": []}));
        let err = submit_for_verification(
            &client(&t),
            AccessToken::new("SYSTEM_TOKEN"),
            &BusinessId::new(US),
            &BusinessId::new(MERCHANT),
            vec![("statement.txt".into(), b"hello".to_vec())],
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidParameter);
        assert_eq!(t.requests().len(), 1, "only the lookup");
    }

    fn account_update(value: &serde_json::Value) -> WebhookEvent {
        let body = json!({"object": "whatsapp_business_account", "entry": [{
            "id": WABA, "time": 1730752761,
            "changes": [{"field": "account_update", "value": value}]
        }]});
        WebhookPayload::from_slice(body.to_string().as_bytes())
            .unwrap()
            .into_events()
            .remove(0)
    }

    #[test]
    fn reads_the_pages_webhooks() {
        // solution-providers/partner-led-business-verification, "Example webhook".
        let decided = account_update(&json!({
            "event": "PARTNER_CLIENT_CERTIFICATION_STATUS_UPDATE",
            "partner_client_certification_info": {
                "client_business_id": MERCHANT,
                "status": "FAILED",
                "rejection_reasons": ["LEGAL_NAME_NOT_FOUND_IN_DOCUMENTS"]
            }
        }));
        assert_eq!(
            verification_event(&decided),
            Some(VerificationEvent::Decided {
                merchant: BusinessId::new(MERCHANT),
                status: CertificationStatus::Failed,
                reasons: vec![RejectionReason::LegalNameNotFoundInDocuments],
            })
        );
        let approved = account_update(&json!({
            "event": "PARTNER_CLIENT_CERTIFICATION_STATUS_UPDATE",
            "partner_client_certification_info": {
                "client_business_id": MERCHANT, "status": "APPROVED", "rejection_reasons": ["NONE"]
            }
        }));
        let Some(VerificationEvent::Decided {
            status, reasons, ..
        }) = verification_event(&approved)
        else {
            panic!("expected a decision");
        };
        assert_eq!(status, CertificationStatus::Approved);
        assert!(reasons.is_empty());
        // embedded-signup/website-optional.
        let needed = account_update(&json!({
            "event": "PARTNER_CLIENT_CERTIFICATION_NEEDED",
            "partner_client_certification_needed_info": {"client_business_id": MERCHANT}
        }));
        assert_eq!(
            verification_event(&needed),
            Some(VerificationEvent::Needed(BusinessId::new(MERCHANT)))
        );
        let other = account_update(&json!({"event": "ACCOUNT_DELETED"}));
        assert_eq!(verification_event(&other), None);
    }

    #[tokio::test]
    async fn the_status_is_read_with_the_merchants_token() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"verification_status": "verified", "id": MERCHANT}),
        );
        let vault = TokenVault::new(
            Arc::new(MemoryKvStore::new()),
            VaultKeys::new(VaultKey::from_base64("2026-09", KEY).unwrap()),
        )
        .unwrap();
        let waba = WabaId::new(WABA);
        let merchant = BusinessId::new(MERCHANT);
        assert!(
            !is_verified(&client(&t), &vault, &waba, &merchant)
                .await
                .unwrap()
        );
        vault
            .store(&StoredBusinessToken::new(
                WABA,
                AccessToken::new("BUSINESS_TOKEN"),
            ))
            .await
            .unwrap();
        assert!(
            is_verified(&client(&t), &vault, &waba, &merchant)
                .await
                .unwrap()
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.bearer(), Some("BUSINESS_TOKEN"));
        assert_eq!(req.path(), "/v25.0/2729063490586005");
        assert_eq!(t.remaining(), 0);
    }
}
