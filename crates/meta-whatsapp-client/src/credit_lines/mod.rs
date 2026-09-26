//! Solution Partner credit lines: find your extended credit line, share it
//! with an onboarded customer, check that it funds the customer's WABA, and
//! revoke it.
//!
//! Docs: `solution-providers/share-and-revoke-credit-lines`, with the
//! prerequisite of the one-call method in
//! `solution-providers/manage-system-users`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! # Which token
//!
//! Every method authenticates with the token of the [`Client`] it was
//! built from. Meta requires the **partner's system user token** (with
//! `business_management`, and an Admin or Financial Editor role on the
//! partner's business portfolio) for everything here except two calls that
//! take the **customer's business token**:
//!
//! | Method | Token |
//! | --- | --- |
//! | [`CreditLines::list`], [`CreditLines::share_and_attach`], [`CreditLines::share`], [`CreditLines::receiving_credential`], [`CreditLines::allocations_for`], [`CreditLines::revoke`], [`CreditLines::revoke_for_business`], [`CreditLines::allocation_status`] | the partner's system user token |
//! | [`CreditLines::attach`], [`CreditLines::primary_funding`] | the customer's business token |
//!
//! ```no_run
//! # async fn demo(client: meta_whatsapp_client::Client, business_token: meta_whatsapp_core::secret::AccessToken) -> meta_whatsapp_core::Result<()> {
//! use meta_whatsapp_client::credit_lines::{WabaCurrency, is_shared};
//! use meta_whatsapp_core::ids::{CreditLineId, WabaId};
//! use meta_whatsapp_core::secret::AccessToken;
//!
//! let system = client.with_token(AccessToken::new("<SYSTEM_USER_TOKEN>"));
//! let business = client.with_token(business_token);
//! let line = CreditLineId::new("1972385232742146");
//! let waba = WabaId::new("102290129340398");
//!
//! let shared = system
//!     .credit_lines()
//!     .share_and_attach(&line, &waba, &WabaCurrency::Usd)
//!     .await?;
//! // Verify: the allocation's receiving credential funds the WABA.
//! let allocation = system
//!     .credit_lines()
//!     .receiving_credential(&shared.allocation_config_id)
//!     .await?;
//! let funding = business.credit_lines().primary_funding(&waba).await?;
//! assert!(is_shared(&allocation, &funding));
//! # Ok(()) }
//! ```
//!
//! # Two ways to share
//!
//! - **One call** ([`CreditLines::share_and_attach`], Meta's current method):
//!   `POST /{CREDIT_LINE_ID}/whatsapp_credit_sharing_and_attach` with the
//!   WABA and its currency, system token. The Solution Partner onboarding
//!   page requires the partner's system user to be added to the customer's
//!   WABA first ([`Waba::assign_user`](crate::waba::Waba::assign_user)).
//! - **Two calls** (Meta's "alternate method", being tested to replace the
//!   first): [`CreditLines::share`] with the customer's business portfolio
//!   id (system token), then [`CreditLines::attach`] with the WABA and its
//!   currency (the customer's business token). Whether this method also
//!   needs the system user on the WABA is not stated.
//!
//! A credit line **cannot be changed** once attached to a WABA; a different
//! line needs a new WABA. Revoking ([`CreditLines::revoke_for_business`])
//! applies to **every** WABA of that customer business shared with you.
//! Meta does not say whether a customer business can attach a line shared
//! with it to other WABAs of its own: reconcile your credit line invoice
//! against the WABAs you onboarded.
//!
//! # Where Meta's page is loose (decided here)
//!
//! - `owning_credit_allocation_configs` is an edge, which normally answers
//!   `{"data": [...]}`, but the page's example is a single object.
//!   [`CreditLines::allocations_for`] accepts both, and follows cursors
//!   should the edge be paged.
//! - The page does not say whether that lookup lists revoked records, and
//!   asks it for `id,receiving_business` only. Whoever needs to tell a
//!   revoked record from an active one reads each one's `request_status`
//!   ([`CreditLines::allocation_status`]), as revocation and Solution
//!   Partner onboarding do.
//! - The revocation status example has no `id`, so
//!   [`AllocationConfig::id`] is optional.
//! - Only `DELETED` is documented for `request_status`
//!   ([`AllocationRequestStatus`] keeps any other value verbatim). A record
//!   without one is active ([`AllocationConfig::is_active`]); one with a
//!   value Meta does not document is neither active nor revoked: Solution
//!   Partner onboarding refuses to share next to it unless the request
//!   opts in, and revocation deletes it like an active one.
//! - The page's examples mix API versions (v21.0, v24.0, v25.0); the
//!   client's configured version is used for all of them.

use std::fmt;
use std::str::FromStr;

use futures::Stream;
#[doc(no_inline)]
pub use meta_whatsapp_core::error::CreditRevocation;
use meta_whatsapp_core::error::{RevocationIncomplete, ValidationError};
use meta_whatsapp_core::ids::{AllocationConfigId, BusinessId, CreditLineId, FundingId, WabaId};
use meta_whatsapp_core::paging::Page;
use meta_whatsapp_core::{Error, Result};
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

use crate::phone_numbers::fields_param;
use crate::request::{decode_json_private, send_checked};
use crate::waba::BusinessRef;
use crate::{Client, GraphRequest};

/// Entry point, see [`Client::credit_lines`].
#[derive(Debug, Clone)]
pub struct CreditLines {
    client: Client,
}

impl Client {
    /// Credit line operations, authenticated with this client's token (see
    /// the [module docs](crate::credit_lines) for which token each needs).
    pub fn credit_lines(&self) -> CreditLines {
        CreditLines {
            client: self.clone(),
        }
    }
}

/// The currency a customer's WABA is invoiced in (`waba_currency`).
///
/// Meta supports six today; [`Self::Other`] exists so a currency Meta adds
/// later can be sent without a new release, and must be chosen explicitly:
/// parsing a string (`"USD".parse()`) accepts only the six.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WabaCurrency {
    /// Australian dollar.
    Aud,
    /// Euro.
    Eur,
    /// Pound sterling.
    Gbp,
    /// Indonesian rupiah.
    Idr,
    /// Indian rupee.
    Inr,
    /// US dollar.
    Usd,
    /// A three-letter code Meta did not list on 2026-09-24. Checked for
    /// shape only (three uppercase ASCII letters).
    Other(String),
}

impl WabaCurrency {
    /// The currencies `share-and-revoke-credit-lines` lists as supported.
    pub const SUPPORTED: [&'static str; 6] = ["AUD", "EUR", "GBP", "IDR", "INR", "USD"];

    /// The code as sent to Meta.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Aud => "AUD",
            Self::Eur => "EUR",
            Self::Gbp => "GBP",
            Self::Idr => "IDR",
            Self::Inr => "INR",
            Self::Usd => "USD",
            Self::Other(code) => code,
        }
    }

    /// Check the shape of an [`Self::Other`] code (the six named variants
    /// are always valid).
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Other(code)
                if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_uppercase()) =>
            {
                Err(ValidationError::new(
                    "waba_currency",
                    "must be a three-letter uppercase currency code",
                ))
            }
            _ => Ok(()),
        }
    }
}

impl FromStr for WabaCurrency {
    type Err = ValidationError;

    /// One of [`WabaCurrency::SUPPORTED`], exactly as Meta spells it.
    fn from_str(code: &str) -> Result<Self, Self::Err> {
        Ok(match code {
            "AUD" => Self::Aud,
            "EUR" => Self::Eur,
            "GBP" => Self::Gbp,
            "IDR" => Self::Idr,
            "INR" => Self::Inr,
            "USD" => Self::Usd,
            _ => {
                return Err(ValidationError::new(
                    "waba_currency",
                    "Meta supports AUD, EUR, GBP, IDR, INR and USD (use WabaCurrency::Other for another code on purpose)",
                ));
            }
        })
    }
}

impl fmt::Display for WabaCurrency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One of a business's extended credit lines, from
/// `GET /{BUSINESS_ID}/extendedcredits`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ExtendedCredit {
    /// The credit line id.
    pub id: CreditLineId,
    /// Legal entity name, when requested (`fields=id,legal_entity_name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_entity_name: Option<String>,
}

/// Response of `whatsapp_credit_sharing_and_attach`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SharedAndAttached {
    /// The allocation configuration id: keep it to verify or revoke.
    pub allocation_config_id: AllocationConfigId,
    /// The customer's WABA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba_id: Option<WabaId>,
}

/// Response of `whatsapp_credit_sharing` (step 2 of the two-call method).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreditShared {
    /// The allocation configuration id.
    pub allocation_config_id: AllocationConfigId,
}

/// Response of `whatsapp_credit_attach` (step 3 of the two-call method).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreditAttached {
    /// The allocation configuration id: keep it to verify or revoke.
    pub allocation_config_id: AllocationConfigId,
    /// The customer's WABA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waba_id: Option<WabaId>,
}

/// An extended credit allocation configuration: one credit line shared with
/// one customer business. Which fields are present depends on the call.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct AllocationConfig {
    /// The allocation configuration id (absent from the page's revocation
    /// status example).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<AllocationConfigId>,
    /// The credential that pays for the customer's WABA once attached
    /// (`fields=receiving_credential`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiving_credential: Option<ReceivingCredential>,
    /// The customer business the line is shared with
    /// (`fields=receiving_business`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiving_business: Option<BusinessRef>,
    /// `DELETED` once revoked (`fields=request_status`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_status: Option<AllocationRequestStatus>,
}

/// `receiving_credential` of an allocation configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ReceivingCredential {
    /// Compared with the WABA's `primary_funding_id` by [`is_shared`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<FundingId>,
}

/// `request_status` of an allocation configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AllocationRequestStatus {
    /// Revoked.
    Deleted,
    /// Any other value, verbatim (the page documents only `DELETED`).
    Other(String),
}

impl AllocationRequestStatus {
    /// The value as Meta sends it.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Deleted => "DELETED",
            Self::Other(s) => s,
        }
    }
}

impl Serialize for AllocationRequestStatus {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AllocationRequestStatus {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(if s == "DELETED" {
            Self::Deleted
        } else {
            Self::Other(s)
        })
    }
}

/// A WABA's `primary_funding_id`, read with the customer's business token.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct WabaFunding {
    /// The WABA.
    pub id: WabaId,
    /// What pays for the WABA's messages; absent until something does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_funding_id: Option<FundingId>,
}

/// Whether `allocation` funds the WABA of `funding`: its receiving
/// credential is the WABA's primary funding
/// (`share-and-revoke-credit-lines`, "Verifying shared status"). `false`
/// when either id is missing or empty.
pub fn is_shared(allocation: &AllocationConfig, funding: &WabaFunding) -> bool {
    let credential = allocation
        .receiving_credential
        .as_ref()
        .and_then(|c| c.id.as_ref());
    match (credential, funding.primary_funding_id.as_ref()) {
        (Some(c), Some(f)) => !c.as_str().trim().is_empty() && c == f,
        _ => false,
    }
}

fn required<'a>(field: &'static str, id: &'a str) -> Result<&'a str> {
    if id.trim().is_empty() {
        return Err(ValidationError::new(field, "required").into());
    }
    Ok(id)
}

impl CreditLines {
    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    fn list_request(&self, business_id: &BusinessId, fields: &[&str]) -> GraphRequest {
        self.client
            .get_at(&[business_id.as_str(), "extendedcredits"])
            .query_opt("fields", fields_param(fields))
            .context("extended credits")
    }

    /// `GET /{BUSINESS_ID}/extendedcredits`: your business portfolio's
    /// credit lines (`fields` empty = Meta's defaults; the revocation guide
    /// asks for `id,legal_entity_name`). System user token.
    ///
    /// Takes no cursor: the page shows neither `after`/`before` nor a
    /// `paging` object. Should Meta page it anyway, [`Self::list_stream`]
    /// follows the cursors.
    pub async fn list(
        &self,
        business_id: &BusinessId,
        fields: &[&str],
    ) -> Result<Page<ExtendedCredit>> {
        self.list_request(business_id, fields).send().await
    }

    /// Every credit line, following cursors.
    pub fn list_stream(
        &self,
        business_id: &BusinessId,
        fields: &[&str],
    ) -> impl Stream<Item = Result<ExtendedCredit>> + Send + 'static + use<> {
        self.list_request(business_id, fields).paginate()
    }

    /// `POST /{CREDIT_LINE_ID}/whatsapp_credit_sharing_and_attach?waba_currency=…&waba_id=…`:
    /// share the credit line with the customer and attach it to their WABA
    /// in one call (Meta's current method). System user token; the system
    /// user must already be on the WABA.
    ///
    /// Not replayed after a timeout or a server error: the line may already
    /// be attached, and Meta refuses to change an attached line. Check with
    /// [`Self::allocations_for`], [`Self::receiving_credential`] and
    /// [`Self::primary_funding`] before trying again.
    pub async fn share_and_attach(
        &self,
        credit_line: &CreditLineId,
        waba_id: &WabaId,
        currency: &WabaCurrency,
    ) -> Result<SharedAndAttached> {
        currency.validate()?;
        let waba_id = required("waba_id", waba_id.as_str())?;
        let request = self
            .client
            .post_at(&[credit_line.as_str(), "whatsapp_credit_sharing_and_attach"])
            .query("waba_currency", currency)
            .query("waba_id", waba_id);
        send_checked(request, "credit sharing and attach response").await
    }

    /// `POST /{CREDIT_LINE_ID}/whatsapp_credit_sharing?receiving_business_id=…`:
    /// step 2 of the two-call method, the intent to share with the
    /// customer's business portfolio (the WABA's verified owner, never an id
    /// from the browser). System user token.
    pub async fn share(
        &self,
        credit_line: &CreditLineId,
        receiving_business_id: &BusinessId,
    ) -> Result<CreditShared> {
        let business = required("receiving_business_id", receiving_business_id.as_str())?;
        let request = self
            .client
            .post_at(&[credit_line.as_str(), "whatsapp_credit_sharing"])
            .query("receiving_business_id", business);
        send_checked(request, "credit sharing response").await
    }

    /// `POST /{CREDIT_LINE_ID}/whatsapp_credit_attach?waba_currency=…&waba_id=…`:
    /// step 3 of the two-call method. **The customer's business token**,
    /// not the system user token. Not replayed after a timeout or a server
    /// error (an attached line cannot be changed).
    pub async fn attach(
        &self,
        credit_line: &CreditLineId,
        waba_id: &WabaId,
        currency: &WabaCurrency,
    ) -> Result<CreditAttached> {
        currency.validate()?;
        let waba_id = required("waba_id", waba_id.as_str())?;
        let request = self
            .client
            .post_at(&[credit_line.as_str(), "whatsapp_credit_attach"])
            .query("waba_currency", currency)
            .query("waba_id", waba_id);
        send_checked(request, "credit attach response").await
    }

    /// `GET /{ALLOCATION_CONFIG_ID}?fields=receiving_credential`. System
    /// user token.
    pub async fn receiving_credential(
        &self,
        allocation: &AllocationConfigId,
    ) -> Result<AllocationConfig> {
        self.client
            .get_at(&[allocation.as_str()])
            .query("fields", "receiving_credential")
            .context("credit allocation")
            .send()
            .await
    }

    /// `GET /{WABA_ID}?fields=primary_funding_id`. **The customer's
    /// business token.**
    pub async fn primary_funding(&self, waba_id: &WabaId) -> Result<WabaFunding> {
        self.client
            .get_at(&[waba_id.as_str()])
            .query("fields", "primary_funding_id")
            .context("WABA primary funding")
            .send()
            .await
    }

    /// `GET /{CREDIT_LINE_ID}/owning_credit_allocation_configs?receiving_business_id=…&fields=id,receiving_business`:
    /// the records of this line shared with a customer business. System
    /// user token.
    ///
    /// Accepts both `{"data": [...]}` and the single object the page's
    /// example shows, and follows `paging` cursors should Meta page the
    /// edge (the page shows none; at most [`MAX_ALLOCATION_PAGES`] pages,
    /// then an error rather than a partial list). The records' business
    /// names never reach an error message. Nothing here checks that each
    /// record's `receiving_business` is the one asked for, nor whether it
    /// was revoked (`fields` has no `request_status`, as on Meta's page):
    /// [`Self::revoke_for_business`] and Solution Partner onboarding do both.
    pub async fn allocations_for(
        &self,
        credit_line: &CreditLineId,
        receiving_business_id: &BusinessId,
    ) -> Result<Vec<AllocationConfig>> {
        const CONTEXT: &str = "owning credit allocation configs";
        let business = required("receiving_business_id", receiving_business_id.as_str())?;
        let mut records = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_ALLOCATION_PAGES {
            let resp = self
                .client
                .get_at(&[credit_line.as_str(), "owning_credit_allocation_configs"])
                .query("receiving_business_id", business)
                .query("fields", "id,receiving_business")
                .query_opt("after", after.as_deref())
                .context(CONTEXT)
                .send_raw()
                .await?;
            let value: serde_json::Value = decode_json_private(CONTEXT, &resp.body)?;
            if value.get("data").is_none() {
                // The page's example: one record, not a page.
                records.push(decode_json_private::<AllocationConfig>(
                    CONTEXT, &resp.body,
                )?);
                return Ok(records);
            }
            let page: Page<AllocationConfig> = decode_json_private(CONTEXT, &resp.body)?;
            let next = page.next_cursor().map(str::to_owned);
            records.extend(page.data);
            match next {
                Some(cursor) if after.as_deref() != Some(cursor.as_str()) => after = Some(cursor),
                Some(_) => {
                    return Err(ValidationError::new(
                        "owning_credit_allocation_configs",
                        "Meta returned the same cursor twice; refusing a partial list",
                    )
                    .into());
                }
                None => return Ok(records),
            }
        }
        Err(ValidationError::new(
            "owning_credit_allocation_configs",
            format!("more than {MAX_ALLOCATION_PAGES} pages of records; refusing a partial list"),
        )
        .into())
    }

    /// `DELETE /{ALLOCATION_CONFIG_ID}`: stop sharing the line with that
    /// customer business, for **all** of its WABAs shared with you. System
    /// user token. See [`Self::revoke_for_business`] for the checked form.
    pub async fn revoke(&self, allocation: &AllocationConfigId) -> Result<()> {
        self.client
            .delete_at(&[allocation.as_str()])
            .context("revoke credit sharing response")
            .send_success()
            .await
    }

    /// Revoke every **active** record of `credit_line` shared with
    /// `business_id`, confirm each, and report what was done. System user
    /// token.
    ///
    /// - Only records whose `receiving_business.id` is `business_id` are
    ///   touched; one naming another business is left alone. A record that
    ///   names no business is not revoked either (that could revoke another
    ///   customer): it is reported in [`RevocationIncomplete::unattributed`]
    ///   after the rest was revoked.
    /// - Each record's status is read first (`allocation_status`): one
    ///   already `DELETED` is reported in
    ///   [`CreditRevocation::already_revoked`] and not deleted again. Each
    ///   `DELETE` is confirmed by reading the status back; a `DELETE` that
    ///   fails on a record Meta then reports `DELETED` counts as done.
    /// - Every record is attempted even if one fails, so a single error
    ///   never leaves the rest of the business funded. Anything left
    ///   undone is an [`Error::Credit`] carrying a [`RevocationIncomplete`]:
    ///   the report of what was revoked, the records that failed, were not
    ///   confirmed yet, or name no business, and the first underlying
    ///   error, with its own `is_retryable` and `may_have_been_sent`. Call
    ///   again to finish: what is already revoked is skipped.
    ///
    /// Works when the WABA is no longer shared with you and its
    /// `owner_business_info` cannot be read any more: pass the business id
    /// stored at onboarding (or the `owner_business_id` of a signed
    /// `PARTNER_REMOVED` webhook). Unlike
    /// [`EmbeddedSignup::revoke_credit_line`](crate::embedded_signup::EmbeddedSignup::revoke_credit_line),
    /// it records nothing: Solution Partner onboarding does not know the
    /// business was revoked.
    pub async fn revoke_for_business(
        &self,
        credit_line: &CreditLineId,
        business_id: &BusinessId,
    ) -> Result<CreditRevocation> {
        self.revoke_all(credit_line, Some(business_id), &[])
            .await
            .into_result()
    }

    /// [`Self::revoke_for_business`] for `business` (when known), plus
    /// `known`, allocation ids recorded at onboarding that the lookup may
    /// not return. A known id is never revoked if its status names another
    /// business than `business`. Never fails by itself: what is left
    /// undone is in the outcome ([`RevokeAll::into_result`]).
    pub(crate) async fn revoke_all(
        &self,
        credit_line: &CreditLineId,
        business: Option<&BusinessId>,
        known: &[AllocationConfigId],
    ) -> RevokeAll {
        let mut out = RevocationIncomplete::new(CreditRevocation::new(business.cloned()));
        let mut named: Vec<AllocationConfigId> = Vec::new();
        let mut first_failure: Option<Error> = None;
        let mut targets: Vec<AllocationConfigId> = Vec::new();
        let records = match business {
            // A failed lookup does not stop the known allocations from being
            // revoked; the lookup's error is returned afterwards.
            Some(business) => match self.allocations_for(credit_line, business).await {
                Ok(records) => records,
                Err(e) => {
                    first_failure = Some(e);
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        if let Some(business) = business {
            for record in records {
                let receiving = record.receiving_business.and_then(|b| b.id);
                match (record.id, receiving) {
                    (Some(id), Some(owner)) if &owner == business => {
                        if !targets.contains(&id) {
                            named.push(id.clone());
                            targets.push(id);
                        }
                    }
                    // Another customer's record: never touched.
                    (Some(_) | None, Some(_)) | (None, None) => {}
                    (Some(id), None) => {
                        if !out.unattributed.contains(&id) {
                            out.unattributed.push(id);
                        }
                    }
                }
            }
        }
        for known in known {
            if !targets.contains(known) {
                out.unattributed.retain(|id| id != known);
                targets.push(known.clone());
            }
        }
        for id in targets {
            match self.revoke_checked(&id, business).await {
                Revoked::Now => {
                    out.deletes_sent = true;
                    out.report.revoked.push(id);
                }
                Revoked::Already { deleted_by_us } => {
                    out.deletes_sent |= deleted_by_us;
                    out.report.already_revoked.push(id);
                }
                Revoked::Unconfirmed(e) => {
                    out.deletes_sent = true;
                    out.unconfirmed.push(id);
                    if let Some(e) = e {
                        first_failure.get_or_insert(e);
                    }
                }
                Revoked::Failed { error, sent } => {
                    out.deletes_sent |= sent;
                    out.failed.push(id);
                    first_failure.get_or_insert(error);
                }
            }
        }
        out.source = first_failure.map(Box::new);
        RevokeAll {
            outcome: out,
            named,
        }
    }

    /// Revoke one record unless it is already `DELETED` or names another
    /// business than `business`, and confirm it.
    async fn revoke_checked(
        &self,
        id: &AllocationConfigId,
        business: Option<&BusinessId>,
    ) -> Revoked {
        // A failed read does not stop the revocation: the DELETE decides.
        if let Ok(status) = self.allocation_status(id).await {
            if let (Some(business), Some(named)) = (
                business,
                status
                    .receiving_business
                    .as_ref()
                    .and_then(|b| b.id.as_ref()),
            ) && named != business
            {
                return Revoked::Failed {
                    error: ValidationError::new(
                        "allocation_config_id",
                        format!("{id} is shared with another business; not revoked"),
                    )
                    .into(),
                    sent: false,
                };
            }
            if status.is_deleted() {
                return Revoked::Already {
                    deleted_by_us: false,
                };
            }
        }
        if let Err(error) = self.revoke(id).await {
            let sent = error.may_have_been_sent();
            return match self.allocation_status(id).await {
                // Gone either way: by this DELETE (whose answer was lost) or
                // before it.
                Ok(status) if status.is_deleted() => Revoked::Already {
                    deleted_by_us: sent,
                },
                _ => Revoked::Failed { error, sent },
            };
        }
        match self.allocation_status(id).await {
            Ok(status) if status.is_deleted() => Revoked::Now,
            Ok(_) => Revoked::Unconfirmed(None),
            Err(e) => Revoked::Unconfirmed(Some(e)),
        }
    }

    /// `GET /{ALLOCATION_CONFIG_ID}?fields=receiving_business,request_status`:
    /// e.g. to confirm a revocation (`request_status` `DELETED`). System
    /// user token. The business name never reaches an error message.
    pub async fn allocation_status(
        &self,
        allocation: &AllocationConfigId,
    ) -> Result<AllocationConfig> {
        self.client
            .get_at(&[allocation.as_str()])
            .query("fields", "receiving_business,request_status")
            .context("credit allocation status")
            .send_private()
            .await
    }
}

/// Most pages [`CreditLines::allocations_for`] reads. Not a Meta limit: a
/// bound so a paging loop cannot run away; one customer business has one
/// record per credit line in Meta's examples.
pub const MAX_ALLOCATION_PAGES: usize = 20;

/// What [`CreditLines::revoke_all`] did: its report, what it left undone,
/// and which records the business lookup attributed to the business.
pub(crate) struct RevokeAll {
    /// The report, and whatever is left undone (nothing, when
    /// [`RevocationIncomplete::is_incomplete`] is `false`).
    /// `deletes_sent` is kept even when nothing is left undone.
    pub(crate) outcome: RevocationIncomplete,
    /// The ids the business lookup returned naming the business (not the
    /// `known` ones, unless the lookup returned them too).
    pub(crate) named: Vec<AllocationConfigId>,
}

impl RevokeAll {
    /// The report, or [`CreditError::RevocationIncomplete`](meta_whatsapp_core::error::CreditError::RevocationIncomplete)
    /// when anything is left undone.
    pub(crate) fn into_result(self) -> Result<CreditRevocation> {
        finish(self.outcome)
    }
}

/// `out`'s report, or `out` as [`CreditError::RevocationIncomplete`](meta_whatsapp_core::error::CreditError::RevocationIncomplete)
/// when anything is left undone (logged, by kind and id only).
pub(crate) fn finish(out: RevocationIncomplete) -> Result<CreditRevocation> {
    if !out.is_incomplete() {
        return Ok(out.report);
    }
    tracing::warn!(
        revoked = ?out.report.revoked,
        already_revoked = ?out.report.already_revoked,
        failed = ?out.failed,
        unconfirmed = ?out.unconfirmed,
        unattributed = ?out.unattributed,
        share_pending = out.share_pending,
        kind = ?out.source.as_ref().map(|e| e.kind()),
        ledger = ?out.ledger.as_ref().map(|e| e.kind()),
        "credit line revocation incomplete"
    );
    Err(out.into())
}

/// What happened to one record.
enum Revoked {
    /// Deleted by this call and confirmed.
    Now,
    /// Already `DELETED` (`deleted_by_us`: a DELETE of this call whose
    /// answer was an error may have done it).
    Already { deleted_by_us: bool },
    /// Meta accepted the DELETE but does not report the record `DELETED`
    /// (yet), or the status could not be read back.
    Unconfirmed(Option<Error>),
    /// Still active.
    Failed { error: Error, sent: bool },
}

impl AllocationConfig {
    /// Whether Meta reports the record revoked (`request_status` `DELETED`).
    pub fn is_deleted(&self) -> bool {
        self.request_status == Some(AllocationRequestStatus::Deleted)
    }

    /// Whether the record is active: it has no `request_status` (Meta
    /// documents only `DELETED`). A value Meta does not document makes it
    /// neither active nor [deleted](Self::is_deleted).
    pub fn is_active(&self) -> bool {
        self.request_status.is_none()
    }
}

/// The ids of `records` that name `business_id` as their receiving
/// business, in order, without duplicates.
pub(crate) fn owned_by(
    records: Vec<AllocationConfig>,
    business_id: &BusinessId,
) -> Vec<AllocationConfigId> {
    let mut ids: Vec<AllocationConfigId> = Vec::new();
    for record in records {
        let names_business = record
            .receiving_business
            .as_ref()
            .and_then(|b| b.id.as_ref())
            .is_some_and(|b| b == business_id);
        if let (true, Some(id)) = (names_business, record.id)
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use meta_whatsapp_core::ErrorKind;
    use meta_whatsapp_core::error::TransportError;
    use meta_whatsapp_core::testing::{RecordedBody, RecordedRequest, ScriptedTransport};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::RetryPolicy;

    const LINE: &str = "1972385232742146";
    const WABA: &str = "102290129340398";
    const ALLOCATION: &str = "58501441721238";
    const CUSTOMER: &str = "2729063490586005";

    fn client(t: &ScriptedTransport, token: &str) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token(token)
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn system(t: &ScriptedTransport) -> CreditLines {
        client(t, "SYSTEM_TOKEN").credit_lines()
    }

    fn line() -> CreditLineId {
        CreditLineId::new(LINE)
    }

    #[tokio::test]
    async fn list_parses_both_examples_of_the_page() {
        let t = ScriptedTransport::new();
        // "Get your credit line ID".
        t.push_json(200, json!({"data": [{"id": "1972385232742146"}]}));
        // "Revoke a shared credit line", step 1.
        t.push_json(
            200,
            json!({"data": [{"id": "1972385232742146", "legal_entity_name": "Your Legal Entity"}]}),
        );
        let lines = system(&t);
        let page = lines
            .list(&BusinessId::new("102289599326934"), &[])
            .await
            .unwrap();
        assert_eq!(page.data[0].id, line());
        assert_eq!(page.data[0].legal_entity_name, None);
        let page = lines
            .list(
                &BusinessId::new("105954558954427"),
                &["id", "legal_entity_name"],
            )
            .await
            .unwrap();
        assert_eq!(
            page.data[0].legal_entity_name.as_deref(),
            Some("Your Legal Entity")
        );
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::GET);
        assert_eq!(reqs[0].path(), "/v25.0/102289599326934/extendedcredits");
        assert_eq!(reqs[0].query("fields"), None);
        assert_eq!(reqs[0].bearer(), Some("SYSTEM_TOKEN"));
        assert_eq!(reqs[1].path(), "/v25.0/105954558954427/extendedcredits");
        assert_eq!(
            reqs[1].query("fields").as_deref(),
            Some("id,legal_entity_name")
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn list_stream_follows_cursors() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "1"}], "paging": {"cursors": {"after": "c1"}, "next": "https://graph.facebook.com/x"}}),
        );
        t.push_json(200, json!({"data": [{"id": "2"}]}));
        let ids: Vec<String> = system(&t)
            .list_stream(&BusinessId::new("B"), &[])
            .map(|c| c.unwrap().id.into_inner())
            .collect()
            .await;
        assert_eq!(ids, ["1", "2"]);
        assert_eq!(t.requests()[1].query("after").as_deref(), Some("c1"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn share_and_attach_sends_currency_and_waba_in_the_query() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"allocation_config_id": "58501441721238", "waba_id": "102290129340398"}),
        );
        let done = system(&t)
            .share_and_attach(&line(), &WabaId::new(WABA), &WabaCurrency::Usd)
            .await
            .unwrap();
        assert_eq!(done.allocation_config_id.as_str(), ALLOCATION);
        assert_eq!(done.waba_id, Some(WabaId::new(WABA)));
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            "/v25.0/1972385232742146/whatsapp_credit_sharing_and_attach"
        );
        assert_eq!(req.query("waba_currency").as_deref(), Some("USD"));
        assert_eq!(req.query("waba_id").as_deref(), Some(WABA));
        assert_eq!(req.bearer(), Some("SYSTEM_TOKEN"));
        assert_eq!(req.body, RecordedBody::Empty);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn share_then_attach_parse_the_examples() {
        let t = ScriptedTransport::new();
        // Step 2.
        t.push_json(
            200,
            json!({"success": true, "allocation_config_id": "58501441721238"}),
        );
        // Step 3.
        t.push_json(
            200,
            json!({"success": true, "waba_id": "102290129340398", "allocation_config_id": "58501441721238"}),
        );
        let shared = system(&t)
            .share(
                &CreditLineId::new("5985499441566032"),
                &BusinessId::new(CUSTOMER),
            )
            .await
            .unwrap();
        assert_eq!(shared.allocation_config_id.as_str(), ALLOCATION);
        let attached = client(&t, "BUSINESS_TOKEN")
            .credit_lines()
            .attach(
                &CreditLineId::new("5985499441566032"),
                &WabaId::new(WABA),
                &WabaCurrency::Usd,
            )
            .await
            .unwrap();
        assert_eq!(attached.allocation_config_id.as_str(), ALLOCATION);
        assert_eq!(attached.waba_id, Some(WabaId::new(WABA)));

        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::POST);
        assert_eq!(
            reqs[0].path(),
            "/v25.0/5985499441566032/whatsapp_credit_sharing"
        );
        assert_eq!(
            reqs[0].query("receiving_business_id").as_deref(),
            Some(CUSTOMER)
        );
        assert_eq!(reqs[0].bearer(), Some("SYSTEM_TOKEN"));
        assert_eq!(reqs[0].body, RecordedBody::Empty);
        assert_eq!(reqs[1].method, Method::POST);
        assert_eq!(
            reqs[1].path(),
            "/v25.0/5985499441566032/whatsapp_credit_attach"
        );
        assert_eq!(reqs[1].query("waba_currency").as_deref(), Some("USD"));
        assert_eq!(reqs[1].query("waba_id").as_deref(), Some(WABA));
        assert_eq!(reqs[1].bearer(), Some("BUSINESS_TOKEN"));
        assert_eq!(reqs[1].body, RecordedBody::Empty);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn success_false_is_an_error_even_with_an_id() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"success": false, "allocation_config_id": "58501441721238"}),
        );
        let err = system(&t)
            .share(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Http { status: 200, .. }), "{err}");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn sharing_posts_are_not_replayed_after_a_timeout() {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        let retrying = Client::builder()
            .transport(t.clone())
            .access_token("SYSTEM_TOKEN")
            .retry(RetryPolicy {
                max_retries: 3,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            })
            .build()
            .unwrap();
        let err = retrying
            .credit_lines()
            .share_and_attach(&line(), &WabaId::new(WABA), &WabaCurrency::Eur)
            .await
            .unwrap_err();
        assert!(err.may_have_been_sent());
        assert_eq!(t.requests().len(), 1, "a timed-out share is never replayed");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn verification_reads_both_sides_and_compares() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"receiving_credential": {"id": "7011"}, "id": "58501441721238"}),
        );
        t.push_json(
            200,
            json!({"primary_funding_id": "7011", "id": "102290129340398"}),
        );
        let allocation = system(&t)
            .receiving_credential(&AllocationConfigId::new(ALLOCATION))
            .await
            .unwrap();
        let funding = client(&t, "BUSINESS_TOKEN")
            .credit_lines()
            .primary_funding(&WabaId::new(WABA))
            .await
            .unwrap();
        assert!(is_shared(&allocation, &funding));
        assert_eq!(allocation.id, Some(AllocationConfigId::new(ALLOCATION)));
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::GET);
        assert_eq!(reqs[0].path(), "/v25.0/58501441721238");
        assert_eq!(
            reqs[0].query("fields").as_deref(),
            Some("receiving_credential")
        );
        assert_eq!(reqs[0].bearer(), Some("SYSTEM_TOKEN"));
        assert_eq!(reqs[1].method, Method::GET);
        assert_eq!(reqs[1].path(), "/v25.0/102290129340398");
        assert_eq!(
            reqs[1].query("fields").as_deref(),
            Some("primary_funding_id")
        );
        assert_eq!(reqs[1].bearer(), Some("BUSINESS_TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    #[test]
    fn is_shared_needs_two_equal_non_empty_ids() {
        let allocation = |id: Option<&str>| AllocationConfig {
            id: None,
            receiving_credential: Some(ReceivingCredential {
                id: id.map(FundingId::new),
            }),
            receiving_business: None,
            request_status: None,
        };
        let funding = |id: Option<&str>| WabaFunding {
            id: WabaId::new(WABA),
            primary_funding_id: id.map(FundingId::new),
        };
        assert!(is_shared(&allocation(Some("1")), &funding(Some("1"))));
        assert!(!is_shared(&allocation(Some("1")), &funding(Some("2"))));
        assert!(!is_shared(&allocation(None), &funding(None)));
        assert!(!is_shared(&allocation(Some("")), &funding(Some(""))));
        assert!(!is_shared(&allocation(Some(" ")), &funding(Some(" "))));
        assert!(!is_shared(&allocation(Some("1")), &funding(None)));
        assert!(!is_shared(&allocation(None), &funding(Some("1"))));
        let no_credential = AllocationConfig {
            receiving_credential: None,
            ..allocation(None)
        };
        assert!(!is_shared(&no_credential, &funding(Some("1"))));
    }

    #[tokio::test]
    async fn allocations_for_accepts_the_single_object_and_the_page() {
        let t = ScriptedTransport::new();
        // The page's example: one object.
        t.push_json(
            200,
            json!({"id": "1972385232742140", "receiving_business": {"name": "Client Business Name", "id": "1972385232742147"}}),
        );
        // What an edge normally answers.
        t.push_json(
            200,
            json!({"data": [
                {"id": "1972385232742140", "receiving_business": {"name": "Client Business Name", "id": "1972385232742147"}},
                {"id": "1972385232742141", "receiving_business": {"id": "1972385232742147"}}
            ]}),
        );
        t.push_json(200, json!({"data": []}));
        let lines = system(&t);
        let business = BusinessId::new("1972385232742147");
        let one = lines.allocations_for(&line(), &business).await.unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].id, Some(AllocationConfigId::new("1972385232742140")));
        assert_eq!(
            one[0].receiving_business.as_ref().unwrap().id,
            Some(business.clone())
        );
        let two = lines.allocations_for(&line(), &business).await.unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two[1].id, Some(AllocationConfigId::new("1972385232742141")));
        assert!(
            lines
                .allocations_for(&line(), &business)
                .await
                .unwrap()
                .is_empty()
        );
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(
            req.path(),
            "/v25.0/1972385232742146/owning_credit_allocation_configs"
        );
        assert_eq!(
            req.query("receiving_business_id").as_deref(),
            Some("1972385232742147")
        );
        assert_eq!(
            req.query("fields").as_deref(),
            Some("id,receiving_business")
        );
        assert_eq!(req.bearer(), Some("SYSTEM_TOKEN"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn revoke_and_status_parse_the_examples() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        t.push_json(
            200,
            json!({"receiving_business": {"name": "Customer Business Name", "id": "1972385232742147"}, "request_status": "DELETED"}),
        );
        let lines = system(&t);
        let id = AllocationConfigId::new("1972385232742140");
        lines.revoke(&id).await.unwrap();
        let status = lines.allocation_status(&id).await.unwrap();
        assert_eq!(
            status.request_status,
            Some(AllocationRequestStatus::Deleted)
        );
        assert_eq!(status.id, None, "the example has no id");
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::DELETE);
        assert_eq!(reqs[0].path(), "/v25.0/1972385232742140");
        assert_eq!(reqs[0].bearer(), Some("SYSTEM_TOKEN"));
        assert_eq!(reqs[1].method, Method::GET);
        assert_eq!(reqs[1].path(), "/v25.0/1972385232742140");
        assert_eq!(
            reqs[1].query("fields").as_deref(),
            Some("receiving_business,request_status")
        );
        assert_eq!(t.remaining(), 0);

        let other: AllocationConfig =
            serde_json::from_value(json!({"request_status": "SOMETHING_NEW"})).unwrap();
        assert_eq!(
            other.request_status,
            Some(AllocationRequestStatus::Other("SOMETHING_NEW".into()))
        );
        // Neither active nor revoked: only a missing status is active.
        assert!(!other.is_active() && !other.is_deleted());
        assert!(status.is_deleted() && !status.is_active());
        let active: AllocationConfig = serde_json::from_value(json!({})).unwrap();
        assert!(active.is_active() && !active.is_deleted());
    }

    fn status(business: &str, deleted: bool) -> serde_json::Value {
        if deleted {
            json!({"receiving_business": {"id": business}, "request_status": "DELETED"})
        } else {
            json!({"receiving_business": {"id": business}})
        }
    }

    #[tokio::test]
    async fn revoke_for_business_deletes_only_that_business_active_records() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [
                {"id": "A1", "receiving_business": {"id": CUSTOMER}},
                {"id": "OTHER", "receiving_business": {"id": "SOMEONE_ELSE"}},
                {"receiving_business": {"id": CUSTOMER}},
                {"id": "A1", "receiving_business": {"id": CUSTOMER}},
                {"id": "GONE", "receiving_business": {"id": CUSTOMER}},
                {"id": "A2", "receiving_business": {"id": CUSTOMER}}
            ]}),
        );
        // A1: active, deleted, confirmed.
        t.push_json(200, status(CUSTOMER, false));
        t.push_json(200, json!({"success": true}));
        t.push_json(200, status(CUSTOMER, true));
        // GONE: already revoked, not deleted again.
        t.push_json(200, status(CUSTOMER, true));
        // A2.
        t.push_json(200, status(CUSTOMER, false));
        t.push_json(200, json!({"success": true}));
        t.push_json(200, status(CUSTOMER, true));
        let report = system(&t)
            .revoke_for_business(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap();
        assert_eq!(
            report.revoked,
            [AllocationConfigId::new("A1"), AllocationConfigId::new("A2")]
        );
        assert_eq!(report.already_revoked, [AllocationConfigId::new("GONE")]);
        assert_eq!(report.business_id, Some(BusinessId::new(CUSTOMER)));
        let reqs = t.requests();
        assert_eq!(reqs.len(), 8);
        assert_eq!(
            reqs[0].query("receiving_business_id").as_deref(),
            Some(CUSTOMER)
        );
        let deletes: Vec<&str> = reqs
            .iter()
            .filter(|r| r.method == Method::DELETE)
            .map(RecordedRequest::path)
            .collect();
        assert_eq!(deletes, ["/v25.0/A1", "/v25.0/A2"]);
        for r in &reqs {
            assert_eq!(r.bearer(), Some("SYSTEM_TOKEN"));
            assert_ne!(r.path(), "/v25.0/OTHER", "another business's record");
        }
        assert_eq!(t.remaining(), 0);

        // Nothing shared: nothing deleted.
        t.push_json(200, json!({"data": []}));
        let report = system(&t)
            .revoke_for_business(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap();
        assert_eq!(report.all().count(), 0);
        assert_eq!(t.requests().len(), 9);
        assert_eq!(t.remaining(), 0);
    }

    /// A record naming no business is reported, never revoked blindly, and
    /// does not stop the others from being revoked.
    #[tokio::test]
    async fn an_unattributed_record_fails_the_call_after_the_rest_is_revoked() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "UNNAMED"}, {"id": "A1", "receiving_business": {"id": CUSTOMER}}]}),
        );
        t.push_json(200, status(CUSTOMER, false));
        t.push_json(200, json!({"success": true}));
        t.push_json(200, status(CUSTOMER, true));
        let err = system(&t)
            .revoke_for_business(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap_err();
        let incomplete = incomplete(&err);
        assert_eq!(
            incomplete.unattributed,
            [AllocationConfigId::new("UNNAMED")]
        );
        assert_eq!(incomplete.report.revoked, [AllocationConfigId::new("A1")]);
        assert!(incomplete.source.is_none(), "{err}");
        assert!(!err.is_retryable(), "a person must check it");
        assert!(err.may_have_been_sent(), "A1's DELETE went out");
        assert!(t.requests().iter().all(|r| r.path() != "/v25.0/UNNAMED"));
        assert_eq!(t.remaining(), 0);
    }

    fn incomplete(err: &Error) -> &RevocationIncomplete {
        err.credit()
            .and_then(meta_whatsapp_core::error::CreditError::revocation)
            .unwrap_or_else(|| panic!("not an incomplete revocation: {err}"))
    }

    /// One failed DELETE does not leave the rest of the business funded:
    /// every record is tried, then the first failure is returned.
    #[tokio::test]
    async fn every_record_is_tried_before_a_failure_is_returned() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [
                {"id": "A1", "receiving_business": {"id": CUSTOMER}},
                {"id": "A2", "receiving_business": {"id": CUSTOMER}},
                {"id": "A3", "receiving_business": {"id": CUSTOMER}}
            ]}),
        );
        // A1: the DELETE fails and the record is still active.
        t.push_json(200, status(CUSTOMER, false));
        t.push_json(
            403,
            json!({"error": {"message": "(#200) Permissions error", "type": "OAuthException", "code": 200}}),
        );
        t.push_json(200, status(CUSTOMER, false));
        // A2: the DELETE fails, but Meta reports it DELETED: done.
        t.push_json(200, status(CUSTOMER, false));
        t.push_json(
            400,
            json!({"error": {"message": "(#100) Object does not exist", "type": "OAuthException", "code": 100}}),
        );
        t.push_json(200, status(CUSTOMER, true));
        // A3: deleted, but not confirmed.
        t.push_json(200, status(CUSTOMER, false));
        t.push_json(200, json!({"success": true}));
        t.push_json(200, status(CUSTOMER, false));
        let err = system(&t)
            .revoke_for_business(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::Permission,
            "the first failure: {err}"
        );
        let reqs = t.requests();
        let deletes: Vec<&str> = reqs
            .iter()
            .filter(|r| r.method == Method::DELETE)
            .map(RecordedRequest::path)
            .collect();
        assert_eq!(deletes, ["/v25.0/A1", "/v25.0/A2", "/v25.0/A3"]);
        assert_eq!(t.remaining(), 0);
        // Each record lands where it belongs: A2's failed DELETE on a
        // record Meta then reports DELETED counts as done.
        let report = incomplete(&err);
        assert_eq!(report.failed, [AllocationConfigId::new("A1")]);
        assert_eq!(
            report.report.already_revoked,
            [AllocationConfigId::new("A2")]
        );
        assert_eq!(report.report.revoked, []);
        assert_eq!(report.unconfirmed, [AllocationConfigId::new("A3")]);
        assert!(err.may_have_been_sent(), "A3's DELETE was accepted");
        assert!(!err.is_retryable(), "a permission error stays one");

        // An unconfirmed DELETE alone is an error too, and worth repeating.
        t.push_json(
            200,
            json!({"id": "A3", "receiving_business": {"id": CUSTOMER}}),
        );
        t.push_json(200, status(CUSTOMER, false));
        t.push_json(200, json!({"success": true}));
        t.push_json(200, status(CUSTOMER, false));
        let err = system(&t)
            .revoke_for_business(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap_err();
        let report = incomplete(&err);
        assert_eq!(report.unconfirmed, [AllocationConfigId::new("A3")]);
        assert!(report.source.is_none(), "{err}");
        assert!(err.is_retryable(), "call again: {err}");
        assert!(err.may_have_been_sent());
        assert_eq!(t.remaining(), 0);

        // A DELETE that times out on a record Meta then reports DELETED is
        // done, and may have been this call's doing.
        t.push_json(
            200,
            json!({"id": "A4", "receiving_business": {"id": CUSTOMER}}),
        );
        t.push_json(200, status(CUSTOMER, false));
        t.push_error(|| TransportError::Timeout);
        t.push_json(200, status(CUSTOMER, true));
        let report = system(&t)
            .revoke_for_business(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap();
        assert_eq!(report.already_revoked, [AllocationConfigId::new("A4")]);
        assert_eq!(t.remaining(), 0);
    }

    /// Z29, Z30: `deletes_sent` counts every DELETE that may have taken
    /// effect: one whose answer was lost, whether Meta then reports the
    /// record DELETED or still active. A refused one (4xx) is not counted.
    #[tokio::test]
    async fn a_delete_whose_answer_was_lost_counts_as_sent() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [
                {"id": "A1", "receiving_business": {"id": CUSTOMER}},
                {"id": "A2", "receiving_business": {"id": CUSTOMER}}
            ]}),
        );
        // A1: the DELETE times out, then Meta reports it DELETED.
        t.push_json(200, status(CUSTOMER, false));
        t.push_error(|| TransportError::Timeout);
        t.push_json(200, status(CUSTOMER, true));
        // A2: the DELETE is refused, the record still active.
        t.push_json(200, status(CUSTOMER, false));
        t.push_json(
            403,
            json!({"error": {"message": "(#200) Permissions error", "type": "OAuthException", "code": 200}}),
        );
        t.push_json(200, status(CUSTOMER, false));
        let err = system(&t)
            .revoke_for_business(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap_err();
        let report = incomplete(&err);
        assert_eq!(
            report.report.already_revoked,
            [AllocationConfigId::new("A1")]
        );
        assert_eq!(report.failed, [AllocationConfigId::new("A2")]);
        assert!(report.deletes_sent, "A1's DELETE may have done it");
        assert!(err.may_have_been_sent(), "{err}");
        assert_eq!(t.remaining(), 0);

        // A3: the DELETE times out and Meta still reports it active.
        t.push_json(
            200,
            json!({"id": "A3", "receiving_business": {"id": CUSTOMER}}),
        );
        t.push_json(200, status(CUSTOMER, false));
        t.push_error(|| TransportError::Timeout);
        t.push_json(200, status(CUSTOMER, false));
        let err = system(&t)
            .revoke_for_business(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap_err();
        let report = incomplete(&err);
        assert_eq!(report.failed, [AllocationConfigId::new("A3")]);
        assert!(report.deletes_sent, "the DELETE may still take effect");
        assert!(err.is_retryable(), "a timeout: call again");
        assert_eq!(t.remaining(), 0);

        // Refused only: nothing sent.
        t.push_json(
            200,
            json!({"id": "A5", "receiving_business": {"id": CUSTOMER}}),
        );
        t.push_json(200, status(CUSTOMER, false));
        t.push_json(
            403,
            json!({"error": {"message": "(#200) Permissions error", "type": "OAuthException", "code": 200}}),
        );
        t.push_json(200, status(CUSTOMER, false));
        let err = system(&t)
            .revoke_for_business(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap_err();
        assert!(!incomplete(&err).deletes_sent);
        assert!(!err.may_have_been_sent(), "{err}");
        assert_eq!(t.remaining(), 0);
    }

    /// The page limit holds when Meta's cursors cycle through more than one
    /// value (A, B, A, ...): the same-cursor check alone never fires.
    #[tokio::test]
    async fn allocations_for_stops_at_the_page_limit() {
        let t = ScriptedTransport::new();
        for page in 0..MAX_ALLOCATION_PAGES {
            let cursor = if page % 2 == 0 { "A" } else { "B" };
            t.push_json(
                200,
                json!({"data": [], "paging": {"cursors": {"after": cursor}, "next": "https://graph.facebook.com/next"}}),
            );
        }
        let err = system(&t)
            .allocations_for(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "owning_credit_allocation_configs" && v.reason.contains("pages")),
            "{err}"
        );
        assert_eq!(t.requests().len(), MAX_ALLOCATION_PAGES);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn allocations_for_follows_cursors_and_refuses_a_loop() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "A1", "receiving_business": {"id": CUSTOMER}}],
                   "paging": {"cursors": {"after": "c1"}, "next": "https://graph.facebook.com/next"}}),
        );
        t.push_json(
            200,
            json!({"data": [{"id": "A2", "receiving_business": {"id": CUSTOMER}}],
                   "paging": {"cursors": {"after": "c2"}}}),
        );
        let records = system(&t)
            .allocations_for(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap();
        assert_eq!(records.len(), 2);
        let reqs = t.requests();
        assert_eq!(reqs[0].query("after"), None);
        assert_eq!(reqs[1].query("after").as_deref(), Some("c1"));
        assert_eq!(
            reqs[1].query("receiving_business_id").as_deref(),
            Some(CUSTOMER)
        );
        assert_eq!(t.remaining(), 0);

        let looping = json!({"data": [], "paging": {"cursors": {"after": "same"}, "next": "https://graph.facebook.com/next"}});
        t.push_json(200, looping.clone());
        t.push_json(200, looping);
        let err = system(&t)
            .allocations_for(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err}");
        assert_eq!(t.remaining(), 0);
    }

    /// Allocation records carry the customer's business name; a response
    /// that does not decode never quotes it.
    #[tokio::test]
    async fn decode_errors_never_quote_the_business_name() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"receiving_business": {"name": "Wind & Wool", "id": 2729063490586005_u64}}),
        );
        t.push_json(
            200,
            json!({"data": [{"id": 1, "receiving_business": {"name": "Wind & Wool"}}]}),
        );
        let lines = system(&t);
        let err = lines
            .allocation_status(&AllocationConfigId::new(ALLOCATION))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Decode { .. }), "{err}");
        assert!(!format!("{err} {err:?}").contains("Wind"), "{err:?}");
        let err = lines
            .allocations_for(&line(), &BusinessId::new(CUSTOMER))
            .await
            .unwrap_err();
        assert!(!format!("{err} {err:?}").contains("Wind"), "{err:?}");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn graph_errors_are_classified() {
        let t = ScriptedTransport::new();
        t.push_json(
            403,
            json!({"error": {"message": "(#200) Permissions error", "type": "OAuthException", "code": 200, "fbtrace_id": "A"}}),
        );
        let err = system(&t)
            .share_and_attach(&line(), &WabaId::new(WABA), &WabaCurrency::Inr)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert!(!err.may_have_been_sent());
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn inputs_are_checked_before_any_request() {
        let t = ScriptedTransport::new();
        let lines = system(&t);
        let err = lines
            .share_and_attach(
                &line(),
                &WabaId::new(WABA),
                &WabaCurrency::Other("usd".into()),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "waba_currency"),
            "{err}"
        );
        let err = lines
            .attach(&line(), &WabaId::new(" "), &WabaCurrency::Usd)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "waba_id"),
            "{err}"
        );
        let err = lines
            .share(&line(), &BusinessId::new(""))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "receiving_business_id"),
            "{err}"
        );
        let err = lines
            .allocations_for(&line(), &BusinessId::new(""))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Validation(_)));
        assert!(t.requests().is_empty());
    }

    #[test]
    fn currencies_parse_strictly_and_other_is_deliberate() {
        for (code, currency) in [
            ("AUD", WabaCurrency::Aud),
            ("EUR", WabaCurrency::Eur),
            ("GBP", WabaCurrency::Gbp),
            ("IDR", WabaCurrency::Idr),
            ("INR", WabaCurrency::Inr),
            ("USD", WabaCurrency::Usd),
        ] {
            assert_eq!(code.parse::<WabaCurrency>().unwrap(), currency);
            assert_eq!(currency.to_string(), code);
            assert!(currency.validate().is_ok());
            assert!(WabaCurrency::SUPPORTED.contains(&code));
        }
        for bad in ["usd", "BRL", "", "US", " USD"] {
            assert!(bad.parse::<WabaCurrency>().is_err(), "{bad}");
        }
        assert!(WabaCurrency::Other("BRL".into()).validate().is_ok());
        for bad in ["brl", "BR", "BRLX", "B1L", ""] {
            assert!(WabaCurrency::Other(bad.into()).validate().is_err(), "{bad}");
        }
    }

    /// An id taken from data must not address another Graph object with
    /// the partner's token.
    #[tokio::test]
    async fn ids_stay_in_their_segment() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"allocation_config_id": "1"}));
        t.push_json(200, json!({"success": true}));
        let lines = system(&t);
        lines
            .share_and_attach(
                &CreditLineId::new("OTHER/whatsapp_credit_sharing"),
                &WabaId::new(WABA),
                &WabaCurrency::Usd,
            )
            .await
            .unwrap();
        lines
            .revoke(&AllocationConfigId::new(
                "1/owning_credit_allocation_configs",
            ))
            .await
            .unwrap();
        let paths: Vec<String> = t.requests().iter().map(|r| r.path().to_owned()).collect();
        assert_eq!(
            paths,
            [
                "/v25.0/OTHER%2Fwhatsapp_credit_sharing/whatsapp_credit_sharing_and_attach",
                "/v25.0/1%2Fowning_credit_allocation_configs",
            ]
        );
        assert!(lines.revoke(&AllocationConfigId::new("..")).await.is_err());
        assert_eq!(t.requests().len(), 2, "refused before the wire");
        assert_eq!(t.remaining(), 0);
    }
}
