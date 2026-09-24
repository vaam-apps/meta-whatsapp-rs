//! Block, unblock and list blocked users.
//!
//! Docs: `block-users`, `reference/whatsapp-business-phone-number/block-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use serde::{Deserialize, Serialize};
use wa_core::Result;
use wa_core::ids::{PhoneNumberId, WaId};
use wa_core::paging::Page;

use crate::Client;

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

    /// Block one or more WhatsApp users (max 1,000 per request).
    pub async fn block(&self, users: &[&str]) -> Result<BlockResponse> {
        let users_to_block = users
            .iter()
            .map(|u| BlockUserInput {
                user: u.to_string(),
            })
            .collect();

        let req = BlockUsersRequest {
            messaging_product: "whatsapp".to_string(),
            block_users: users_to_block,
        };

        self.client
            .post(&format!("{}/block_users", self.phone_number_id))
            .json(&req)
            .context("block users response")
            .send::<BlockResponse>()
            .await
    }

    /// Unblock one or more WhatsApp users.
    pub async fn unblock(&self, users: &[&str]) -> Result<UnblockResponse> {
        let users_to_unblock = users
            .iter()
            .map(|u| UnblockUserInput {
                user: u.to_string(),
            })
            .collect();

        let req = UnblockUsersRequest {
            messaging_product: "whatsapp".to_string(),
            unblock_users: users_to_unblock,
        };

        self.client
            .post(&format!("{}/unblock_users", self.phone_number_id))
            .json(&req)
            .context("unblock users response")
            .send::<UnblockResponse>()
            .await
    }

    /// List all blocked users.
    pub async fn list_blocked(&self) -> Result<Page<BlockedUser>> {
        self.client
            .get(&format!("{}/blocked_contacts", self.phone_number_id))
            .context("list blocked users response")
            .send::<Page<BlockedUser>>()
            .await
    }

    /// Stream all blocked users.
    pub fn list_blocked_stream(
        &self,
    ) -> Result<impl futures::Stream<Item = Result<BlockedUser>>> {
        Ok(self
            .client
            .get(&format!("{}/blocked_contacts", self.phone_number_id))
            .paginate::<BlockedUser>())
    }
}

/// Request to block users.
#[derive(Debug, Clone, Serialize)]
pub struct BlockUsersRequest {
    /// Messaging product identifier.
    pub messaging_product: String,
    /// Array of users to block.
    pub block_users: Vec<BlockUserInput>,
}

/// Single user to block.
#[derive(Debug, Clone, Serialize)]
pub struct BlockUserInput {
    /// Phone number or WhatsApp ID to block.
    pub user: String,
}

/// Response from blocking users.
#[derive(Debug, Clone, Deserialize)]
pub struct BlockResponse {
    /// Messaging product identifier.
    pub messaging_product: String,
    /// Block results.
    pub block_users: BlockResult,
}

/// Result of block operation.
#[derive(Debug, Clone, Deserialize)]
pub struct BlockResult {
    /// Successfully blocked users.
    #[serde(default)]
    pub added_users: Vec<BlockedUserResult>,
    /// Users that failed to block.
    #[serde(default)]
    pub failed_users: Vec<FailedBlockUser>,
}

/// Blocked user result.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BlockedUserResult {
    /// Input identifier (phone number or wa_id).
    pub input: String,
    /// WhatsApp ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
}

/// Failed block user.
#[derive(Debug, Clone, Deserialize)]
pub struct FailedBlockUser {
    /// Input identifier.
    pub input: String,
    /// WhatsApp ID (may be absent for invalid numbers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wa_id: Option<WaId>,
    /// Error details.
    pub errors: Vec<BlockError>,
}

/// Block operation error.
#[derive(Debug, Clone, Deserialize)]
pub struct BlockError {
    /// Error message.
    pub message: String,
    /// Error code.
    pub code: u32,
    /// Error data with details.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_data: Option<ErrorData>,
}

/// Error details.
#[derive(Debug, Clone, Deserialize)]
pub struct ErrorData {
    /// Details about the error.
    pub details: String,
}

/// Request to unblock users.
#[derive(Debug, Clone, Serialize)]
pub struct UnblockUsersRequest {
    /// Messaging product identifier.
    pub messaging_product: String,
    /// Array of users to unblock.
    pub unblock_users: Vec<UnblockUserInput>,
}

/// Single user to unblock.
#[derive(Debug, Clone, Serialize)]
pub struct UnblockUserInput {
    /// Phone number or WhatsApp ID to unblock.
    pub user: String,
}

/// Response from unblocking users.
#[derive(Debug, Clone, Deserialize)]
pub struct UnblockResponse {
    /// Messaging product identifier.
    pub messaging_product: String,
    /// Unblock results.
    pub unblock_users: UnblockResult,
}

/// Result of unblock operation.
#[derive(Debug, Clone, Deserialize)]
pub struct UnblockResult {
    /// Successfully unblocked users.
    #[serde(default)]
    pub added_users: Vec<BlockedUserResult>,
    /// Users that failed to unblock.
    #[serde(default)]
    pub failed_users: Vec<FailedBlockUser>,
}

/// A blocked user in the blocklist.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlockedUser {
    /// WhatsApp ID of blocked user.
    pub wa_id: WaId,
    /// Phone number of blocked user (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone_number: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Client, RetryPolicy};
    use http::Method;
    use serde_json::json;
    use wa_core::testing::ScriptedTransport;

    #[tokio::test]
    async fn block_users_success() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "messaging_product": "whatsapp",
                "block_users": {
                    "added_users": [
                        {
                            "input": "+16505551234",
                            "wa_id": "16505551234"
                        }
                    ]
                }
            }),
        );
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let response = client
            .block_users("123")
            .block(&["+16505551234"])
            .await
            .unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/123/block_users");
        assert_eq!(
            req.json(),
            Some(json!({
                "messaging_product": "whatsapp",
                "block_users": [
                    {"user": "+16505551234"}
                ]
            }))
        );

        assert_eq!(response.block_users.added_users.len(), 1);
        assert_eq!(response.block_users.added_users[0].input, "+16505551234");
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn unblock_users() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "messaging_product": "whatsapp",
                "unblock_users": {
                    "added_users": [
                        {
                            "input": "+16505551234",
                            "wa_id": "16505551234"
                        }
                    ]
                }
            }),
        );
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let response = client
            .block_users("123")
            .unblock(&["+16505551234"])
            .await
            .unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::POST);
        assert_eq!(req.path(), "/v25.0/123/unblock_users");
        assert_eq!(response.unblock_users.added_users.len(), 1);
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn list_blocked() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "data": [
                    {
                        "wa_id": "16505551234",
                        "phone_number": "+16505551234"
                    }
                ],
                "paging": {}
            }),
        );
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let response = client.block_users("123").list_blocked().await.unwrap();

        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/123/blocked_contacts");

        assert_eq!(response.data.len(), 1);
        assert_eq!(response.data[0].wa_id, WaId::new("16505551234"));
        assert_eq!(t.remaining(), 0);
    }
}
