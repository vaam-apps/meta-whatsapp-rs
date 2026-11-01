//! The max price beta agreement (`POST`/`GET /{BUSINESS_ID}/max_price_agreements`).
//!
//! Source: `marketing-messages/pricing/enroll-max-price`.
//!
//! **Signing is a legal act** (AGENTS.md § Decisions, `OPEN_QUESTIONS.md` #45):
//! [`MarketingBusiness::sign_max_price_agreement`] signs Meta's Max Price
//! Beta Agreement on the business's behalf. It is the only call in this
//! crate that sends the agreement's `POST`, and no other call makes it: no
//! send, duplicate, reach estimate or builder signs implicitly, and an
//! error saying the agreement is missing is returned, never answered by
//! signing. Whether to sign is the deployer's decision, for their own
//! deployment. This file is the only one under `src/` naming the
//! agreement's path (a test checks).

use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::{Error, Result};
use serde::{Deserialize, Serialize};

use super::{MarketingBusiness, wire_enum};
use crate::RetryPolicy;

/// [`SIGNER_WITHHELD`] as text, the snippet of an [`Error::Http`].
const SIGNER_WITHHELD_TEXT: &str = "(withheld: the response may name the agreement's signer)";

/// A decode error's placeholder: the response names the signer.
const SIGNER_WITHHELD: &[u8] = SIGNER_WITHHELD_TEXT.as_bytes();

/// A non-2xx answer of the agreement's endpoint that is not Meta's error
/// envelope: its snippet becomes [`SIGNER_WITHHELD_TEXT`], whatever the
/// body says, since the endpoint's answers name the signer
/// (`signer_name`, `signer_email`, a `message` quoting the email), and the
/// crate's own rule (`safe_snippet`) withholds only bodies naming
/// `paging` or `access_token`. Meta's envelope (`Error::Api`) carries
/// Meta's message, not the body, and is kept, as for the solution token
/// (roadmap L12).
fn withhold_signer(error: Error) -> Error {
    match error {
        Error::Http { status, .. } => Error::Http {
            status,
            body_snippet: SIGNER_WITHHELD_TEXT.to_owned(),
        },
        other => other,
    }
}

wire_enum! {
    /// Where a business is with the max price beta agreement.
    pub enum AgreementStatus {
        /// No agreement requested (`none`).
        NoAgreement = "none",
        /// Requested; the acceptance email is out, not yet accepted.
        Pending = "pending",
        /// Accepted: Meta allowlists the business's own WABAs.
        Accepted = "accepted",
    }
}

/// Who signs the max price beta agreement, and where Meta sends the
/// acceptance link.
///
/// Meta states it uses `signer_name` (printed on the agreement) and
/// `signer_email` (the link's destination) only to execute the agreement.
/// Both are required and must not be blank; whether the email is well
/// formed is Meta's check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaxPriceAgreementSigner {
    /// Name of the person signing.
    pub signer_name: String,
    /// Where Meta emails the acceptance link.
    pub signer_email: String,
}

impl MaxPriceAgreementSigner {
    /// The signer `signer_name`, reached at `signer_email`.
    pub fn new(signer_name: impl Into<String>, signer_email: impl Into<String>) -> Self {
        Self {
            signer_name: signer_name.into(),
            signer_email: signer_email.into(),
        }
    }

    fn validate(&self) -> Result<(), ValidationError> {
        if self.signer_name.trim().is_empty() {
            return Err(ValidationError::new("signer_name", "is required"));
        }
        if self.signer_email.trim().is_empty() {
            return Err(ValidationError::new("signer_email", "is required"));
        }
        Ok(())
    }
}

/// The agreement's state, as `POST` and `GET /{BUSINESS_ID}/max_price_agreements`
/// answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaxPriceAgreement {
    /// `none`, `pending` or `accepted`.
    pub status: AgreementStatus,
    /// The signer, once one was named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_name: Option<String>,
    /// Where the acceptance link went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_email: Option<String>,
    /// Meta's note on a `POST` ("Agreement email sent to …").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl MarketingBusiness {
    /// **Sign Meta's Max Price Beta Agreement on this business's behalf**
    /// (`POST /{BUSINESS_ID}/max_price_agreements`).
    ///
    /// Calling this signs Meta's agreement for the business: a legal act,
    /// and the deployer's decision to make (AGENTS.md § Decisions,
    /// `OPEN_QUESTIONS.md` #45). Nothing else in this crate calls it: max-price
    /// sends, template duplication, reach estimates and template max price
    /// updates never sign, even when Meta refuses them for want of the
    /// agreement. Call it only when the business has decided to sign.
    ///
    /// Signed once per business portfolio, by an admin of it, with a token
    /// holding `business_management` (a system user or business token).
    /// Meta emails `signer.signer_email` an acceptance link and answers
    /// [`AgreementStatus::Pending`]; once the business accepts, its own
    /// WABAs are allowlisted, and a Solution Partner's agreement covers the
    /// client WABAs shared with or managed by it (since the open beta of
    /// 2026-10-01, no per-client allowlist). Calling it again while pending
    /// resends the email, to a new `signer_email` if given; within 5
    /// minutes of the last email Meta answers HTTP 429.
    ///
    /// Sent once, whatever the client's [`RetryPolicy`]: not replayed
    /// after a timeout (a lost answer is surfaced; check
    /// [`Self::max_price_agreement`] rather than signing again), nor on an
    /// HTTP 429 or a throttling error, which the policy resends elsewhere
    /// ([`Error::may_resend`]): Meta's 5 minutes outlast any retry's delay,
    /// and a resend once they are over would send the email again
    /// unasked. A 429 is returned as
    /// [`ErrorKind::RateLimited`](meta_whatsapp_core::ErrorKind::RateLimited);
    /// wait 5 minutes from the last email before calling again.
    ///
    /// The signer stays out of errors: an answer that does not decode is
    /// an [`Error::Decode`] carrying neither the body nor serde's message,
    /// and a non-2xx answer that is not Meta's error envelope an
    /// [`Error::Http`] whose snippet is a placeholder, never the body
    /// (any `retry_after` in the body included).
    pub async fn sign_max_price_agreement(
        &self,
        signer: &MaxPriceAgreementSigner,
    ) -> Result<MaxPriceAgreement> {
        signer.validate()?;
        self.client()
            .clone()
            .with_retry(RetryPolicy::NONE)
            .post_at(&[self.id().as_str(), "max_price_agreements"])
            .json(signer)
            .idempotent(false)
            .context("max price agreement response")
            .send_withheld(SIGNER_WITHHELD)
            .await
            .map_err(withhold_signer)
    }

    /// The max price agreement's state
    /// (`GET /{BUSINESS_ID}/max_price_agreements`): `none`, `pending` or
    /// `accepted`. Reads only: it never signs. A read, retried as any
    /// `GET` is; its errors withhold the signer as
    /// [`Self::sign_max_price_agreement`]'s do.
    pub async fn max_price_agreement(&self) -> Result<MaxPriceAgreement> {
        self.client()
            .get_at(&[self.id().as_str(), "max_price_agreements"])
            .context("max price agreement status response")
            .send_withheld(SIGNER_WITHHELD)
            .await
            .map_err(withhold_signer)
    }
}
