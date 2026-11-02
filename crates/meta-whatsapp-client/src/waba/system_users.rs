//! System users: a business portfolio's system users
//! (`solution-providers/manage-system-users`, the Marketing API's
//! `reference/business/system_users`), the system user token of a client
//! business (`system_user_access_tokens`), and the WABAs assigned to a user
//! (`reference/user/assigned-whatsapp-business-accounts-api`).

use futures::Stream;
use hmac::{Hmac, Mac};
use meta_whatsapp_core::error::ValidationError;
use meta_whatsapp_core::ids::SystemUserId;
use meta_whatsapp_core::open_enum;
use meta_whatsapp_core::paging::Page;
use meta_whatsapp_core::secret::{AccessToken, AppSecret};
use meta_whatsapp_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use super::types::WabaInfo;
use super::{Business, check_limit, fields_query};
use crate::request::{paginate_or_error, reject_cursors};
use crate::{Client, GraphRequest};

open_enum! {
    #![case_insensitive]
    /// A system user's `role`. The Marketing API's
    /// `reference/business/system_users` says the list carries base roles
    /// only, "Admin and Employee" (`ADMIN` and `EMPLOYEE` among the roles it
    /// lists for creating one); `manage-system-users` shows `EMPLOYEE`.
    /// Anything else is kept verbatim.
    pub enum SystemUserRole {
        /// An admin system user: access to every account of the portfolio
        /// by default (`access-tokens`).
        Admin => "ADMIN",
        /// An employee system user: granted access account by account.
        Employee => "EMPLOYEE",
    }
}

/// One of a business portfolio's system users
/// (`GET /{BUSINESS_ID}/system_users`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct SystemUser {
    /// The system user's id: what [`Waba::assign_user`](super::Waba::assign_user) takes.
    pub id: SystemUserId,
    /// Its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<SystemUserRole>,
}

impl Business {
    /// `GET /{BUSINESS_ID}/system_users` (`solution-providers/manage-system-users`,
    /// § Retrieve system user IDs; the Marketing API's
    /// `reference/business/system_users`): the portfolio's system users, e.g.
    /// to find the id to pass to [`Waba::assign_user`](super::Waba::assign_user).
    ///
    /// On the business portfolio, not the WABA: the guide's request prints a
    /// WABA id, but its own next line says to look up the *business*'s id,
    /// and the edge is the Business node's (see
    /// [the module docs](crate::waba#where-metas-pages-disagree)).
    ///
    /// Neither page documents a parameter, so this takes none (as
    /// [`Waba::subscribed_apps`](super::Waba::subscribed_apps)); [`Self::system_users_stream`] follows the
    /// cursors of a paged answer.
    pub async fn system_users(&self) -> Result<Page<SystemUser>> {
        self.system_users_request().send().await
    }

    /// All system users, following cursors.
    pub fn system_users_stream(
        &self,
    ) -> impl Stream<Item = Result<SystemUser>> + Send + 'static + use<> {
        self.system_users_request().paginate()
    }

    fn system_users_request(&self) -> GraphRequest {
        self.client
            .get_at(&[self.business_id.as_str(), "system_users"])
            .context("system users")
    }
}

/// What to ask `POST /{BUSINESS_ID}/system_user_access_tokens` for: the
/// parameters of `facebook-login/facebook-login-for-business` (§ Get tokens)
/// and of the Marketing API reference
/// (`ads-commerce/marketing-api/reference/business/system_user_access_tokens`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SystemUserTokenRequest {
    /// `fetch_only`: fetch the existing token instead of generating one, a
    /// read-only operation (what every example of Meta's sends).
    pub fetch_only: bool,
    /// `system_user_id`: the system user in the token (an integer).
    pub system_user_id: Option<SystemUserId>,
    /// `scope`: for a more granular token, permission names, a subset of
    /// the original token's (sent comma-separated).
    pub scope: Vec<String>,
    /// `asset`: for a more granular token, asset ids (integers), a subset
    /// of the original token's (sent comma-separated).
    pub asset: Vec<String>,
    /// `set_token_expires_in_60_days`: when generating a token, make it
    /// expire in 60 days.
    pub set_token_expires_in_60_days: Option<bool>,
}

impl SystemUserTokenRequest {
    /// `fetch_only=true`: the client business's existing token, as
    /// `embedded-signup/hosted-es` (step 5),
    /// `marketing-messages/onboard-business-customers` and
    /// `facebook-login-for-business` fetch it.
    pub fn fetch_only() -> Self {
        Self {
            fetch_only: true,
            ..Self::default()
        }
    }

    /// Generate a token (no `fetch_only`), e.g. a more granular one with
    /// [`Self::scope`] and [`Self::asset`].
    pub fn new() -> Self {
        Self::default()
    }

    /// For this system user.
    #[must_use]
    pub fn system_user_id(mut self, id: impl Into<SystemUserId>) -> Self {
        self.system_user_id = Some(id.into());
        self
    }

    /// Only these permissions.
    #[must_use]
    pub fn scope<I, S>(mut self, permissions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.scope = permissions.into_iter().map(Into::into).collect();
        self
    }

    /// Only these assets.
    #[must_use]
    pub fn asset<I, S>(mut self, asset_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.asset = asset_ids.into_iter().map(Into::into).collect();
        self
    }

    /// Set `set_token_expires_in_60_days`.
    #[must_use]
    pub fn set_token_expires_in_60_days(mut self, expires: bool) -> Self {
        self.set_token_expires_in_60_days = Some(expires);
        self
    }

    /// Check what the pages state: `system_user_id` and each `asset` are
    /// integers, each `scope` entry a permission name (not blank, and no
    /// comma, which would split it in the comma-separated list).
    ///
    /// # Errors
    ///
    /// A [`ValidationError`] on `system_user_id`, `asset` or `scope`.
    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        let integer = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if let Some(id) = &self.system_user_id
            && !integer(id.as_str())
        {
            return Err(ValidationError::new(
                "system_user_id",
                "must be an integer id",
            ));
        }
        if self.asset.iter().any(|a| !integer(a)) {
            return Err(ValidationError::new(
                "asset",
                "each asset id must be an integer",
            ));
        }
        if self
            .scope
            .iter()
            .any(|p| p.trim().is_empty() || p.contains(','))
        {
            return Err(ValidationError::new(
                "scope",
                "each permission must be a name, without a comma",
            ));
        }
        Ok(())
    }

    fn form(&self, appsecret_proof: &str) -> String {
        let mut form = url::form_urlencoded::Serializer::new(String::new());
        form.append_pair("appsecret_proof", appsecret_proof);
        if self.fetch_only {
            form.append_pair("fetch_only", "true");
        }
        if let Some(id) = &self.system_user_id {
            form.append_pair("system_user_id", id.as_str());
        }
        if !self.scope.is_empty() {
            form.append_pair("scope", &self.scope.join(","));
        }
        if !self.asset.is_empty() {
            form.append_pair("asset", &self.asset.join(","));
        }
        if let Some(expires) = self.set_token_expires_in_60_days {
            form.append_pair(
                "set_token_expires_in_60_days",
                if expires { "true" } else { "false" },
            );
        }
        form.finish()
    }
}

/// A client business's system user token ("business token"), from
/// [`Business::system_user_access_token`]: `{"access_token": "…"}`.
///
/// `Debug` never shows the token ([`AccessToken`]); use it with
/// [`Client::with_token`] and store it in the vault, never in a log.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct SystemUserAccessToken {
    /// The token (an empty or blank one fails to decode).
    #[serde(deserialize_with = "non_empty_token")]
    pub access_token: AccessToken,
}

/// An `access_token` that is not blank. The error names no value: the
/// caller withholds serde's message anyway.
fn non_empty_token<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<AccessToken, D::Error> {
    let token = AccessToken::deserialize(d)?;
    if token.expose_secret().trim().is_empty() {
        Err(serde::de::Error::custom("empty access_token"))
    } else {
        Ok(token)
    }
}

/// The body snippet of an error for an answer that carries a token.
const TOKEN_WITHHELD_TEXT: &str = "(withheld: the response carries an access token)";

/// [`TOKEN_WITHHELD_TEXT`] as bytes, the decode error's placeholder.
const TOKEN_WITHHELD: &[u8] = TOKEN_WITHHELD_TEXT.as_bytes();

/// A failed token request keeps its class and status, never the body: a
/// non-2xx answer that is not Meta's error envelope (a proxy's page, an
/// answer cut short) could still hold the token, whatever key it is under,
/// which the markers of `request::safe_snippet` would not see.
fn withhold_token_body(e: Error) -> Error {
    match e {
        Error::Http { status, .. } => Error::Http {
            status,
            body_snippet: TOKEN_WITHHELD_TEXT.to_owned(),
        },
        other => other,
    }
}

/// `appsecret_proof`: the hex HMAC-SHA256 of `token` keyed with the app
/// secret (`embedded-signup/hosted-es`, step 4; Graph's secure requests).
fn appsecret_proof(app_secret: &AppSecret, token: &AccessToken) -> Result<String> {
    if app_secret.expose_secret().trim().is_empty() {
        return Err(ValidationError::new("app_secret", "required").into());
    }
    // HMAC takes a key of any length; the error is handled, not unwrapped.
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(app_secret.expose_secret().as_bytes())
        .map_err(|_| {
            Error::Crypto(meta_whatsapp_core::error::CryptoError::InvalidKey(
                "app secret",
            ))
        })?;
    mac.update(token.expose_secret().as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

impl Business {
    /// `POST /{BUSINESS_ID}/system_user_access_tokens`: the token of the
    /// client business this API is scoped to (its business integration
    /// system user token, "business token"), asked for with the client's
    /// token, your system user token (`embedded-signup/hosted-es`, step 5:
    /// target the customer's business portfolio id, not yours).
    ///
    /// The request carries the `appsecret_proof` of that token, computed
    /// here from `app_secret`, in a form body with the request's fields (as
    /// `hosted-es` sends it), never in the URL; the token goes in the
    /// `Authorization` header. No error quotes the answer's body, whether
    /// it fails to decode or comes with a non-2xx status that is not Meta's
    /// error envelope, and a blank `access_token` is refused.
    ///
    /// Refused before any request when the client has no token, the app
    /// secret is blank or the request does not validate
    /// ([`SystemUserTokenRequest::validate`]). A `fetch_only` request is
    /// read-only (`facebook-login-for-business`), so it is replayed on
    /// transient errors as a read is; one that generates a token is **never
    /// replayed** on a timeout or a transient error: only an error proving
    /// Meta refused it is resent ([`crate::RetryPolicy`]).
    pub async fn system_user_access_token(
        &self,
        app_secret: &AppSecret,
        request: &SystemUserTokenRequest,
    ) -> Result<SystemUserAccessToken> {
        let Some(token) = self.client.token.as_ref() else {
            return Err(ValidationError::new(
                "access_token",
                "the client needs your system user token to ask for a client's",
            )
            .into());
        };
        request.validate()?;
        let proof = appsecret_proof(app_secret, token)?;
        self.client
            .post_at(&[self.business_id.as_str(), "system_user_access_tokens"])
            .bytes("application/x-www-form-urlencoded", request.form(&proof))
            .idempotent(request.fetch_only)
            .context("system user access token response")
            .send_withheld(TOKEN_WITHHELD)
            .await
            .map_err(withhold_token_body)
    }
}

/// Options for [`BusinessUser::assigned_whatsapp_business_accounts`]
/// (`reference/user/assigned-whatsapp-business-accounts-api`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListAssignedWabas {
    /// Fields to return (empty = Meta's defaults, `id` and `name`; those
    /// are the only ones the reference lists).
    pub fields: Vec<String>,
    /// Page size, 1–100 (Meta's default: 25).
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`; for the
    /// one-page method only, the stream manages its own.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

impl ListAssignedWabas {
    /// Meta's defaults.
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

/// Entry point, see [`Client::business_user`]: a user of a business (a
/// system user, e.g. the one [`Business::system_users`] lists).
#[derive(Debug, Clone)]
pub struct BusinessUser {
    client: Client,
    user_id: SystemUserId,
}

impl Client {
    /// [`BusinessUser`] API for `user_id`
    /// (`reference/user/assigned-whatsapp-business-accounts-api`).
    pub fn business_user(&self, user_id: impl Into<SystemUserId>) -> BusinessUser {
        BusinessUser {
            client: self.clone(),
            user_id: user_id.into(),
        }
    }
}

impl BusinessUser {
    /// The id this API is scoped to.
    pub fn id(&self) -> &SystemUserId {
        &self.user_id
    }

    fn assigned_request(&self, query: &ListAssignedWabas) -> Result<GraphRequest> {
        check_limit(query.limit)?;
        Ok(self
            .client
            .get_at(&[self.user_id.as_str(), "assigned_whatsapp_business_accounts"])
            .query_opt("fields", fields_query(&query.fields))
            .query_opt("limit", query.limit)
            .context("assigned WhatsApp Business Accounts"))
    }

    /// `GET /{USER_ID}/assigned_whatsapp_business_accounts`, one page: the
    /// WABAs assigned to this user. The next page: the same query with
    /// `after` set to this page's [`Page::next_cursor`].
    pub async fn assigned_whatsapp_business_accounts(
        &self,
        query: &ListAssignedWabas,
    ) -> Result<Page<WabaInfo>> {
        self.assigned_request(query)?
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// All assigned WABAs, following cursors. The stream manages them
    /// itself: a query with `after` or `before` set is refused (the
    /// stream's single item is that validation error).
    pub fn assigned_whatsapp_business_accounts_stream(
        &self,
        query: &ListAssignedWabas,
    ) -> impl Stream<Item = Result<WabaInfo>> + Send + 'static + use<> {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .and_then(|()| self.assigned_request(query)),
        )
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use meta_whatsapp_core::ErrorKind;
    use meta_whatsapp_core::error::TransportError;
    use meta_whatsapp_core::ids::BusinessId;
    use meta_whatsapp_core::testing::{RecordedBody, RecordedRequest, ScriptedTransport};
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::RetryPolicy;

    /// The partner's system user token, and what the app secret signs.
    const SYSTEM_TOKEN: &str = "EAAJB-SYSTEM-TOKEN-SENTINEL";
    const APP_SECRET: &str = "614fc2afde15eee07a26b2fe3eaee9b9";
    /// The client business's token in the answer: a sentinel that must
    /// never reach `Debug` or an error.
    const BUSINESS_TOKEN: &str = "EAAN-BUSINESS-TOKEN-SENTINEL";

    fn client(t: &ScriptedTransport) -> Client {
        Client::builder()
            .transport(t.clone())
            .access_token(SYSTEM_TOKEN)
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap()
    }

    fn form(req: &RecordedRequest) -> Vec<(String, String)> {
        let RecordedBody::Bytes { content_type, data } = &req.body else {
            panic!("expected a buffered body, got {:?}", req.body);
        };
        assert_eq!(content_type, "application/x-www-form-urlencoded");
        url::form_urlencoded::parse(data)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    /// The proof computed apart from the code under test: HMAC-SHA256 of
    /// the token, keyed with the secret, hex (`hosted-es`, step 4:
    /// `echo -n TOKEN | openssl dgst -sha256 -hmac SECRET`).
    fn expected_proof(secret: &str, token: &str) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(token.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn the_proof_is_the_hmac_of_the_token() {
        // A fixed vector, so that a change of algorithm or encoding shows.
        assert_eq!(
            appsecret_proof(
                &AppSecret::new("key"),
                &AccessToken::new("The quick brown fox jumps over the lazy dog")
            )
            .unwrap(),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    /// The guide's response example; the request on the business
    /// portfolio, as the Business node's `system_users` edge and the guide's
    /// own "find the ID of a business" have it.
    #[tokio::test]
    async fn system_users_parse_the_guides_example() {
        // solution-providers/manage-system-users, § Retrieve system user IDs.
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "1972555232742222", "name": "My System User", "role": "EMPLOYEE"}]}),
        );
        let page = client(&t)
            .business(BusinessId::new("2780023991704").unwrap())
            .system_users()
            .await
            .unwrap();
        assert_eq!(page.data.len(), 1);
        assert_eq!(page.data[0].id.as_str(), "1972555232742222");
        assert_eq!(page.data[0].name.as_deref(), Some("My System User"));
        assert_eq!(page.data[0].role, Some(SystemUserRole::Employee));
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/2780023991704/system_users");
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some(SYSTEM_TOKEN));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn system_users_stream_follows_cursors_should_meta_page_it() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"id": "1", "role": "ADMIN"}], "paging": {"cursors": {"after": "a"}, "next": "https://graph.facebook.com/x"}}),
        );
        t.push_json(
            200,
            json!({"data": [{"id": "2", "role": "FINANCE_EDITOR"}]}),
        );
        let users: Vec<SystemUser> = client(&t)
            .business(BusinessId::new("2780023991704").unwrap())
            .system_users_stream()
            .map(Result::unwrap)
            .collect()
            .await;
        assert_eq!(users[0].role, Some(SystemUserRole::Admin));
        assert_eq!(
            users[1].role,
            Some(SystemUserRole::Other("FINANCE_EDITOR".into()))
        );
        assert_eq!(users[1].id.as_str(), "2");
        let reqs = t.requests();
        assert_eq!(reqs[0].path(), "/v25.0/2780023991704/system_users");
        assert_eq!(reqs[1].path(), "/v25.0/2780023991704/system_users");
        assert_eq!(reqs[1].query("after").as_deref(), Some("a"));
        assert_eq!(t.remaining(), 0);
    }

    /// `embedded-signup/hosted-es`, step 5: a form body with the proof and
    /// `fetch_only=true`, the system token as bearer, the client's
    /// portfolio in the path; the answer's response syntax.
    #[tokio::test]
    async fn a_client_token_is_fetched_with_the_proof_of_the_system_token() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"access_token": BUSINESS_TOKEN}));
        let token = client(&t)
            .business(BusinessId::new("2780023991704").unwrap())
            .system_user_access_token(
                &AppSecret::new(APP_SECRET),
                &SystemUserTokenRequest::fetch_only(),
            )
            .await
            .unwrap();
        assert_eq!(token.access_token.expose_secret(), BUSINESS_TOKEN);
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/2780023991704/system_user_access_tokens");
        assert_eq!(req.url.query(), None, "nothing in the URL");
        assert_eq!(req.bearer(), Some(SYSTEM_TOKEN));
        assert_eq!(
            form(&req),
            [
                (
                    "appsecret_proof".to_owned(),
                    expected_proof(APP_SECRET, SYSTEM_TOKEN)
                ),
                ("fetch_only".to_owned(), "true".to_owned()),
            ]
        );
        assert_eq!(t.remaining(), 0);

        // A tenant's client signs its own token; a wrong secret gives
        // another proof.
        t.push_json(200, json!({"access_token": BUSINESS_TOKEN}));
        client(&t)
            .with_token("OTHER".into())
            .business(BusinessId::new("2780023991704").unwrap())
            .system_user_access_token(
                &AppSecret::new("another secret"),
                // `facebook-login-for-business`, § Get tokens: the scope's
                // example, comma-separated, as are the assets.
                &SystemUserTokenRequest::new()
                    .system_user_id("1972555232742222")
                    .scope(["pages_show_list", "pages_read_engagement"])
                    .asset(["102290129340398", "106540352242922"])
                    .set_token_expires_in_60_days(true),
            )
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.bearer(), Some("OTHER"));
        let sent = form(&req);
        assert_ne!(sent[0].1, expected_proof(APP_SECRET, "OTHER"));
        assert_eq!(
            sent,
            [
                (
                    "appsecret_proof".to_owned(),
                    expected_proof("another secret", "OTHER")
                ),
                ("system_user_id".to_owned(), "1972555232742222".to_owned()),
                (
                    "scope".to_owned(),
                    "pages_show_list,pages_read_engagement".to_owned()
                ),
                (
                    "asset".to_owned(),
                    "102290129340398,106540352242922".to_owned()
                ),
                ("set_token_expires_in_60_days".to_owned(), "true".to_owned()),
            ]
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_token_request_is_checked_before_any_request() {
        let t = ScriptedTransport::new();
        let b = client(&t).business(BusinessId::new("1").unwrap());
        for (request, field) in [
            (
                SystemUserTokenRequest::new().system_user_id("12a"),
                "system_user_id",
            ),
            (
                SystemUserTokenRequest::new().system_user_id(""),
                "system_user_id",
            ),
            (SystemUserTokenRequest::new().asset(["1", "x"]), "asset"),
            (SystemUserTokenRequest::new().asset([""]), "asset"),
            (SystemUserTokenRequest::new().scope(["a,b"]), "scope"),
            (SystemUserTokenRequest::new().scope([" "]), "scope"),
        ] {
            let err = b
                .system_user_access_token(&AppSecret::new(APP_SECRET), &request)
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == field),
                "{field}: {err:?}"
            );
        }
        assert!(t.requests().is_empty());
    }

    /// The sentinel test: the token reaches neither `Debug` nor an error,
    /// whether the answer decodes, fails to, or holds an empty token; nor
    /// does the proof or the app secret.
    #[tokio::test]
    async fn a_system_user_token_never_reaches_debug_or_an_error() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"access_token": BUSINESS_TOKEN}));
        // `access_token` of the wrong type: serde's message would quote it.
        t.push_json(200, json!({"access_token": [BUSINESS_TOKEN]}));
        t.push_json(200, json!({"token": BUSINESS_TOKEN}));
        t.push_json(200, json!({"access_token": " "}));
        // A non-2xx answer that is not Meta's envelope, holding the token
        // under a key no marker names, and one under `access_token`.
        t.push_json(502, json!({"token": BUSINESS_TOKEN}));
        t.push_json(500, json!({"access_token": BUSINESS_TOKEN}));
        let b = client(&t).business(BusinessId::new("2780023991704").unwrap());
        let secret = AppSecret::new(APP_SECRET);
        let request = SystemUserTokenRequest::fetch_only();
        let ok = b.system_user_access_token(&secret, &request).await.unwrap();
        let mut shown = vec![format!("{ok:?}"), format!("{request:?}"), format!("{b:?}")];
        for _ in 0..3 {
            let err = b
                .system_user_access_token(&secret, &request)
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::Decode { body_snippet, .. } if body_snippet.as_bytes() == TOKEN_WITHHELD),
                "{err:?}"
            );
            shown.push(format!("{err} {err:?}"));
        }
        for status in [502, 500] {
            let err = b
                .system_user_access_token(&secret, &request)
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::Http { status: s, body_snippet } if *s == status && body_snippet == TOKEN_WITHHELD_TEXT),
                "{err:?}"
            );
            shown.push(format!("{err} {err:?}"));
        }
        let proof = expected_proof(APP_SECRET, SYSTEM_TOKEN);
        for text in &shown {
            for secret in [BUSINESS_TOKEN, SYSTEM_TOKEN, APP_SECRET, proof.as_str()] {
                assert!(!text.contains(secret), "{text}");
            }
        }
        assert!(shown[0].contains("REDACTED"), "{}", shown[0]);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_token_request_is_refused_without_a_token_or_a_secret() {
        let t = ScriptedTransport::new();
        let anonymous = Client::builder()
            .transport(t.clone())
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();
        let err = anonymous
            .business(BusinessId::new("1").unwrap())
            .system_user_access_token(
                &AppSecret::new(APP_SECRET),
                &SystemUserTokenRequest::fetch_only(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, Error::Validation(v) if v.field == "access_token"),
            "{err:?}"
        );
        for blank in ["", "  "] {
            let err = client(&t)
                .business(BusinessId::new("1").unwrap())
                .system_user_access_token(
                    &AppSecret::new(blank),
                    &SystemUserTokenRequest::fetch_only(),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == "app_secret"),
                "{err:?}"
            );
        }
        assert!(t.requests().is_empty());
    }

    /// Generating a token is never replayed after a timeout; fetching the
    /// existing one, read-only, is. The reference's `200` keeps its kind.
    #[tokio::test]
    async fn a_token_generation_is_not_replayed_and_a_fetch_is() {
        let t = ScriptedTransport::new();
        t.push_error(|| TransportError::Timeout);
        let retrying = Client::builder()
            .transport(t.clone())
            .access_token(SYSTEM_TOKEN)
            .retry(RetryPolicy {
                max_retries: 3,
                base_delay: std::time::Duration::ZERO,
                max_delay: std::time::Duration::ZERO,
            })
            .build()
            .unwrap();
        let b = retrying.business(BusinessId::new("1").unwrap());
        assert!(
            b.system_user_access_token(
                &AppSecret::new(APP_SECRET),
                &SystemUserTokenRequest::new().scope(["whatsapp_business_messaging"])
            )
            .await
            .is_err()
        );
        assert_eq!(t.requests().len(), 1, "a generation is not replayed");

        t.push_error(|| TransportError::Timeout);
        t.push_json(200, json!({"access_token": BUSINESS_TOKEN}));
        b.system_user_access_token(
            &AppSecret::new(APP_SECRET),
            &SystemUserTokenRequest::fetch_only(),
        )
        .await
        .unwrap();
        let reqs = t.requests();
        assert_eq!(reqs.len(), 3, "a fetch is replayed");
        assert_eq!(form(&reqs[1]), form(&reqs[2]));
        assert_eq!(t.remaining(), 0);

        let t = ScriptedTransport::new();
        t.push_json(
            403,
            json!({"error": {"message": "Permissions error", "type": "OAuthException", "code": 200}}),
        );
        let err = client(&t)
            .business(BusinessId::new("1").unwrap())
            .system_user_access_token(
                &AppSecret::new(APP_SECRET),
                &SystemUserTokenRequest::fetch_only(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Permission);
        assert_eq!(t.remaining(), 0);
    }

    fn assigned_example() -> serde_json::Value {
        // The `AssignedAccountsResponse` schema (the page prints no
        // example): `id`, `name`, and the cursor paging object.
        json!({
            "data": [
                {"id": "106526625562206", "name": "Lucky Shrub"},
                {"id": "102290129340398"}
            ],
            "paging": {"cursors": {"before": "QVFI", "after": "QVFJ"}}
        })
    }

    #[tokio::test]
    async fn assigned_wabas_send_the_documented_query() {
        let t = ScriptedTransport::new();
        t.push_json(200, assigned_example());
        let page = client(&t)
            .business_user("1972555232742222")
            .assigned_whatsapp_business_accounts(
                &ListAssignedWabas::new()
                    .fields(["id", "name"])
                    .limit(100)
                    .after("QVFJ")
                    .before("QVFI"),
            )
            .await
            .unwrap();
        assert_eq!(page.data[0].id.as_str(), "106526625562206");
        assert_eq!(page.data[0].name.as_deref(), Some("Lucky Shrub"));
        assert_eq!(page.data[1].name, None);
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(
            req.path(),
            "/v25.0/1972555232742222/assigned_whatsapp_business_accounts"
        );
        assert_eq!(req.query("fields").as_deref(), Some("id,name"));
        assert_eq!(req.query("limit").as_deref(), Some("100"));
        assert_eq!(req.query("after").as_deref(), Some("QVFJ"));
        assert_eq!(req.query("before").as_deref(), Some("QVFI"));
        assert_eq!(req.bearer(), Some(SYSTEM_TOKEN));
        assert_eq!(t.remaining(), 0);

        // Meta's defaults: no query at all; an id with `/` stays one segment.
        t.push_json(200, assigned_example());
        client(&t)
            .business_user("1/x")
            .assigned_whatsapp_business_accounts(&ListAssignedWabas::new())
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(
            req.path(),
            "/v25.0/1%2Fx/assigned_whatsapp_business_accounts"
        );
        assert_eq!(req.url.query(), None);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn assigned_wabas_check_the_limit_and_stream() {
        let t = ScriptedTransport::new();
        let user = client(&t).business_user("1");
        for limit in [0, 101] {
            let err = user
                .assigned_whatsapp_business_accounts(&ListAssignedWabas::new().limit(limit))
                .await
                .unwrap_err();
            assert!(
                matches!(&err, Error::Validation(v) if v.field == "limit"),
                "{err:?}"
            );
        }
        let refused: Vec<_> = user
            .assigned_whatsapp_business_accounts_stream(&ListAssignedWabas::new().after("x"))
            .collect()
            .await;
        assert!(
            matches!(&refused[..], [Err(Error::Validation(_))]),
            "{refused:?}"
        );
        assert!(t.requests().is_empty());

        t.push_json(
            200,
            json!({"data": [{"id": "1"}], "paging": {"cursors": {"after": "a"}, "next": "https://graph.facebook.com/x"}}),
        );
        t.push_json(200, json!({"data": [{"id": "2"}]}));
        let ids: Vec<String> = user
            .assigned_whatsapp_business_accounts_stream(&ListAssignedWabas::new().limit(1))
            .map(|w| w.unwrap().id.into_inner())
            .collect()
            .await;
        assert_eq!(ids, ["1", "2"]);
        let reqs = t.requests();
        assert_eq!(reqs[0].query("after"), None);
        assert_eq!(reqs[1].query("after").as_deref(), Some("a"));
        assert_eq!(reqs[1].query("limit").as_deref(), Some("1"));
        assert_eq!(t.remaining(), 0);
    }
}
