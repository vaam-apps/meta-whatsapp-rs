//! Groups API: create groups, invite links, participants, join requests.
//!
//! Docs: `groups/*`, `reference/groups/*`, `reference/whatsapp-business-phone-number/groups-management-api`.
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).

use serde::{Deserialize, Serialize};
use wa_core::Result;
use wa_core::ids::{GroupId, PhoneNumberId, WaId};
use wa_core::paging::Page;

use crate::Client;

/// Entry point, see [`Client::groups`].
#[derive(Debug, Clone)]
pub struct Groups {
    client: Client,
    phone_number_id: PhoneNumberId,
}

impl Client {
    /// [`Groups`] API for `phone_number_id`.
    pub fn groups(&self, phone_number_id: impl Into<PhoneNumberId>) -> Groups {
        Groups {
            client: self.clone(),
            phone_number_id: phone_number_id.into(),
        }
    }
}

impl Groups {
    /// The id this API is scoped to.
    pub fn id(&self) -> &PhoneNumberId {
        &self.phone_number_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Create a new group.
    pub async fn create(&self, req: &CreateGroupRequest) -> Result<CreateGroupResponse> {
        self.client
            .post(&format!("{}/groups", self.phone_number_id))
            .json(req)
            .context("create group response")
            .send::<CreateGroupResponse>()
            .await
    }

    /// Get information about a group.
    pub async fn get_info(&self, group_id: &GroupId) -> Result<GroupInfo> {
        self.client
            .get(&format!("{}", group_id))
            .context("get group info response")
            .send::<GroupInfo>()
            .await
    }

    /// Update group settings (subject and description).
    pub async fn update(
        &self,
        group_id: &GroupId,
        req: &UpdateGroupRequest,
    ) -> Result<UpdateGroupResponse> {
        self.client
            .post(&format!("{}", group_id))
            .json(req)
            .context("update group response")
            .send::<UpdateGroupResponse>()
            .await
    }

    /// Delete/archive a group.
    pub async fn delete(&self, group_id: &GroupId) -> Result<()> {
        self.client
            .delete(&format!("{}", group_id))
            .context("delete group response")
            .send_success()
            .await
    }

    /// Get the invite link for a group.
    pub async fn get_invite_link(&self, group_id: &GroupId) -> Result<InviteLinkResponse> {
        self.client
            .get(&format!("{}/invite_link", group_id))
            .context("get invite link response")
            .send::<InviteLinkResponse>()
            .await
    }

    /// Reset/regenerate the invite link for a group.
    pub async fn reset_invite_link(&self, group_id: &GroupId) -> Result<InviteLinkResponse> {
        let req = ResetInviteLinkRequest {
            messaging_product: "whatsapp".to_string(),
        };
        self.client
            .post(&format!("{}/invite_link", group_id))
            .json(&req)
            .context("reset invite link response")
            .send::<InviteLinkResponse>()
            .await
    }

    /// Get list of join requests for a group.
    pub async fn list_join_requests(&self, group_id: &GroupId) -> Result<Page<JoinRequest>> {
        self.client
            .get(&format!("{}/join_requests", group_id))
            .context("list join requests response")
            .send::<Page<JoinRequest>>()
            .await
    }

    /// Approve one or more join requests.
    pub async fn approve_join_requests(
        &self,
        group_id: &GroupId,
        request_ids: &[&str],
    ) -> Result<ApproveJoinRequestsResponse> {
        let req_ids = request_ids.iter().map(|s| s.to_string()).collect();
        let req = ApproveJoinRequestsRequest {
            messaging_product: "whatsapp".to_string(),
            join_requests: req_ids,
        };
        self.client
            .post(&format!("{}/join_requests", group_id))
            .json(&req)
            .context("approve join requests response")
            .send::<ApproveJoinRequestsResponse>()
            .await
    }

    /// Reject one or more join requests.
    pub async fn reject_join_requests(
        &self,
        group_id: &GroupId,
        request_ids: &[&str],
    ) -> Result<RejectJoinRequestsResponse> {
        let req_ids = request_ids.iter().map(|s| s.to_string()).collect();
        let req = RejectJoinRequestsRequest {
            messaging_product: "whatsapp".to_string(),
            join_requests: req_ids,
        };
        self.client
            .delete(&format!("{}/join_requests", group_id))
            .json(&req)
            .context("reject join requests response")
            .send::<RejectJoinRequestsResponse>()
            .await
    }
}

/// Request to create a group.
#[derive(Debug, Clone, Serialize)]
pub struct CreateGroupRequest {
    /// Messaging product.
    pub messaging_product: String,
    /// Group subject (max 128 characters).
    pub subject: String,
    /// Group description (max 2048 characters, optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Join approval mode: "auto_approve" or "approval_required".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_approval_mode: Option<String>,
}

/// Response when creating a group.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateGroupResponse {
    /// Group ID.
    pub group_id: GroupId,
}

/// Request to reset invite link.
#[derive(Debug, Clone, Serialize)]
pub struct ResetInviteLinkRequest {
    /// Messaging product.
    pub messaging_product: String,
}

/// Group information.
#[derive(Debug, Clone, Deserialize)]
pub struct GroupInfo {
    /// Group ID.
    pub id: GroupId,
    /// Group subject.
    pub subject: String,
    /// Group description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Group icon URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Group participants.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<GroupParticipant>>,
}

/// Group participant.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GroupParticipant {
    /// Participant's WhatsApp ID.
    pub wa_id: WaId,
}

/// Request to update a group.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateGroupRequest {
    /// Messaging product.
    pub messaging_product: String,
    /// New group subject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// New group description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Response from updating a group.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateGroupResponse {
    /// Whether the update was successful.
    pub success: bool,
}

/// Invite link response.
#[derive(Debug, Clone, Deserialize)]
pub struct InviteLinkResponse {
    /// Messaging product.
    pub messaging_product: String,
    /// The invite link.
    pub invite_link: String,
}

/// Join request.
#[derive(Debug, Clone, Deserialize)]
pub struct JoinRequest {
    /// Join request ID.
    pub join_request_id: String,
    /// WhatsApp user ID.
    pub wa_id: WaId,
    /// Timestamp when join request was created.
    pub creation_timestamp: i64,
}

/// Request to approve join requests.
#[derive(Debug, Clone, Serialize)]
pub struct ApproveJoinRequestsRequest {
    /// Messaging product.
    pub messaging_product: String,
    /// Join request IDs to approve.
    pub join_requests: Vec<String>,
}

/// Response from approving join requests.
#[derive(Debug, Clone, Deserialize)]
pub struct ApproveJoinRequestsResponse {
    /// Messaging product.
    pub messaging_product: String,
    /// Approved join request IDs.
    pub approved_join_requests: Vec<String>,
    /// Failed join requests.
    #[serde(default)]
    pub failed_join_requests: Vec<FailedJoinRequest>,
}

/// Failed join request.
#[derive(Debug, Clone, Deserialize)]
pub struct FailedJoinRequest {
    /// Join request ID.
    pub join_request_id: String,
    /// Errors.
    pub errors: Vec<JoinRequestError>,
}

/// Join request error.
#[derive(Debug, Clone, Deserialize)]
pub struct JoinRequestError {
    /// Error code.
    pub code: u32,
    /// Error message.
    pub message: String,
}

/// Request to reject join requests.
#[derive(Debug, Clone, Serialize)]
pub struct RejectJoinRequestsRequest {
    /// Messaging product.
    pub messaging_product: String,
    /// Join request IDs to reject.
    pub join_requests: Vec<String>,
}

/// Response from rejecting join requests.
#[derive(Debug, Clone, Deserialize)]
pub struct RejectJoinRequestsResponse {
    /// Messaging product.
    pub messaging_product: String,
    /// Rejected join request IDs.
    pub rejected_join_requests: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Client, RetryPolicy};
    use http::Method;
    use serde_json::json;
    use wa_core::testing::ScriptedTransport;

    #[tokio::test]
    async fn create_group() {
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({"group_id": "Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD"}),
        );
        let client = Client::builder()
            .transport(t.clone())
            .access_token("TOKEN")
            .retry(RetryPolicy::NONE)
            .build()
            .unwrap();

        let req = CreateGroupRequest {
            messaging_product: "whatsapp".to_string(),
            subject: "Test Group".to_string(),
            description: Some("A test group".to_string()),
            join_approval_mode: Some("auto_approve".to_string()),
        };

        let response = client.groups("123").create(&req).await.unwrap();

        let http_req = t.last_request().unwrap();
        assert_eq!(http_req.method, Method::POST);
        assert_eq!(http_req.path(), "/v25.0/123/groups");

        assert_eq!(
            response.group_id,
            GroupId::new("Y2FwaV9ncm91cDoxNzA1NTU1MDEzOToxMjAzNjM0MDQ2OTQyMzM4MjAZD")
        );
        assert_eq!(t.remaining(), 0);
    }
}
