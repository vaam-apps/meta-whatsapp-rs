//! Pre-verified business phone numbers (roadmap L11c): the pool on a
//! business portfolio and each number's code, partners and deletion. The
//! notes are in the [module docs](super#pre-verified-phone-numbers).

use futures::Stream;
use meta_whatsapp_core::Result;
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::{
    BusinessId, MAX_NUMERIC_ID_LEN, PreVerifiedPhoneNumberId, is_numeric_id,
};
use meta_whatsapp_core::paging::Page;
use serde::{Deserialize, Serialize};

use super::business::Business;
use super::types::{BusinessInfo, BusinessRef};
use super::{check_limit, fields_query};
use crate::phone_numbers::{CodeMethod, CodeVerificationStatus, VerificationCode, fields_param};
use crate::request::{paginate_or_error, reject_cursors};
use crate::{Client, GraphRequest};

/// A pre-verified phone number, from `GET /{Pre-Verified-Phone-Number-ID}`
/// or a page of `GET /{Business-ID}/preverified_numbers`. Which fields are
/// present depends on the `fields` requested (Meta's list: `id`,
/// `code_verification_status`, `code_verification_time`, `owner_business`,
/// `phone_number`, `verification_expiry_time`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct PreVerifiedNumber {
    /// The pre-verified number's id.
    pub id: PreVerifiedPhoneNumberId,
    /// `VERIFIED`, `NOT_VERIFIED` or `EXPIRED` (a value Meta adds later is
    /// `Unknown`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_verification_status: Option<CodeVerificationStatus>,
    /// When the number was last verified, Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_verification_time: Option<i64>,
    /// The business that owns the number (`id`, `name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_business: Option<BusinessRef>,
    /// The number, E.164.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
    /// When the verification expires, Unix seconds. Re-verifying an
    /// unclaimed number is possible from 45 days before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_expiry_time: Option<i64>,
}

/// Answer of [`Business::add_pre_verified_number`]: the new pre-verified
/// number's id, for [`Client::pre_verified_phone_number`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreatedPreVerifiedNumber {
    /// The pre-verified number's id, as Meta sent it.
    pub id: PreVerifiedPhoneNumberId,
}

/// Options for [`Business::pre_verified_numbers`]
/// (`reference/business/whatsapp-business-pre-verified-phone-numbers-api`).
/// Meta sorts the pool by creation time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListPreVerifiedNumbers {
    /// Fields to return (empty = Meta's defaults).
    pub fields: Vec<String>,
    /// Only the number matching this phone number.
    pub phone_number: Option<String>,
    /// Only numbers in this state: [`CodeVerificationStatus::Verified`],
    /// [`CodeVerificationStatus::NotVerified`] or
    /// [`CodeVerificationStatus::Expired`], the three values the reference
    /// lists (any other is refused before a request).
    pub code_verification_status: Option<CodeVerificationStatus>,
    /// Page size, at least 1 (the reference states no maximum).
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`
    /// ([`Page::next_cursor`]); for the one-page method only, the stream
    /// manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl ListPreVerifiedNumbers {
    /// Meta's defaults: the whole pool, whatever its state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fields to return.
    #[must_use]
    pub fn fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Only the number matching `phone_number`.
    #[must_use]
    pub fn phone_number(mut self, phone_number: impl Into<String>) -> Self {
        self.phone_number = Some(phone_number.into());
        self
    }

    /// Only numbers in `status`.
    #[must_use]
    pub fn code_verification_status(mut self, status: CodeVerificationStatus) -> Self {
        self.code_verification_status = Some(status);
        self
    }

    /// Page size (at least 1).
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Continue after this cursor.
    #[must_use]
    pub fn after(mut self, cursor: impl Into<String>) -> Self {
        self.after = Some(cursor.into());
        self
    }

    /// Go back before this cursor.
    #[must_use]
    pub fn before(mut self, cursor: impl Into<String>) -> Self {
        self.before = Some(cursor.into());
        self
    }
}

/// Options for [`PreVerifiedPhoneNumber::partners`]
/// (`reference/whatsapp-business-pre-verified-phone-number/whatsapp-business-pre-verified-phone-number-partners-api`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListPreVerifiedNumberPartners {
    /// Business node fields to return (empty = Meta's defaults, `id` and
    /// `name`).
    pub fields: Vec<String>,
    /// Page size, 1–100 (Meta's default: 25).
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`; for the
    /// one-page method only, the stream manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl ListPreVerifiedNumberPartners {
    /// Meta's defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Business node fields to return.
    #[must_use]
    pub fn fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Page size (1–100).
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Continue after this cursor.
    #[must_use]
    pub fn after(mut self, cursor: impl Into<String>) -> Self {
        self.after = Some(cursor.into());
        self
    }

    /// Go back before this cursor.
    #[must_use]
    pub fn before(mut self, cursor: impl Into<String>) -> Self {
        self.before = Some(cursor.into());
        self
    }
}

#[derive(Serialize)]
struct AddBody<'a> {
    phone_number: &'a str,
}

#[derive(Serialize)]
struct ShareBody<'a> {
    preverified_id: &'a str,
    partner_business_id: &'a str,
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

/// The id, checked as digits again before it goes into a path or a body:
/// a [`PreVerifiedPhoneNumberId`] built with `new_unchecked` (or read
/// from a response) may hold anything.
pub(super) fn checked_pre_verified_id(id: &PreVerifiedPhoneNumberId) -> Result<&str> {
    if is_numeric_id(id.as_str()) {
        Ok(id.as_str())
    } else {
        Err(ValidationError::new(
            PreVerifiedPhoneNumberId::FIELD,
            format!("expected a numeric id: 1 to {MAX_NUMERIC_ID_LEN} ASCII digits"),
        )
        .into())
    }
}

/// The phone number to add: the reference accepts E.164 or a number
/// formatted with spaces, hyphens and parentheses, and normalizes it.
fn check_phone_number(phone_number: &str) -> Result<()> {
    let allowed = |c: char| c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')' | ' ');
    if !phone_number.chars().any(|c| c.is_ascii_digit()) || !phone_number.chars().all(allowed) {
        return Err(ValidationError::new(
            "phone_number",
            "must be a phone number: digits, with an optional +, spaces, hyphens and parentheses",
        )
        .into());
    }
    Ok(())
}

/// The filter's wire value: the three values the reference lists, any
/// other refused before a request.
fn status_param(status: &CodeVerificationStatus) -> Result<&'static str> {
    match status {
        CodeVerificationStatus::Verified => Ok("VERIFIED"),
        CodeVerificationStatus::NotVerified => Ok("NOT_VERIFIED"),
        CodeVerificationStatus::Expired => Ok("EXPIRED"),
        _ => Err(ValidationError::new(
            "code_verification_status",
            "must be VERIFIED, NOT_VERIFIED or EXPIRED",
        )
        .into()),
    }
}

impl Business {
    /// `POST /{Business-ID}/add_phone_numbers`: add `phone_number` to this
    /// portfolio's pool of pre-verified numbers. Next:
    /// [`PreVerifiedPhoneNumber::request_code`] on the returned id, then
    /// [`PreVerifiedPhoneNumber::verify_code`] within 30 days.
    ///
    /// `phone_number` is E.164 or formatted with spaces, hyphens and
    /// parentheses (Meta normalizes it); anything else is refused before a
    /// request. A creation: never replayed after a timeout.
    pub async fn add_pre_verified_number(
        &self,
        phone_number: &str,
    ) -> Result<CreatedPreVerifiedNumber> {
        check_phone_number(phone_number)?;
        self.client
            .post_at(&[self.business_id.as_str(), "add_phone_numbers"])
            .json(&AddBody { phone_number })
            .context("added pre-verified phone number")
            .send()
            .await
    }

    fn pre_verified_request(&self, query: &ListPreVerifiedNumbers) -> Result<GraphRequest> {
        if query.limit == Some(0) {
            return Err(ValidationError::new("limit", "must be at least 1").into());
        }
        let status = query
            .code_verification_status
            .as_ref()
            .map(status_param)
            .transpose()?;
        Ok(self
            .client
            .get_at(&[self.business_id.as_str(), "preverified_numbers"])
            .query_opt("fields", fields_query(&query.fields))
            .query_opt("phone_number", query.phone_number.as_deref())
            .query_opt("code_verification_status", status)
            .query_opt("limit", query.limit)
            .context("pre-verified phone numbers"))
    }

    /// `GET /{Business-ID}/preverified_numbers`, one page: the pool's
    /// numbers, whatever their state unless the query filters it, in order
    /// of creation. The next page: the same query with `after` set to this
    /// page's [`Page::next_cursor`].
    pub async fn pre_verified_numbers(
        &self,
        query: &ListPreVerifiedNumbers,
    ) -> Result<Page<PreVerifiedNumber>> {
        self.pre_verified_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// The whole pool, following cursors. The stream manages them itself:
    /// a query with `after` or `before` set is refused (the stream's single
    /// item is that validation error).
    pub fn pre_verified_numbers_stream(
        &self,
        query: &ListPreVerifiedNumbers,
    ) -> impl Stream<Item = Result<PreVerifiedNumber>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.pre_verified_request(query)),
        )
    }

    /// `POST /{Business-ID}/share_preverified_numbers`: share `number`, a
    /// pre-verified number of this portfolio, with `partner_business_id`, a
    /// partner of a Multi-Partner Solution you are part of, who can then
    /// offer it in their Embedded Signup. Tell them its id, and have them
    /// list what is shared with them before offering it: a claimed number
    /// no longer appears in the flow. [`PreVerifiedPhoneNumber::partners`]
    /// reads who has it.
    ///
    /// Meta removed unsharing (its `DELETE` "returns an error"), so share
    /// deliberately. An explicit call nothing else in the library makes;
    /// never replayed after a timeout.
    pub async fn share_pre_verified_number(
        &self,
        number: &PreVerifiedPhoneNumberId,
        partner_business_id: &BusinessId,
    ) -> Result<()> {
        let preverified_id = checked_pre_verified_id(number)?;
        self.client
            .post_at(&[self.business_id.as_str(), "share_preverified_numbers"])
            .json(&ShareBody {
                preverified_id,
                partner_business_id: partner_business_id.as_str(),
            })
            .context("share pre-verified phone number response")
            .send_success()
            .await
    }
}

/// Entry point, see [`Client::pre_verified_phone_number`].
#[derive(Debug, Clone)]
pub struct PreVerifiedPhoneNumber {
    client: Client,
    id: PreVerifiedPhoneNumberId,
}

impl Client {
    /// [`PreVerifiedPhoneNumber`] API for a pre-verified number of a pool
    /// (the id [`Business::add_pre_verified_number`] returned). Not a
    /// business phone number: once claimed, the number has another id,
    /// for [`Client::phone_number`].
    pub fn pre_verified_phone_number(
        &self,
        id: impl Into<PreVerifiedPhoneNumberId>,
    ) -> PreVerifiedPhoneNumber {
        PreVerifiedPhoneNumber {
            client: self.clone(),
            id: id.into(),
        }
    }
}

impl PreVerifiedPhoneNumber {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PreVerifiedPhoneNumberId {
        &self.id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `GET /{Pre-Verified-Phone-Number-ID}` with `fields` (empty = Meta's
    /// defaults): its state, and when it was verified and expires
    /// (`code_verification_time`, `verification_expiry_time`), to re-verify
    /// it in time.
    pub async fn get(&self, fields: &[&str]) -> Result<PreVerifiedNumber> {
        let id = checked_pre_verified_id(&self.id)?;
        self.client
            .get_at(&[id])
            .query_opt("fields", fields_param(fields))
            .context("pre-verified phone number")
            .send()
            .await
    }

    /// `POST /{Pre-Verified-Phone-Number-ID}/request_code`: have a one-time
    /// code sent to the number by SMS or voice, in `language` (e.g.
    /// `en_US`; Meta falls back to `en_US`). Not replayed after a timeout:
    /// each request sends a message, and Meta limits them per number.
    pub async fn request_code(&self, method: CodeMethod, language: &str) -> Result<()> {
        let id = checked_pre_verified_id(&self.id)?;
        if language.trim().is_empty() {
            return Err(ValidationError::new("language", "must not be empty").into());
        }
        self.client
            .post_at(&[id, "request_code"])
            .json(&RequestCodeBody {
                code_method: method,
                language,
            })
            .context("pre-verified request code response")
            .send_success()
            .await
    }

    /// `POST /{Pre-Verified-Phone-Number-ID}/verify_code`: submit the code
    /// received; the number is then `VERIFIED` for 90 days. The code goes
    /// in the JSON body, never the URL. Meta limits verifications (125 an
    /// hour), and a code is single-use: not replayed after a timeout.
    pub async fn verify_code(&self, code: &VerificationCode) -> Result<()> {
        let id = checked_pre_verified_id(&self.id)?;
        self.client
            .post_at(&[id, "verify_code"])
            .json(&VerifyCodeBody {
                code: code.expose_secret(),
            })
            .context("pre-verified verify code response")
            .send_success()
            .await
    }

    fn partners_request(&self, query: &ListPreVerifiedNumberPartners) -> Result<GraphRequest> {
        let id = checked_pre_verified_id(&self.id)?;
        check_limit(query.limit)?;
        Ok(self
            .client
            .get_at(&[id, "partners"])
            .query_opt("fields", fields_query(&query.fields))
            .query_opt("limit", query.limit)
            .context("pre-verified phone number partners"))
    }

    /// `GET /{Pre-Verified-Phone-Number-ID}/partners`, one page: the
    /// partner businesses the number is shared with (Business nodes; `id`
    /// and `name` unless the query asks for more fields). The next page:
    /// the same query with `after` set to this page's
    /// [`Page::next_cursor`].
    pub async fn partners(
        &self,
        query: &ListPreVerifiedNumberPartners,
    ) -> Result<Page<BusinessInfo>> {
        self.partners_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// Every partner, following cursors. The stream manages them itself: a
    /// query with `after` or `before` set is refused (the stream's single
    /// item is that validation error).
    pub fn partners_stream(
        &self,
        query: &ListPreVerifiedNumberPartners,
    ) -> impl Stream<Item = Result<BusinessInfo>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.partners_request(query)),
        )
    }

    /// `DELETE /{Pre-Verified-Phone-Number-ID}`: remove the number from the
    /// pool. **Irreversible** (Meta: "cannot be undone"); Meta refuses a
    /// number in use. To offer it again, add it again and verify it. An
    /// explicit call nothing else in the library makes.
    pub async fn delete(&self) -> Result<()> {
        let id = checked_pre_verified_id(&self.id)?;
        self.client
            .delete_at(&[id])
            .context("delete pre-verified phone number response")
            .send_success()
            .await
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use meta_whatsapp_core::error::TransportError;
    use meta_whatsapp_core::ids::WabaId;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use meta_whatsapp_core::{Error, ErrorKind};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::RetryPolicy;
    use crate::waba::NewPhoneNumber;

    /// The guide's ids: the partner portfolio and the pre-verified number
    /// (`embedded-signup/pre-verified-numbers`, Steps 1–3), and the
    /// sharing example's number and partner.
    const BUSINESS: &str = "506914307656634";
    const PRE_VERIFIED: &str = "106540352242922";
    const SHARED: &str = "1706193509821738";
    const PARTNER: &str = "506914307656634";

    fn retrying() -> RetryPolicy {
        RetryPolicy {
            max_retries: 3,
            base_delay: std::time::Duration::ZERO,
            max_delay: std::time::Duration::ZERO,
        }
    }

    fn client_with(t: &ScriptedTransport, retry: RetryPolicy) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token("EAAJB-TOKEN")
            .retry(retry)
            .build()
            .unwrap()
    }

    fn client(t: &ScriptedTransport) -> Client {
        client_with(t, RetryPolicy::NONE)
    }

    fn business(c: &Client) -> Business {
        c.business(BusinessId::new(BUSINESS).unwrap())
    }

    fn number(c: &Client) -> PreVerifiedPhoneNumber {
        c.pre_verified_phone_number(PreVerifiedPhoneNumberId::new(PRE_VERIFIED).unwrap())
    }

    /// The verify-code reference's 403 example.
    fn permission_denied() -> serde_json::Value {
        json!({"error": {
            "message": "Your app doesn't have permission to access this Pre-Verified Phone Number",
            "type": "OAuthException",
            "code": 200,
            "error_subcode": 1349174,
            "fbtrace_id": "AXsgnV2Cm3ZMGF3dF_cfYIn",
            "error_user_title": "Permission Denied",
            "error_user_msg": "Your app doesn't have permission to access this resource"
        }})
    }

    /// Step 1: the guide's sample request and answer. The number goes in
    /// the reference's JSON body (`AddPhoneNumbersRequest`), not the URL.
    #[tokio::test]
    async fn adding_sends_the_number_in_the_body_and_parses_the_guides_answer() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"id": "106540352242922"}));
        let c = client(&t);
        let created = business(&c)
            .add_pre_verified_number("15550783881")
            .await
            .unwrap();
        assert_eq!(created.id.as_str(), PRE_VERIFIED);
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/506914307656634/add_phone_numbers");
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("EAAJB-TOKEN"));
        assert_eq!(req.json(), Some(json!({"phone_number": "15550783881"})));
        assert_eq!(t.remaining(), 0);
    }

    /// The reference's formats pass as given (Meta normalizes them);
    /// anything that is not a phone number is refused before a request.
    #[tokio::test]
    async fn a_number_to_add_is_checked_before_any_request() {
        let t = ScriptedTransport::new();
        for _ in 0..3 {
            t.push_json(200, json!({"id": "1"}));
        }
        let c = client(&t);
        for good in ["+1234567890", "+1 (631) 555-1000", "+1-631-555-1000"] {
            business(&c).add_pre_verified_number(good).await.unwrap();
            assert_eq!(
                t.last_request().unwrap().json(),
                Some(json!({"phone_number": good}))
            );
        }
        for bad in [
            "",
            " ",
            "+",
            "1555abc0000",
            "1555/0000",
            "1555\n0000",
            "１５５５",
        ] {
            let err = business(&c).add_pre_verified_number(bad).await.unwrap_err();
            let Error::Validation(v) = &err else {
                panic!("{bad:?}: {err:?}")
            };
            assert_eq!(v.field, "phone_number", "{bad:?}");
        }
        assert_eq!(t.requests().len(), 3, "no request for a refused number");
        assert_eq!(t.remaining(), 0);
    }

    /// An addition, a code request, a verification and a share are never
    /// replayed after a timeout, with retries on: each has an effect (a
    /// pool entry, a message sent, a single-use code spent, a grant).
    #[tokio::test]
    async fn no_post_is_replayed_after_a_timeout() {
        let code = VerificationCode::new("123456").unwrap();
        for which in ["add", "request_code", "verify_code", "share"] {
            let t = ScriptedTransport::new();
            t.push_error(|| TransportError::Timeout);
            t.push_json(200, json!({"success": true, "id": "1"}));
            let c = client_with(&t, retrying());
            let outcome = match which {
                "add" => business(&c)
                    .add_pre_verified_number("15550783881")
                    .await
                    .map(|_| ()),
                "request_code" => number(&c).request_code(CodeMethod::Sms, "en_US").await,
                "verify_code" => number(&c).verify_code(&code).await,
                _ => {
                    business(&c)
                        .share_pre_verified_number(
                            &PreVerifiedPhoneNumberId::new(SHARED).unwrap(),
                            &BusinessId::new(PARTNER).unwrap(),
                        )
                        .await
                }
            };
            let err = outcome.unwrap_err();
            assert!(matches!(err, Error::Transport(_)), "{which}: {err:?}");
            assert_eq!(t.requests().len(), 1, "{which}: replayed after a timeout");
            assert_eq!(t.remaining(), 1);
        }
    }

    /// § Get pre-verified business phone numbers: every documented query
    /// parameter by its name. The page has no response example: the answer
    /// is `PreVerifiedPhoneNumbersResponse`'s fields with the guide's ids
    /// (values ours).
    #[tokio::test]
    async fn the_pool_is_listed_with_every_documented_filter() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "data": [
                    {
                        "id": "106540352242922",
                        "code_verification_status": "VERIFIED",
                        "code_verification_time": 1760000000,
                        "owner_business": {"id": "506914307656634", "name": "Lucky Shrub Partners"},
                        "phone_number": "+15550783881",
                        "verification_expiry_time": 1767776000
                    },
                    {"id": "105954558954427", "code_verification_status": "SOMETHING_NEW"}
                ],
                "paging": {"cursors": {"before": "QVFIUb", "after": "QVFIUa"}}
            }),
        );
        let c = client(&t);
        let page = business(&c)
            .pre_verified_numbers(
                &ListPreVerifiedNumbers::new()
                    .fields(["id", "code_verification_status", "verification_expiry_time"])
                    .phone_number("+15550783881")
                    .code_verification_status(CodeVerificationStatus::Verified)
                    .limit(10)
                    .after("QVFIUz"),
            )
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/506914307656634/preverified_numbers");
        assert_eq!(req.bearer(), Some("EAAJB-TOKEN"));
        assert_eq!(
            req.query("fields").as_deref(),
            Some("id,code_verification_status,verification_expiry_time")
        );
        assert_eq!(req.query("phone_number").as_deref(), Some("+15550783881"));
        assert_eq!(
            req.query("code_verification_status").as_deref(),
            Some("VERIFIED")
        );
        assert_eq!(req.query("limit").as_deref(), Some("10"));
        assert_eq!(req.query("after").as_deref(), Some("QVFIUz"));
        assert_eq!(req.query("before"), None);
        assert_eq!(t.remaining(), 0);

        let first = &page.data[0];
        assert_eq!(first.id.as_str(), PRE_VERIFIED);
        assert_eq!(
            first.code_verification_status,
            Some(CodeVerificationStatus::Verified)
        );
        assert_eq!(first.code_verification_time, Some(1_760_000_000));
        assert_eq!(first.verification_expiry_time, Some(1_767_776_000));
        assert_eq!(first.phone_number.as_deref(), Some("+15550783881"));
        let owner = first.owner_business.as_ref().unwrap();
        assert_eq!(owner.id.as_ref().map(BusinessId::as_str), Some(BUSINESS));
        assert_eq!(owner.name.as_deref(), Some("Lucky Shrub Partners"));
        // A state Meta adds later parses.
        assert_eq!(
            page.data[1].code_verification_status,
            Some(CodeVerificationStatus::Other("SOMETHING_NEW".into()))
        );
    }

    /// The guide's filtered request, and each status by its wire name.
    #[tokio::test]
    async fn each_status_filter_has_the_references_name() {
        let t = ScriptedTransport::new();
        let c = client(&t);
        for (status, wire) in [
            (CodeVerificationStatus::Verified, "VERIFIED"),
            (CodeVerificationStatus::NotVerified, "NOT_VERIFIED"),
            (CodeVerificationStatus::Expired, "EXPIRED"),
        ] {
            t.push_json(200, json!({"data": []}));
            business(&c)
                .pre_verified_numbers(
                    &ListPreVerifiedNumbers::new().code_verification_status(status),
                )
                .await
                .unwrap();
            let req = t.last_request().unwrap();
            assert_eq!(
                req.url.query(),
                Some(format!("code_verification_status={wire}").as_str())
            );
        }
        t.push_json(200, json!({"data": []}));
        business(&c)
            .pre_verified_numbers(&ListPreVerifiedNumbers::new())
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().url.query(),
            None,
            "no filter, no query"
        );
        assert_eq!(t.remaining(), 0);
    }

    /// What the reference does not allow is refused before any request: a
    /// status outside its three values, a page size of 0.
    #[tokio::test]
    async fn a_list_meta_would_refuse_is_refused_before_any_request() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"data": []}));
        let c = client(&t);
        for (query, field) in [
            (
                ListPreVerifiedNumbers::new()
                    .code_verification_status(CodeVerificationStatus::Unverified),
                "code_verification_status",
            ),
            (
                ListPreVerifiedNumbers::new().code_verification_status(
                    CodeVerificationStatus::Other("SOMETHING_NEW".into()),
                ),
                "code_verification_status",
            ),
            (ListPreVerifiedNumbers::new().limit(0), "limit"),
        ] {
            let err = business(&c).pre_verified_numbers(&query).await.unwrap_err();
            let Error::Validation(v) = &err else {
                panic!("{err:?}")
            };
            assert_eq!(v.field, field);
        }
        assert!(t.requests().is_empty());
        assert_eq!(t.remaining(), 1);
    }

    #[tokio::test]
    async fn the_pool_stream_follows_two_pages_and_refuses_a_cursor() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "1"}], "paging": {"cursors": {"after": "c1"}, "next": "https://graph.facebook.com/x"}}),
        );
        t.push_json(
            200,
            json!({"data": [{"id": "2"}], "paging": {"cursors": {"after": "c2"}}}),
        );
        let c = client(&t);
        let query = ListPreVerifiedNumbers::new()
            .code_verification_status(CodeVerificationStatus::Verified);
        let ids: Vec<String> = business(&c)
            .pre_verified_numbers_stream(&query)
            .map(|n| n.unwrap().id.into_inner())
            .collect()
            .await;
        assert_eq!(ids, ["1", "2"]);
        let reqs = t.requests();
        assert_eq!(reqs[0].query("after"), None);
        assert_eq!(reqs[1].query("after").as_deref(), Some("c1"));
        assert_eq!(
            reqs[1].query("code_verification_status").as_deref(),
            Some("VERIFIED")
        );
        assert_eq!(t.remaining(), 0);

        let refused: Vec<_> = business(&c)
            .pre_verified_numbers_stream(&ListPreVerifiedNumbers::new().after("c1"))
            .collect()
            .await;
        assert!(matches!(refused.as_slice(), [Err(Error::Validation(_))]));
        assert_eq!(t.requests().len(), 2, "no request for a refused stream");
    }

    /// § Sharing: the guide's ids, in the reference's JSON body
    /// (`PreVerifiedPhoneNumberShareRequest`), and its `{"success": true}`.
    #[tokio::test]
    async fn sharing_sends_both_ids_in_the_body() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        let c = client(&t);
        business(&c)
            .share_pre_verified_number(
                &PreVerifiedPhoneNumberId::new(SHARED).unwrap(),
                &BusinessId::new(PARTNER).unwrap(),
            )
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(
            req.path(),
            "/v25.0/506914307656634/share_preverified_numbers"
        );
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("EAAJB-TOKEN"));
        assert_eq!(
            req.json(),
            Some(
                json!({"preverified_id": "1706193509821738", "partner_business_id": "506914307656634"})
            )
        );
        assert_eq!(t.remaining(), 0);
    }

    /// `GET /{Pre-Verified-Phone-Number-ID}`: the guide's two timing
    /// fields asked by name, and the answer's fields read.
    #[tokio::test]
    async fn a_number_is_read_with_its_fields() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "id": "106540352242922",
                "code_verification_status": "EXPIRED",
                "code_verification_time": 1752000000,
                "verification_expiry_time": 1759776000
            }),
        );
        let c = client(&t);
        let info = number(&c)
            .get(&["code_verification_time", "verification_expiry_time"])
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/106540352242922");
        assert_eq!(
            req.query("fields").as_deref(),
            Some("code_verification_time,verification_expiry_time")
        );
        assert_eq!(req.bearer(), Some("EAAJB-TOKEN"));
        assert_eq!(
            info.code_verification_status,
            Some(CodeVerificationStatus::Expired)
        );
        assert_eq!(info.code_verification_time, Some(1_752_000_000));
        assert_eq!(info.verification_expiry_time, Some(1_759_776_000));
        assert_eq!(info.owner_business, None);
        assert_eq!(t.remaining(), 0);
    }

    /// Step 2: the guide's `code_method=SMS&language=en_US`, in the
    /// reference's JSON body (`RequestCodeRequest`).
    #[tokio::test]
    async fn a_code_request_sends_method_and_language() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        t.push_json(200, json!({"success": true}));
        let c = client(&t);
        number(&c)
            .request_code(CodeMethod::Sms, "en_US")
            .await
            .unwrap();
        number(&c)
            .request_code(CodeMethod::Voice, "pt_BR")
            .await
            .unwrap();
        let reqs = t.requests();
        assert_eq!(reqs[0].method, Method::POST);
        assert_eq!(reqs[0].path(), "/v25.0/106540352242922/request_code");
        assert_eq!(reqs[0].url.query(), None);
        assert_eq!(reqs[0].bearer(), Some("EAAJB-TOKEN"));
        assert_eq!(
            reqs[0].json(),
            Some(json!({"code_method": "SMS", "language": "en_US"}))
        );
        assert_eq!(
            reqs[1].json(),
            Some(json!({"code_method": "VOICE", "language": "pt_BR"}))
        );
        assert_eq!(t.remaining(), 0);

        let err = number(&c)
            .request_code(CodeMethod::Sms, " ")
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Validation(ref v) if v.field == "language"),
            "{err:?}"
        );
        assert_eq!(t.requests().len(), 2, "no request for a blank language");
    }

    /// Step 3: the code of the guide's sample SMS (`WhatsApp code
    /// 123-456`), as a user copies it, reaches the reference's JSON body
    /// (`VerifyCodeRequest`) without the hyphen, and never the URL.
    #[tokio::test]
    async fn verify_code_sends_the_code_in_the_body_only() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        let c = client(&t);
        let code = VerificationCode::new("123-456").unwrap();
        number(&c).verify_code(&code).await.unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/106540352242922/verify_code");
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("EAAJB-TOKEN"));
        assert_eq!(req.json(), Some(json!({"code": "123456"})));
        assert_eq!(t.remaining(), 0);
    }

    /// The code is a secret: not in the request's URL, nor in `Debug` of
    /// the code or the API, nor in the error Meta's refusal becomes.
    #[tokio::test]
    async fn a_verification_code_reaches_no_debug_url_or_error() {
        const SENTINEL: &str = "908172";
        let t = ScriptedTransport::new();
        t.push_json(403, permission_denied());
        let c = client(&t);
        let code = VerificationCode::new(SENTINEL).unwrap();
        let api = number(&c);
        let err = api.verify_code(&code).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Permission);
        let shown = format!("{err} {err:?} {code:?} {api:?}");
        assert!(!shown.contains(SENTINEL), "{shown}");
        let req = t.last_request().unwrap();
        assert!(!req.url.as_str().contains(SENTINEL), "{}", req.url);
        assert_eq!(req.json(), Some(json!({"code": SENTINEL})));
        assert_eq!(t.remaining(), 0);
    }

    /// The partners edge: every documented query parameter by its name,
    /// and `BusinessPartner`'s `id` and `name` (values ours; the page has
    /// no response example).
    #[tokio::test]
    async fn the_partners_are_listed_with_the_documented_parameters() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "data": [{"id": "506914307656634", "name": "Lucky Shrub Partners"}],
                "paging": {"cursors": {"before": "QVFIUb", "after": "QVFIUa"}}
            }),
        );
        let c = client(&t);
        let page = number(&c)
            .partners(
                &ListPreVerifiedNumberPartners::new()
                    .fields(["id", "name"])
                    .limit(100)
                    .before("QVFIUb"),
            )
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/106540352242922/partners");
        assert_eq!(req.bearer(), Some("EAAJB-TOKEN"));
        assert_eq!(req.query("fields").as_deref(), Some("id,name"));
        assert_eq!(req.query("limit").as_deref(), Some("100"));
        assert_eq!(req.query("before").as_deref(), Some("QVFIUb"));
        assert_eq!(req.query("after"), None);
        assert_eq!(page.data[0].id.as_str(), PARTNER);
        assert_eq!(page.data[0].name.as_deref(), Some("Lucky Shrub Partners"));
        assert_eq!(page.next_cursor(), None, "no `next` link, no next page");
        assert_eq!(t.remaining(), 0);

        for limit in [0, 101] {
            let err = number(&c)
                .partners(&ListPreVerifiedNumberPartners::new().limit(limit))
                .await
                .unwrap_err();
            assert!(
                matches!(err, Error::Validation(ref v) if v.field == "limit"),
                "{limit}"
            );
        }
        assert_eq!(t.requests().len(), 1);
    }

    #[tokio::test]
    async fn the_partners_stream_follows_two_pages_and_refuses_a_cursor() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "11", "name": "A"}], "paging": {"cursors": {"after": "p1"}, "next": "https://graph.facebook.com/x"}}),
        );
        t.push_json(200, json!({"data": [{"id": "12", "name": "B"}]}));
        let c = client(&t);
        let ids: Vec<String> = number(&c)
            .partners_stream(&ListPreVerifiedNumberPartners::new().limit(1))
            .map(|b| b.unwrap().id.into_inner())
            .collect()
            .await;
        assert_eq!(ids, ["11", "12"]);
        let reqs = t.requests();
        assert_eq!(reqs[1].path(), "/v25.0/106540352242922/partners");
        assert_eq!(reqs[1].query("after").as_deref(), Some("p1"));
        assert_eq!(reqs[1].query("limit").as_deref(), Some("1"));
        assert_eq!(t.remaining(), 0);

        let refused: Vec<_> = number(&c)
            .partners_stream(&ListPreVerifiedNumberPartners::new().before("p0"))
            .collect()
            .await;
        assert!(matches!(refused.as_slice(), [Err(Error::Validation(_))]));
        assert_eq!(t.requests().len(), 2);
    }

    /// `DELETE /{Pre-Verified-Phone-Number-ID}` and its `success`; a Graph
    /// error keeps its kind (the 403 example).
    #[tokio::test]
    async fn delete_addresses_the_number_and_maps_meta_errors() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"success": true}));
        t.push_json(403, permission_denied());
        let c = client(&t);
        number(&c).delete().await.unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::DELETE);
        assert_eq!(req.path(), "/v25.0/106540352242922");
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("EAAJB-TOKEN"));
        let err = number(&c).delete().await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert_eq!(t.remaining(), 0);
    }

    /// Decisive (roadmap L11c): a pre-verified id is checked as digits
    /// before any request. The checked constructor refuses one holding a
    /// separator; one built unchecked (as from a response) is refused by
    /// every call that would put it in a path or a body. Every answer is
    /// scripted, so an id let through would be sent and recorded: none is.
    #[tokio::test]
    async fn a_pre_verified_id_that_is_not_digits_is_refused_before_any_request() {
        for hostile in [
            "123/subscribed_apps",
            "..",
            "",
            "106540352242922?fields=x",
            "abc",
        ] {
            assert_eq!(
                PreVerifiedPhoneNumberId::new(hostile).unwrap_err().field,
                "pre_verified_phone_number_id"
            );
            let t = ScriptedTransport::new();
            for _ in 0..8 {
                t.push_json(200, json!({"success": true, "id": "1", "data": []}));
            }
            let c = client(&t);
            let id = PreVerifiedPhoneNumberId::new_unchecked(hostile);
            let api = c.pre_verified_phone_number(id.clone());
            let code = VerificationCode::new("123456").unwrap();
            let mut outcomes: Vec<(&str, Result<()>)> = vec![
                ("get", api.get(&[]).await.map(|_| ())),
                (
                    "request_code",
                    api.request_code(CodeMethod::Sms, "en_US").await,
                ),
                ("verify_code", api.verify_code(&code).await),
                (
                    "partners",
                    api.partners(&ListPreVerifiedNumberPartners::new())
                        .await
                        .map(|_| ()),
                ),
                ("delete", api.delete().await),
                (
                    "share",
                    business(&c)
                        .share_pre_verified_number(&id, &BusinessId::new(PARTNER).unwrap())
                        .await,
                ),
                (
                    "create_phone_number",
                    c.waba(WabaId::new("102290129340398").unwrap())
                        .create_phone_number(
                            &NewPhoneNumber::new("1", "15550783881", "Lucky Shrub")
                                .preverified_id(id.clone()),
                        )
                        .await
                        .map(|_| ()),
                ),
            ];
            let streamed: Vec<_> = api
                .partners_stream(&ListPreVerifiedNumberPartners::new())
                .collect()
                .await;
            outcomes.push((
                "partners_stream",
                streamed.into_iter().next().unwrap().map(|_| ()),
            ));
            for (call, outcome) in outcomes {
                let err = outcome.unwrap_err();
                let Error::Validation(v) = &err else {
                    panic!("{hostile:?} {call}: {err:?}")
                };
                assert!(
                    v.field == "pre_verified_phone_number_id" || v.field == "preverified_id",
                    "{hostile:?} {call}: {v:?}"
                );
            }
            assert!(t.requests().is_empty(), "{hostile:?}: {:?}", t.requests());
        }
    }

    /// Registering a pre-verified number programmatically: the WABA's
    /// `POST phone_numbers` carries `preverified_id` (§ Registering
    /// pre-verified numbers programmatically).
    #[tokio::test]
    async fn a_new_number_carries_the_pre_verified_id() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"id": "110200345501442"}));
        let c = client(&t);
        c.waba(WabaId::new("102290129340398").unwrap())
            .create_phone_number(
                &NewPhoneNumber::new("1", "15550783881", "Lucky Shrub")
                    .preverified_id(PreVerifiedPhoneNumberId::new(PRE_VERIFIED).unwrap()),
            )
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.path(), "/v25.0/102290129340398/phone_numbers");
        assert_eq!(
            req.json(),
            Some(json!({
                "cc": "1",
                "phone_number": "15550783881",
                "verified_name": "Lucky Shrub",
                "preverified_id": "106540352242922"
            }))
        );
        assert_eq!(t.remaining(), 0);
    }
}
