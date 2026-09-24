//! Block and unblock WhatsApp users, and list the block list.
//!
//! Docs:
//! - `block-users` — the three operations, limits, error codes.
//! - `reference/whatsapp-business-phone-number/block-api` —
//!   `POST`/`DELETE`/`GET /{phone-number-id}/block_users`.
//! - `business-scoped-user-ids#block-users-api` — addressing a user by BSUID
//!   (`user_id`), and `user_id`/`parent_user_id` in responses.
//!
//! Users are addressed with [`Recipient`]: a phone number becomes `user`, a
//! BSUID becomes `user_id`, both send both (Meta then uses the phone
//! number). Limits enforced locally: 1–1,000 users per request, no groups,
//! no parent BSUIDs (`CC.ENT.…`; Meta documents that the request fails).
//! The 24-hour "user must have messaged you" rule and the 64,000-entry block
//! list cap are server-side.
//!
//! Partial failures: the guide shows a mixed result carrying both the
//! per-user lists and a top-level `error` (`139100`). When Meta sends that
//! with a 2xx status it is kept in [`BlockUsersResponse::error`]; with a
//! non-2xx status the request fails with that Graph error and the per-user
//! lists are not available.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use futures::Stream;
use serde::{Deserialize, Serialize};
use wa_core::error::ValidationError;
use wa_core::ids::{PhoneNumberId, UserId, WaId};
use wa_core::paging::Page;
use wa_core::recipient::Recipient;
use wa_core::{GraphApiError, Result};

use crate::request::{paginate_or_error, reject_cursors};
use crate::{Client, GraphRequest};

/// Most users one block or unblock request may carry (`block-users`,
/// "Limitations").
pub const MAX_USERS_PER_REQUEST: usize = 1000;

/// Entry point, see [`Client::block_users`].
#[derive(Debug, Clone)]
pub struct BlockUsers {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`BlockUsers`] API for `phone_number_id`.
    pub fn block_users(&self, phone_number_id: impl Into<PhoneNumberId>) -> BlockUsers {
        BlockUsers {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl BlockUsers {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Path segments; the id stays one segment whatever it contains.
    fn segments(&self) -> [&str; 2] {
        [self.phone_number_id.as_str(), "block_users"]
    }

    /// Block up to 1,000 users: `POST /{phone-number-id}/block_users`
    /// (`block-users#block-a-user`). Results are per user; see the module
    /// docs for partial failures.
    pub async fn block(&self, users: &[Recipient]) -> Result<BlockUsersResponse> {
        let body = BlockUsersBody::new(users)?;
        self.client
            .post_at(&self.segments())
            .json(&body)
            .context("block users response")
            .send()
            .await
    }

    /// Unblock up to 1,000 users: `DELETE /{phone-number-id}/block_users`
    /// with the same body as [`Self::block`] (`block-users#unblock-a-user`).
    pub async fn unblock(&self, users: &[Recipient]) -> Result<UnblockUsersResponse> {
        let body = BlockUsersBody::new(users)?;
        self.client
            .delete_at(&self.segments())
            .json(&body)
            .context("unblock users response")
            .send()
            .await
    }

    /// One page of the block list: `GET /{phone-number-id}/block_users`
    /// (`block-users#get-blocked-users`).
    pub async fn list(&self, query: &ListBlockedUsers) -> Result<Page<BlockedUser>> {
        self.list_request(query)
            .query_opt("after", query.after.as_deref())
            .query_opt("before", query.before.as_deref())
            .send()
            .await
    }

    /// The whole block list, following `paging.cursors.after`.
    ///
    /// The stream manages cursors itself, so `query.after`/`query.before`
    /// must be `None`; otherwise the stream yields that single validation
    /// error.
    pub fn list_stream(
        &self,
        query: &ListBlockedUsers,
    ) -> impl Stream<Item = Result<BlockedUser>> + Send + 'static {
        paginate_or_error(
            reject_cursors(query.after.as_deref(), query.before.as_deref())
                .map(|()| self.list_request(query)),
        )
    }

    fn list_request(&self, query: &ListBlockedUsers) -> GraphRequest {
        self.client
            .get_at(&self.segments())
            .query_opt("limit", query.limit)
            .context("list blocked users response")
    }
}

#[derive(Serialize)]
struct BlockUsersBody<'a> {
    messaging_product: &'static str,
    block_users: Vec<UserEntry<'a>>,
}

#[derive(Serialize)]
struct UserEntry<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<&'a UserId>,
}

impl<'a> BlockUsersBody<'a> {
    fn new(users: &'a [Recipient]) -> Result<Self> {
        if users.is_empty() {
            return Err(ValidationError::new("block_users", "must list at least one user").into());
        }
        if users.len() > MAX_USERS_PER_REQUEST {
            return Err(ValidationError::new(
                "block_users",
                format!(
                    "at most {MAX_USERS_PER_REQUEST} users per request, got {}",
                    users.len()
                ),
            )
            .into());
        }
        let block_users = users
            .iter()
            .enumerate()
            .map(|(i, r)| UserEntry::new(i, r))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            messaging_product: "whatsapp",
            block_users,
        })
    }
}

impl<'a> UserEntry<'a> {
    fn new(index: usize, recipient: &'a Recipient) -> Result<Self> {
        let (user, user_id) = match recipient {
            Recipient::Phone(phone) => (Some(phone.as_str()), None),
            Recipient::User(id) => (None, Some(id)),
            Recipient::PhoneAndUser { phone, user } => (Some(phone.as_str()), Some(user)),
            _ => {
                return Err(ValidationError::new(
                    format!("block_users[{index}]"),
                    "only individual users (phone number and/or BSUID) can be blocked",
                )
                .into());
            }
        };
        if user_id.is_some_and(is_parent_bsuid) {
            return Err(ValidationError::new(
                format!("block_users[{index}].user_id"),
                "parent BSUIDs are not supported for blocking or unblocking",
            )
            .into());
        }
        Ok(Self { user, user_id })
    }
}

/// Parent BSUIDs carry `ENT` between the country code and the id
/// (`business-scoped-user-ids#parent-business-scoped-user-ids`).
fn is_parent_bsuid(id: &UserId) -> bool {
    id.as_str().split('.').nth(1) == Some("ENT")
}

/// Query of [`BlockUsers::list`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListBlockedUsers {
    /// Page size.
    pub limit: Option<u32>,
    /// Cursor from a previous page's `paging.cursors.after`.
    pub after: Option<String>,
    /// Cursor from a previous page's `paging.cursors.before`.
    pub before: Option<String>,
}

/// Answer of [`BlockUsers::block`] (`BlockUsersData`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BlockUsersResponse {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// Per-user results.
    #[serde(default)]
    pub block_users: BlockUsersResult,
    /// Present on a partial failure (`139100`), next to the per-user lists.
    #[serde(default)]
    pub error: Option<GraphApiError>,
}

/// `block_users` object of a block answer.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct BlockUsersResult {
    /// Users now blocked.
    #[serde(default)]
    pub added_users: Vec<UserOutcome>,
    /// Users that could not be blocked, with the reasons.
    #[serde(default)]
    pub failed_users: Vec<FailedUser>,
}

/// Answer of [`BlockUsers::unblock`] (`UnblockUsersData`). Meta reuses the
/// `block_users` key for unblock results.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UnblockUsersResponse {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// Per-user results.
    #[serde(default)]
    pub block_users: UnblockUsersResult,
    /// Present on a partial failure (`139100`), next to the per-user lists.
    #[serde(default)]
    pub error: Option<GraphApiError>,
}

/// `block_users` object of an unblock answer.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct UnblockUsersResult {
    /// Users no longer blocked.
    #[serde(default)]
    pub removed_users: Vec<UserOutcome>,
    /// Users that could not be unblocked, with the reasons.
    #[serde(default)]
    pub failed_users: Vec<FailedUser>,
}

/// A user the operation succeeded for (`BlockedUserOperation`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct UserOutcome {
    /// What you sent: the phone number, or the BSUID if you sent only that.
    #[serde(default)]
    pub input: Option<String>,
    /// Phone number as WhatsApp knows it; omitted when you sent a BSUID.
    #[serde(default)]
    pub wa_id: Option<WaId>,
    /// BSUID; present when you sent one.
    #[serde(default)]
    pub user_id: Option<UserId>,
}

/// A user the operation failed for.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct FailedUser {
    /// What you sent.
    #[serde(default)]
    pub input: Option<String>,
    /// Phone number as WhatsApp knows it; may be absent for invalid numbers.
    #[serde(default)]
    pub wa_id: Option<WaId>,
    /// BSUID, when you sent one.
    #[serde(default)]
    pub user_id: Option<UserId>,
    /// Why, e.g. `131047` (user has not messaged in 24h), `131021` (self
    /// block). Branch on [`GraphApiError::kind`].
    #[serde(default)]
    pub errors: Vec<GraphApiError>,
}

/// An entry of the block list (`BlockedUser`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct BlockedUser {
    /// Always `whatsapp`.
    #[serde(default)]
    pub messaging_product: Option<String>,
    /// Phone number; omitted when the user has a username and Meta cannot
    /// share the number.
    #[serde(default)]
    pub wa_id: Option<WaId>,
    /// BSUID.
    #[serde(default)]
    pub user_id: Option<UserId>,
    /// Parent BSUID, if your portfolio is enrolled for them.
    #[serde(default)]
    pub parent_user_id: Option<UserId>,
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use http::Method;
    use serde_json::json;
    use wa_core::testing::ScriptedTransport;
    use wa_core::{Error, ErrorKind};

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

    fn validation_field(err: &Error) -> &str {
        match err {
            Error::Validation(v) => &v.field,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    fn two_users() -> Vec<Recipient> {
        vec![
            Recipient::phone("+16505551234"),
            Recipient::phone("+14155559876"),
        ]
    }

    #[tokio::test]
    async fn block_matches_docs_example() {
        let t = ScriptedTransport::new();
        // block-users "Block a user" example response (all blocked).
        t.push_json(
            200,
            json!({
                "messaging_product": "whatsapp",
                "block_users": {
                    "added_users": [
                        {"input": "+16505551234", "wa_id": "16505551234"},
                        {"input": "+14155559876", "wa_id": "14155559876"}
                    ]
                }
            }),
        );
        let resp = client(&t)
            .block_users("106540352242922")
            .block(&two_users())
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/106540352242922/block_users");
        assert_eq!(req.url.query(), None);
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({
                "messaging_product": "whatsapp",
                "block_users": [{"user": "+16505551234"}, {"user": "+14155559876"}]
            }))
        );
        assert_eq!(resp.block_users.added_users.len(), 2);
        assert_eq!(
            resp.block_users.added_users[1],
            UserOutcome {
                input: Some("+14155559876".into()),
                wa_id: Some(WaId::new("14155559876")),
                user_id: None,
            }
        );
        assert!(resp.block_users.failed_users.is_empty());
        assert!(resp.error.is_none());
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn block_mixed_result_keeps_per_user_errors_and_top_level_error() {
        let t = ScriptedTransport::new();
        // block-users mixed success/failure example.
        t.push_json(
            200,
            json!({
                "messaging_product": "whatsapp",
                "block_users": {
                    "added_users": [{"input": "+16505551234", "wa_id": "16505551234"}],
                    "failed_users": [{
                        "input": "+14155559876",
                        "wa_id": "14155559876",
                        "errors": [{
                            "message": "Re-engagement required",
                            "code": 131047,
                            "error_data": {"details": "User has not messaged in the last 24 hours"}
                        }]
                    }]
                },
                "error": {
                    "message": "(#139100) Failed to block/unblock users",
                    "type": "OAuthException",
                    "code": 139100,
                    "error_data": {"details": "Failed to block some users, see the block_users response list for details"},
                    "fbtrace_id": "<FBTRACE_ID>"
                }
            }),
        );
        let resp = client(&t)
            .block_users("1")
            .block(&two_users())
            .await
            .unwrap();
        let failed = &resp.block_users.failed_users[0];
        assert_eq!(failed.input.as_deref(), Some("+14155559876"));
        assert_eq!(failed.errors[0].code, 131047);
        assert_eq!(
            failed.errors[0].kind(),
            ErrorKind::CustomerServiceWindowClosed
        );
        assert_eq!(
            failed.errors[0].details(),
            Some("User has not messaged in the last 24 hours")
        );
        assert_eq!(resp.error.as_ref().map(|e| e.code), Some(139100));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn unblock_is_a_delete_with_a_body_and_reads_removed_users() {
        let t = ScriptedTransport::new();
        // block-users "Unblock a user" example response.
        t.push_json(
            200,
            json!({
                "messaging_product": "whatsapp",
                "block_users": {
                    "removed_users": [
                        {"input": "+16505551234", "wa_id": "16505551234"},
                        {"input": "+14155559876", "wa_id": "14155559876"}
                    ]
                }
            }),
        );
        let resp = client(&t)
            .block_users("106540352242922")
            .unblock(&two_users())
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::DELETE);
        assert_eq!(req.path(), "/v25.0/106540352242922/block_users");
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(
            req.json(),
            Some(json!({
                "messaging_product": "whatsapp",
                "block_users": [{"user": "+16505551234"}, {"user": "+14155559876"}]
            }))
        );
        assert_eq!(resp.block_users.removed_users.len(), 2);
        assert_eq!(
            resp.block_users.removed_users[0].wa_id,
            Some(WaId::new("16505551234"))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn unblock_mixed_result() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "messaging_product": "whatsapp",
                "block_users": {
                    "removed_users": [{"input": "+16505551234", "wa_id": "16505551234"}],
                    "failed_users": [{"input": "+14155559876", "wa_id": "14155559876", "errors": [{"message": "Re-engagement required", "code": 131047, "error_data": {"details": "User has not messaged in the last 24 hours"}}]}]
                },
                "error": {"message": "(#139100) Failed to block/unblock users", "type": "OAuthException", "code": 139100}
            }),
        );
        let resp = client(&t)
            .block_users("1")
            .unblock(&two_users())
            .await
            .unwrap();
        assert_eq!(resp.block_users.removed_users.len(), 1);
        assert_eq!(resp.block_users.failed_users[0].errors[0].code, 131047);
        assert_eq!(resp.error.map(|e| e.code), Some(139100));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn bsuid_addressing_matches_bsuid_docs() {
        let t = ScriptedTransport::new();
        // business-scoped-user-ids "Block or unblock request responses".
        t.push_json(
            200,
            json!({
                "messaging_product": "whatsapp",
                "block_users": {"added_users": [
                    {"input": "+16505551234", "wa_id": "16505551234"},
                    {"input": "US.13491208655302741918", "user_id": "US.13491208655302741918"}
                ]}
            }),
        );
        let resp = client(&t)
            .block_users("1")
            .block(&[
                Recipient::phone("+16505551234"),
                Recipient::user("US.13491208655302741918"),
                Recipient::PhoneAndUser {
                    phone: "+14155559876".into(),
                    user: "US.2".into(),
                },
            ])
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().json(),
            Some(json!({
                "messaging_product": "whatsapp",
                "block_users": [
                    {"user": "+16505551234"},
                    {"user_id": "US.13491208655302741918"},
                    {"user": "+14155559876", "user_id": "US.2"}
                ]
            }))
        );
        let by_bsuid = &resp.block_users.added_users[1];
        assert_eq!(by_bsuid.wa_id, None);
        assert_eq!(
            by_bsuid.user_id,
            Some(UserId::new("US.13491208655302741918"))
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn list_matches_docs_example() {
        let t = ScriptedTransport::new();
        // block-users "Get blocked users" example response.
        t.push_json(
            200,
            json!({
                "data": [
                    {"messaging_product": "whatsapp", "wa_id": "16505551234"},
                    {"messaging_product": "whatsapp", "wa_id": "14155559876"}
                ],
                "paging": {"cursors": {
                    "after": "eyJvZAmZAzZAXQiOjAsInZAlcnNpb25JZACI6IjE3Mzc2Nzk2ODgzODM1ODQifQZDZD",
                    "before": "eyJvZAmZAzZAXQiOjAsInZAlcnNpb25JZACI6IjE3Mzc2Nzk2ODgzODM1ODQifQZDZD"
                }}
            }),
        );
        let page = client(&t)
            .block_users("106540352242922")
            .list(&ListBlockedUsers {
                limit: Some(10),
                ..ListBlockedUsers::default()
            })
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/106540352242922/block_users");
        assert_eq!(req.url.query(), Some("limit=10"));
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(page.data.len(), 2);
        assert_eq!(page.data[0].wa_id, Some(WaId::new("16505551234")));
        assert_eq!(page.next_cursor(), None, "no `next` link: last page");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn list_parses_bsuid_entries_and_passes_cursors() {
        let t = ScriptedTransport::new();
        // business-scoped-user-ids "Get blocked users" shape.
        t.push_json(
            200,
            json!({"data": [{"messaging_product": "whatsapp", "user_id": "US.1", "parent_user_id": "US.ENT.2"}]}),
        );
        let page = client(&t)
            .block_users("1")
            .list(&ListBlockedUsers {
                limit: None,
                after: Some("A".into()),
                before: Some("B".into()),
            })
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().url.query(),
            Some("after=A&before=B")
        );
        assert_eq!(
            page.data[0],
            BlockedUser {
                messaging_product: Some("whatsapp".into()),
                wa_id: None,
                user_id: Some("US.1".into()),
                parent_user_id: Some("US.ENT.2".into()),
            }
        );
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn list_stream_follows_cursors() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"data": [{"wa_id": "1"}], "paging": {"cursors": {"after": "c1"}, "next": "https://graph.facebook.com/v25.0/1/block_users?after=c1"}}),
        );
        t.push_json(200, json!({"data": [{"wa_id": "2"}]}));
        let users: Vec<BlockedUser> = client(&t)
            .block_users("1")
            .list_stream(&ListBlockedUsers {
                limit: Some(1),
                ..ListBlockedUsers::default()
            })
            .map(Result::unwrap)
            .collect()
            .await;
        assert_eq!(users.len(), 2);
        assert_eq!(t.requests()[1].url.query(), Some("limit=1&after=c1"));
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn list_stream_refuses_caller_cursors() {
        let t = ScriptedTransport::new();
        for (q, field) in [
            (
                ListBlockedUsers {
                    after: Some("c".into()),
                    ..ListBlockedUsers::default()
                },
                "after",
            ),
            (
                ListBlockedUsers {
                    before: Some("c".into()),
                    ..ListBlockedUsers::default()
                },
                "before",
            ),
        ] {
            let items: Vec<Result<BlockedUser>> =
                client(&t).block_users("1").list_stream(&q).collect().await;
            assert_eq!(items.len(), 1);
            assert_eq!(validation_field(items[0].as_ref().unwrap_err()), field);
        }
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn self_block_error_is_classified() {
        let t = ScriptedTransport::new();
        t.push_json(
            400,
            json!({"error": {"message": "(#131021) Recipient cannot be sender", "type": "OAuthException", "code": 131021}}),
        );
        let err = client(&t)
            .block_users("1")
            .block(&[Recipient::phone("+16505551234")])
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidParameter);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn empty_and_oversized_requests_are_rejected_locally() {
        let t = ScriptedTransport::new();
        let api = client(&t).block_users("1");
        let err = api.block(&[]).await.unwrap_err();
        assert_eq!(validation_field(&err), "block_users");
        let err = api.unblock(&[]).await.unwrap_err();
        assert_eq!(validation_field(&err), "block_users");
        let too_many = vec![Recipient::phone("+16505551234"); 1001];
        let err = api.block(&too_many).await.unwrap_err();
        assert_eq!(validation_field(&err), "block_users");
        let err = api.unblock(&too_many).await.unwrap_err();
        assert_eq!(validation_field(&err), "block_users");
        assert!(t.requests().is_empty());

        // Exactly 1,000 is allowed.
        t.push_json(
            200,
            json!({"messaging_product": "whatsapp", "block_users": {}}),
        );
        let max = vec![Recipient::phone("+16505551234"); 1000];
        api.block(&max).await.unwrap();
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn groups_cannot_be_blocked() {
        let t = ScriptedTransport::new();
        let err = client(&t)
            .block_users("1")
            .block(&[Recipient::phone("+1"), Recipient::group("Y2FwaV9ncm91cDox")])
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "block_users[1]");
        assert!(t.requests().is_empty());
    }

    #[tokio::test]
    async fn parent_bsuids_are_rejected() {
        let t = ScriptedTransport::new();
        let api = client(&t).block_users("1");
        let err = api
            .block(&[Recipient::user("US.ENT.11815799212886844830")])
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "block_users[0].user_id");
        let err = api
            .unblock(&[
                Recipient::phone("+1"),
                Recipient::PhoneAndUser {
                    phone: "+1".into(),
                    user: "US.ENT.1".into(),
                },
            ])
            .await
            .unwrap_err();
        assert_eq!(validation_field(&err), "block_users[1].user_id");
        assert!(t.requests().is_empty());
        assert!(!is_parent_bsuid(&UserId::new("US.13491208655302741918")));
    }
}
