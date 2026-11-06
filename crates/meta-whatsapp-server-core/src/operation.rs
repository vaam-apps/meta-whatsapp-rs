//! The tenant operations, by id (docs/design/server.md, section 8.1;
//! roadmap S6): what an API adapter serves, named the same whatever its
//! framework, so that what the core decides about a request follows the
//! operation and not how the adapter spells it. A request's rate class is
//! its operation's ([`crate::ratelimit::RouteClass::of`], charged with
//! [`crate::ratelimit::RateLimiter::charge`]), never its HTTP method's;
//! an `Idempotency-Key`'s fingerprint is over its operation and its
//! canonical input ([`crate::idempotency::Fingerprint`]), never the
//! request's path.
//!
//! An id is the operation's `operationId` in the HTTP adapter's OpenAPI
//! document (`send_message`), the name of its handler; the HTTP adapter
//! maps a request (its method and route template) to one, and its tests
//! check that every tenant operation of its document has one here, with
//! the scope its routes require. The admin routes (behind an admin key,
//! with no rate limit and no idempotency key) and the operations routes
//! (`/livez`, `/metrics`, no key) are not tenant operations.

use crate::model::Scope;

/// A tenant operation: what a tenant (or a platform key acting as one)
/// asks the service, by id ([`Self::id`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    /// `list_wabas`: the tenant's WABAs.
    ListWabas,
    /// `list_numbers`: the tenant's numbers.
    ListNumbers,
    /// `get_number`: a number's live details.
    GetNumber,
    /// `get_profile`: a number's business profile.
    GetProfile,
    /// `update_profile`: a number's business profile changed (since roadmap M5c2, its picture too).
    UpdateProfile,
    /// `request_display_name_change`: a new display name asked of Meta, which reviews it (roadmap M5c2; pending the owner's confirmation before the first release).
    RequestDisplayNameChange,
    /// `get_display_name`: a number's display name and the review of a requested one (roadmap M5c2; pending the owner's confirmation before the first release).
    GetDisplayName,
    /// `get_number_settings`: a number's settings: local storage and payload encryption's status, never a key (roadmap M5c3; pending the owner's confirmation before the first release).
    GetNumberSettings,
    /// `update_number_settings`: one of a number's settings changed (roadmap M5c3; pending the owner's confirmation before the first release).
    UpdateNumberSettings,
    /// `enable_payload_encryption`: payload encryption turned on with the business's public key (roadmap M5c3; pending the owner's confirmation before the first release).
    EnablePayloadEncryption,
    /// `disable_payload_encryption`: payload encryption turned off (roadmap M5c3; pending the owner's confirmation before the first release).
    DisablePayloadEncryption,
    /// `get_username`: a number's business username (roadmap M5c3; pending the owner's confirmation before the first release).
    GetUsername,
    /// `set_username`: a business username adopted or changed (roadmap M5c3; pending the owner's confirmation before the first release).
    SetUsername,
    /// `delete_username`: a business username removed (roadmap M5c3; pending the owner's confirmation before the first release).
    DeleteUsername,
    /// `list_username_suggestions`: the usernames WhatsApp reserved for the portfolio (roadmap M5c3; pending the owner's confirmation before the first release).
    ListUsernameSuggestions,
    /// `get_official_business_account`: a number's Official Business Account status (roadmap M5c3; pending the owner's confirmation before the first release).
    GetOfficialBusinessAccount,
    /// `request_official_business_account`: an application for Official Business Account status (not a legal act; audited) (roadmap M5c3; pending the owner's confirmation before the first release).
    RequestOfficialBusinessAccount,
    /// `get_compliance_info`: a number's business compliance information (India) (roadmap M5c3; pending the owner's confirmation before the first release).
    GetComplianceInfo,
    /// `set_compliance_info`: a number's business compliance information declared (a legal act: off until the deployment enables `WA_SERVER_LEGAL_ACTS`) (roadmap M5c3; pending the owner's confirmation before the first release).
    SetComplianceInfo,
    /// `get_health_status`: a number's health status (roadmap M5c3; pending the owner's confirmation before the first release).
    GetHealthStatus,
    /// `get_conversational_components`: a number's welcome message flag, ice breakers and commands (roadmap M5c3; pending the owner's confirmation before the first release).
    GetConversationalComponents,
    /// `set_conversational_components`: a number's welcome message flag, ice breakers and commands set (roadmap M5c3; pending the owner's confirmation before the first release).
    SetConversationalComponents,
    /// `delete_contact_book_entry`: one user's contact book entry erased at Meta (irreversible, an explicit erasure under the scope `erasure`, not gated by `WA_SERVER_LEGAL_ACTS`) (roadmap M5c3; pending the owner's confirmation before the first release).
    DeleteContactBookEntry,
    /// `disconnect_waba`: a WABA disconnected (unsubscribed, its token
    /// and bindings deleted).
    DisconnectWaba,
    /// `send_message`: a message sent.
    SendMessage,
    /// `mark_read`: a received message marked read.
    MarkRead,
    /// `upload_media`: a file uploaded.
    UploadMedia,
    /// `download_media`: a media file downloaded.
    DownloadMedia,
    /// `start_media_upload`: a resumable upload session opened (roadmap
    /// M5c1; pending the owner's confirmation before the first release).
    StartMediaUpload,
    /// `upload_media_chunk`: a chunk of a resumable upload sent (M5c1).
    UploadMediaChunk,
    /// `get_media_upload`: a resumable upload's progress (M5c1).
    GetMediaUpload,
    /// `delete_media`: a media file deleted.
    DeleteMedia,
    /// `list_templates`: a WABA's templates.
    ListTemplates,
    /// `create_template`: a template created.
    CreateTemplate,
    /// `delete_templates`: templates deleted, by name or id.
    DeleteTemplates,
    /// `get_template`: a template, by id.
    GetTemplate,
    /// `list_events`: the tenant's events, polled.
    ListEvents,
    /// `get_event`: one of the tenant's events, by id (roadmap M2b).
    GetEvent,
    /// `stream_events`: the tenant's events, live (Server-Sent Events,
    /// roadmap M2b).
    StreamEvents,
    /// `stream_number_events`: one of the tenant's numbers' events, live.
    StreamNumberEvents,
    /// `list_conversations`: a number's inbox, newest activity first.
    ListConversations,
    /// `list_conversation_messages`: one conversation's history, newest
    /// first.
    ListConversationMessages,
    /// `reply_to_conversation`: a reply to a conversation's contact,
    /// checked against the window and the thread's owner first.
    ReplyToConversation,
    /// `get_conversation_window`: a conversation's 24-hour window, as a
    /// reply would meet it (roadmap M2g).
    GetConversationWindow,
    /// `mark_conversation_read`: a conversation's unread count reset, and
    /// on request its latest inbound message marked read on Meta (roadmap
    /// M2g).
    MarkConversationRead,
    /// `get_inbox_message`: a message of a number's inbox, by id (roadmap
    /// M2g).
    GetInboxMessage,
    /// `create_authentication_template`: an authentication template
    /// created or updated in one or more languages (roadmap M3c).
    CreateAuthenticationTemplate,
    /// `issue_otp`: a one-time passcode sent with the tenant's
    /// authentication template (roadmap M3c).
    IssueOtp,
    /// `verify_otp`: a typed code checked, which always counts as an
    /// attempt (roadmap M3c).
    VerifyOtp,
    /// `request_verification_code`: a verification code sent to a number
    /// by SMS or voice call (roadmap M3d).
    RequestVerificationCode,
    /// `verify_code`: a number's verification code checked (roadmap M3d).
    VerifyCode,
    /// `register_number`: a number registered with its two-step
    /// verification PIN (roadmap M3d).
    RegisterNumber,
    /// `deregister_number`: a number deregistered (roadmap M3d).
    DeregisterNumber,
    /// `set_two_step_pin`: a number's two-step verification PIN set
    /// (roadmap M3d).
    SetTwoStepPin,
    /// `list_payment_configurations`: a WABA's payment configurations
    /// (roadmap P2).
    ListPaymentConfigurations,
    /// `create_payment_configuration`: a payment configuration created in
    /// a WABA.
    CreatePaymentConfiguration,
    /// `get_payment_configuration`: one payment configuration of a WABA,
    /// by name.
    GetPaymentConfiguration,
    /// `delete_payment_configuration`: a payment configuration removed.
    DeletePaymentConfiguration,
    /// `regenerate_payment_oauth_link`: a new OAuth link for a payment
    /// gateway configuration.
    RegeneratePaymentOauthLink,
    /// `get_payment`: the payment of an order, looked up by its
    /// configuration and reference id.
    GetPayment,
    /// `refund_payment`: a refund of an order paid through a payment
    /// gateway.
    RefundPayment,
    /// `start_signup`: an Embedded Signup attempt bound to the tenant, and
    /// what the page's `FB.login` needs (roadmap M3a).
    StartSignup,
    /// `complete_signup`: the attempt redeemed for the tenant, then the
    /// business onboarded (roadmap M3a).
    CompleteSignup,
    /// `list_signup_attempts`: the tenant's resumable attempts (roadmap
    /// M3a).
    ListSignupAttempts,
    /// `resume_signup`: an attempt's steps after the stored token run
    /// again (roadmap M3a).
    ResumeSignup,
    /// `erase_customer`: a customer erased on a number, from the inbox and
    /// the outbox, journaled and audited (roadmap M2f).
    EraseCustomer,
    /// `create_webhook_endpoint`: a webhook endpoint created, its secret
    /// shown once (roadmap M2c).
    CreateWebhookEndpoint,
    /// `list_webhook_endpoints`: the tenant's webhook endpoints.
    ListWebhookEndpoints,
    /// `get_webhook_endpoint`: one webhook endpoint.
    GetWebhookEndpoint,
    /// `update_webhook_endpoint`: a webhook endpoint's URL, types or
    /// `enabled` changed.
    UpdateWebhookEndpoint,
    /// `delete_webhook_endpoint`: a webhook endpoint deleted.
    DeleteWebhookEndpoint,
    /// `rotate_webhook_secret`: a webhook endpoint's new secret.
    RotateWebhookSecret,
    /// `test_webhook_endpoint`: a ping to a webhook endpoint.
    TestWebhookEndpoint,
    /// `list_webhook_deliveries`: a webhook endpoint's deliveries.
    ListWebhookDeliveries,
    /// `retry_webhook_delivery`: a webhook delivery made again.
    RetryWebhookDelivery,
    /// `edit_template`: a template edited: its category, components or time to live (roadmap M5b).
    EditTemplate,
    /// `list_library_templates`: the Template Library's entries (roadmap M5b).
    ListLibraryTemplates,
    /// `create_library_template`: a template created from a Template Library entry (roadmap M5b).
    CreateLibraryTemplate,
    /// `migrate_templates`: templates recreated from another of the tenant's WABAs (roadmap M5b).
    MigrateTemplates,
    /// `compare_templates`: two templates of a WABA compared (roadmap M5b).
    CompareTemplates,
    /// `unpause_template`: a paused template unpaused (roadmap M5b).
    UnpauseTemplate,
    /// `archive_templates`: templates archived (roadmap M5b).
    ArchiveTemplates,
    /// `unarchive_templates`: archived templates unarchived (roadmap M5b).
    UnarchiveTemplates,
    /// `create_template_group`: a template group created (roadmap M5b).
    CreateTemplateGroup,
    /// `get_template_group`: a template group, by id (roadmap M5b).
    GetTemplateGroup,
    /// `update_template_group`: a template group renamed, described, or its templates changed (roadmap M5b).
    UpdateTemplateGroup,
    /// `delete_template_group`: a template group deleted, its templates kept (roadmap M5b).
    DeleteTemplateGroup,
    /// `create_group`: a group asked of Meta, its id coming in a webhook (roadmap M5f; pending the owner's confirmation before the first release).
    CreateGroup,
    /// `list_groups`: a number's groups, which the service records as the number's (roadmap M5f; pending the owner's confirmation before the first release).
    ListGroups,
    /// `get_group`: a group of the number (roadmap M5f; pending the owner's confirmation before the first release).
    GetGroup,
    /// `update_group`: a group's subject and description changed (roadmap M5f; pending the owner's confirmation before the first release).
    UpdateGroup,
    /// `delete_group`: a group deleted, its participants removed (roadmap M5f; pending the owner's confirmation before the first release).
    DeleteGroup,
    /// `get_group_invite_link`: a group's invite link (roadmap M5f; pending the owner's confirmation before the first release).
    GetGroupInviteLink,
    /// `reset_group_invite_link`: a group's invite link replaced (roadmap M5f; pending the owner's confirmation before the first release).
    ResetGroupInviteLink,
    /// `list_group_participants`: a group's participants (roadmap M5f; pending the owner's confirmation before the first release).
    ListGroupParticipants,
    /// `remove_group_participants`: participants removed from a group (roadmap M5f; pending the owner's confirmation before the first release).
    RemoveGroupParticipants,
    /// `list_group_join_requests`: a group's open join requests (roadmap M5f; pending the owner's confirmation before the first release).
    ListGroupJoinRequests,
    /// `approve_group_join_requests`: join requests approved (roadmap M5f; pending the owner's confirmation before the first release).
    ApproveGroupJoinRequests,
    /// `reject_group_join_requests`: join requests rejected (roadmap M5f; pending the owner's confirmation before the first release).
    RejectGroupJoinRequests,
    /// `get_group_analytics`: a group's analytics, day by day (roadmap M5f; pending the owner's confirmation before the first release).
    GetGroupAnalytics,
    /// `create_flow`: a Flow created in a WABA (roadmap M5d).
    CreateFlow,
    /// `list_flows`: a WABA's Flows (roadmap M5d).
    ListFlows,
    /// `get_flow`: a Flow, by id (roadmap M5d).
    GetFlow,
    /// `update_flow`: a Flow's name, categories or endpoint changed (roadmap M5d).
    UpdateFlow,
    /// `upload_flow_json`: a Flow's Flow JSON replaced (roadmap M5d).
    UploadFlowJson,
    /// `preview_flow`: a Flow's web preview link (roadmap M5d).
    PreviewFlow,
    /// `publish_flow`: a Flow published (roadmap M5d).
    PublishFlow,
    /// `deprecate_flow`: a published Flow deprecated (roadmap M5d).
    DeprecateFlow,
    /// `delete_flow`: a draft Flow deleted (roadmap M5d).
    DeleteFlow,
    /// `get_commerce_settings`: a number's commerce settings (roadmap M5g).
    GetCommerceSettings,
    /// `update_commerce_settings`: a number's commerce settings changed: the cart enabled or not, the catalog shown or not (roadmap M5g).
    UpdateCommerceSettings,
    /// `list_catalogs`: the catalog a WABA connects (roadmap M5g).
    ListCatalogs,
    /// `get_catalog`: a catalog of a WABA, by id (roadmap M5g).
    GetCatalog,
    /// `list_catalog_products`: a page of a WABA's catalog's products (roadmap M5g).
    ListCatalogProducts,
    /// `create_qr_code`: a QR code and short link created (roadmap M5g).
    CreateQrCode,
    /// `list_qr_codes`: a number's QR codes (roadmap M5g).
    ListQrCodes,
    /// `get_qr_code`: one of a number's QR codes (roadmap M5g).
    GetQrCode,
    /// `update_qr_code`: a QR code's prefilled message changed (roadmap M5g).
    UpdateQrCode,
    /// `delete_qr_code`: a QR code deleted (roadmap M5g).
    DeleteQrCode,
    /// `get_messaging_analytics`: a WABA's messaging analytics (roadmap M5g).
    GetMessagingAnalytics,
    /// `get_conversation_analytics`: a WABA's conversation analytics (roadmap M5g).
    GetConversationAnalytics,
    /// `get_pricing_analytics`: a WABA's pricing analytics (roadmap M5g).
    GetPricingAnalytics,
    /// `get_template_analytics`: analytics of templates of a WABA (roadmap M5g).
    GetTemplateAnalytics,
    /// `list_blocked_users`: a number's block list (roadmap M5g).
    ListBlockedUsers,
    /// `block_users`: people blocked on a number (roadmap M5g).
    BlockUsers,
    /// `unblock_users`: people unblocked on a number (roadmap M5g).
    UnblockUsers,
    /// `get_routing_roles`: the routing roles a number's business holds (roadmap M5l).
    GetRoutingRoles,
    /// `pass_thread`: a user's thread passed to another responder (roadmap M5l).
    PassThread,
    /// `take_thread`: a user's thread taken by the escalation partner (roadmap M5l).
    TakeThread,
    /// `release_thread`: a user's thread released to idle (roadmap M5l).
    ReleaseThread,
}

impl Operation {
    /// Every tenant operation.
    pub const ALL: [Self; 130] = [
        Self::ListWabas,
        Self::ListNumbers,
        Self::GetNumber,
        Self::GetProfile,
        Self::UpdateProfile,
        Self::RequestDisplayNameChange,
        Self::GetDisplayName,
        Self::GetNumberSettings,
        Self::UpdateNumberSettings,
        Self::EnablePayloadEncryption,
        Self::DisablePayloadEncryption,
        Self::GetUsername,
        Self::SetUsername,
        Self::DeleteUsername,
        Self::ListUsernameSuggestions,
        Self::GetOfficialBusinessAccount,
        Self::RequestOfficialBusinessAccount,
        Self::GetComplianceInfo,
        Self::SetComplianceInfo,
        Self::GetHealthStatus,
        Self::GetConversationalComponents,
        Self::SetConversationalComponents,
        Self::DeleteContactBookEntry,
        Self::DisconnectWaba,
        Self::SendMessage,
        Self::MarkRead,
        Self::UploadMedia,
        Self::DownloadMedia,
        Self::StartMediaUpload,
        Self::UploadMediaChunk,
        Self::GetMediaUpload,
        Self::DeleteMedia,
        Self::ListTemplates,
        Self::CreateTemplate,
        Self::DeleteTemplates,
        Self::GetTemplate,
        Self::ListEvents,
        Self::GetEvent,
        Self::StreamEvents,
        Self::StreamNumberEvents,
        Self::ListConversations,
        Self::ListConversationMessages,
        Self::ReplyToConversation,
        Self::GetConversationWindow,
        Self::MarkConversationRead,
        Self::GetInboxMessage,
        Self::CreateAuthenticationTemplate,
        Self::IssueOtp,
        Self::VerifyOtp,
        Self::RequestVerificationCode,
        Self::VerifyCode,
        Self::RegisterNumber,
        Self::DeregisterNumber,
        Self::SetTwoStepPin,
        Self::ListPaymentConfigurations,
        Self::CreatePaymentConfiguration,
        Self::GetPaymentConfiguration,
        Self::DeletePaymentConfiguration,
        Self::RegeneratePaymentOauthLink,
        Self::GetPayment,
        Self::RefundPayment,
        Self::StartSignup,
        Self::CompleteSignup,
        Self::ListSignupAttempts,
        Self::ResumeSignup,
        Self::EraseCustomer,
        Self::CreateWebhookEndpoint,
        Self::ListWebhookEndpoints,
        Self::GetWebhookEndpoint,
        Self::UpdateWebhookEndpoint,
        Self::DeleteWebhookEndpoint,
        Self::RotateWebhookSecret,
        Self::TestWebhookEndpoint,
        Self::ListWebhookDeliveries,
        Self::RetryWebhookDelivery,
        Self::EditTemplate,
        Self::ListLibraryTemplates,
        Self::CreateLibraryTemplate,
        Self::MigrateTemplates,
        Self::CompareTemplates,
        Self::UnpauseTemplate,
        Self::ArchiveTemplates,
        Self::UnarchiveTemplates,
        Self::CreateTemplateGroup,
        Self::GetTemplateGroup,
        Self::UpdateTemplateGroup,
        Self::DeleteTemplateGroup,
        Self::CreateGroup,
        Self::ListGroups,
        Self::GetGroup,
        Self::UpdateGroup,
        Self::DeleteGroup,
        Self::GetGroupInviteLink,
        Self::ResetGroupInviteLink,
        Self::ListGroupParticipants,
        Self::RemoveGroupParticipants,
        Self::ListGroupJoinRequests,
        Self::ApproveGroupJoinRequests,
        Self::RejectGroupJoinRequests,
        Self::GetGroupAnalytics,
        Self::CreateFlow,
        Self::ListFlows,
        Self::GetFlow,
        Self::UpdateFlow,
        Self::UploadFlowJson,
        Self::PreviewFlow,
        Self::PublishFlow,
        Self::DeprecateFlow,
        Self::DeleteFlow,
        Self::GetCommerceSettings,
        Self::UpdateCommerceSettings,
        Self::ListCatalogs,
        Self::GetCatalog,
        Self::ListCatalogProducts,
        Self::CreateQrCode,
        Self::ListQrCodes,
        Self::GetQrCode,
        Self::UpdateQrCode,
        Self::DeleteQrCode,
        Self::GetMessagingAnalytics,
        Self::GetConversationAnalytics,
        Self::GetPricingAnalytics,
        Self::GetTemplateAnalytics,
        Self::ListBlockedUsers,
        Self::BlockUsers,
        Self::UnblockUsers,
        Self::GetRoutingRoles,
        Self::PassThread,
        Self::TakeThread,
        Self::ReleaseThread,
    ];

    /// Its id: the HTTP adapter's `operationId`, e.g. `send_message`.
    #[allow(clippy::too_many_lines)] // one arm per operation
    pub fn id(self) -> &'static str {
        match self {
            Self::ListWabas => "list_wabas",
            Self::ListNumbers => "list_numbers",
            Self::GetNumber => "get_number",
            Self::GetProfile => "get_profile",
            Self::UpdateProfile => "update_profile",
            Self::RequestDisplayNameChange => "request_display_name_change",
            Self::GetDisplayName => "get_display_name",
            Self::GetNumberSettings => "get_number_settings",
            Self::UpdateNumberSettings => "update_number_settings",
            Self::EnablePayloadEncryption => "enable_payload_encryption",
            Self::DisablePayloadEncryption => "disable_payload_encryption",
            Self::GetUsername => "get_username",
            Self::SetUsername => "set_username",
            Self::DeleteUsername => "delete_username",
            Self::ListUsernameSuggestions => "list_username_suggestions",
            Self::GetOfficialBusinessAccount => "get_official_business_account",
            Self::RequestOfficialBusinessAccount => "request_official_business_account",
            Self::GetComplianceInfo => "get_compliance_info",
            Self::SetComplianceInfo => "set_compliance_info",
            Self::GetHealthStatus => "get_health_status",
            Self::GetConversationalComponents => "get_conversational_components",
            Self::SetConversationalComponents => "set_conversational_components",
            Self::DeleteContactBookEntry => "delete_contact_book_entry",
            Self::DisconnectWaba => "disconnect_waba",
            Self::SendMessage => "send_message",
            Self::MarkRead => "mark_read",
            Self::UploadMedia => "upload_media",
            Self::DownloadMedia => "download_media",
            Self::StartMediaUpload => "start_media_upload",
            Self::UploadMediaChunk => "upload_media_chunk",
            Self::GetMediaUpload => "get_media_upload",
            Self::DeleteMedia => "delete_media",
            Self::ListTemplates => "list_templates",
            Self::CreateTemplate => "create_template",
            Self::DeleteTemplates => "delete_templates",
            Self::GetTemplate => "get_template",
            Self::ListEvents => "list_events",
            Self::GetEvent => "get_event",
            Self::StreamEvents => "stream_events",
            Self::StreamNumberEvents => "stream_number_events",
            Self::ListConversations => "list_conversations",
            Self::ListConversationMessages => "list_conversation_messages",
            Self::ReplyToConversation => "reply_to_conversation",
            Self::GetConversationWindow => "get_conversation_window",
            Self::MarkConversationRead => "mark_conversation_read",
            Self::GetInboxMessage => "get_inbox_message",
            Self::CreateAuthenticationTemplate => "create_authentication_template",
            Self::IssueOtp => "issue_otp",
            Self::VerifyOtp => "verify_otp",
            Self::RequestVerificationCode => "request_verification_code",
            Self::VerifyCode => "verify_code",
            Self::RegisterNumber => "register_number",
            Self::DeregisterNumber => "deregister_number",
            Self::SetTwoStepPin => "set_two_step_pin",
            Self::ListPaymentConfigurations => "list_payment_configurations",
            Self::CreatePaymentConfiguration => "create_payment_configuration",
            Self::GetPaymentConfiguration => "get_payment_configuration",
            Self::DeletePaymentConfiguration => "delete_payment_configuration",
            Self::RegeneratePaymentOauthLink => "regenerate_payment_oauth_link",
            Self::GetPayment => "get_payment",
            Self::RefundPayment => "refund_payment",
            Self::StartSignup => "start_signup",
            Self::CompleteSignup => "complete_signup",
            Self::ListSignupAttempts => "list_signup_attempts",
            Self::ResumeSignup => "resume_signup",
            Self::EraseCustomer => "erase_customer",
            Self::CreateWebhookEndpoint => "create_webhook_endpoint",
            Self::ListWebhookEndpoints => "list_webhook_endpoints",
            Self::GetWebhookEndpoint => "get_webhook_endpoint",
            Self::UpdateWebhookEndpoint => "update_webhook_endpoint",
            Self::DeleteWebhookEndpoint => "delete_webhook_endpoint",
            Self::RotateWebhookSecret => "rotate_webhook_secret",
            Self::TestWebhookEndpoint => "test_webhook_endpoint",
            Self::ListWebhookDeliveries => "list_webhook_deliveries",
            Self::RetryWebhookDelivery => "retry_webhook_delivery",
            Self::EditTemplate => "edit_template",
            Self::ListLibraryTemplates => "list_library_templates",
            Self::CreateLibraryTemplate => "create_library_template",
            Self::MigrateTemplates => "migrate_templates",
            Self::CompareTemplates => "compare_templates",
            Self::UnpauseTemplate => "unpause_template",
            Self::ArchiveTemplates => "archive_templates",
            Self::UnarchiveTemplates => "unarchive_templates",
            Self::CreateTemplateGroup => "create_template_group",
            Self::GetTemplateGroup => "get_template_group",
            Self::UpdateTemplateGroup => "update_template_group",
            Self::DeleteTemplateGroup => "delete_template_group",
            Self::CreateGroup => "create_group",
            Self::ListGroups => "list_groups",
            Self::GetGroup => "get_group",
            Self::UpdateGroup => "update_group",
            Self::DeleteGroup => "delete_group",
            Self::GetGroupInviteLink => "get_group_invite_link",
            Self::ResetGroupInviteLink => "reset_group_invite_link",
            Self::ListGroupParticipants => "list_group_participants",
            Self::RemoveGroupParticipants => "remove_group_participants",
            Self::ListGroupJoinRequests => "list_group_join_requests",
            Self::ApproveGroupJoinRequests => "approve_group_join_requests",
            Self::RejectGroupJoinRequests => "reject_group_join_requests",
            Self::GetGroupAnalytics => "get_group_analytics",
            Self::CreateFlow => "create_flow",
            Self::ListFlows => "list_flows",
            Self::GetFlow => "get_flow",
            Self::UpdateFlow => "update_flow",
            Self::UploadFlowJson => "upload_flow_json",
            Self::PreviewFlow => "preview_flow",
            Self::PublishFlow => "publish_flow",
            Self::DeprecateFlow => "deprecate_flow",
            Self::DeleteFlow => "delete_flow",
            Self::GetCommerceSettings => "get_commerce_settings",
            Self::UpdateCommerceSettings => "update_commerce_settings",
            Self::ListCatalogs => "list_catalogs",
            Self::GetCatalog => "get_catalog",
            Self::ListCatalogProducts => "list_catalog_products",
            Self::CreateQrCode => "create_qr_code",
            Self::ListQrCodes => "list_qr_codes",
            Self::GetQrCode => "get_qr_code",
            Self::UpdateQrCode => "update_qr_code",
            Self::DeleteQrCode => "delete_qr_code",
            Self::GetMessagingAnalytics => "get_messaging_analytics",
            Self::GetConversationAnalytics => "get_conversation_analytics",
            Self::GetPricingAnalytics => "get_pricing_analytics",
            Self::GetTemplateAnalytics => "get_template_analytics",
            Self::ListBlockedUsers => "list_blocked_users",
            Self::BlockUsers => "block_users",
            Self::UnblockUsers => "unblock_users",
            Self::GetRoutingRoles => "get_routing_roles",
            Self::PassThread => "pass_thread",
            Self::TakeThread => "take_thread",
            Self::ReleaseThread => "release_thread",
        }
    }

    /// The operation of id `id`, if it is a tenant operation.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|operation| operation.id() == id)
    }

    /// The scope a key needs for it (docs/design/server.md, section 3.2).
    #[allow(clippy::too_many_lines)] // one arm per operation
    pub fn scope(self) -> Scope {
        match self {
            Self::ListWabas
            | Self::ListNumbers
            | Self::GetNumber
            | Self::GetProfile
            | Self::UpdateProfile
            | Self::RequestDisplayNameChange
            | Self::GetDisplayName
            | Self::GetNumberSettings
            | Self::UpdateNumberSettings
            | Self::EnablePayloadEncryption
            | Self::DisablePayloadEncryption
            | Self::GetUsername
            | Self::SetUsername
            | Self::DeleteUsername
            | Self::ListUsernameSuggestions
            | Self::GetOfficialBusinessAccount
            | Self::RequestOfficialBusinessAccount
            | Self::GetComplianceInfo
            | Self::SetComplianceInfo
            | Self::GetHealthStatus
            | Self::GetConversationalComponents
            | Self::SetConversationalComponents
            | Self::DisconnectWaba
            | Self::RequestVerificationCode
            | Self::VerifyCode
            | Self::RegisterNumber
            | Self::DeregisterNumber
            | Self::SetTwoStepPin => Scope::Numbers,
            Self::SendMessage | Self::MarkRead => Scope::Send,
            Self::UploadMedia
            | Self::DownloadMedia
            | Self::DeleteMedia
            | Self::StartMediaUpload
            | Self::UploadMediaChunk
            | Self::GetMediaUpload => Scope::Media,
            Self::ListTemplates
            | Self::CreateTemplate
            | Self::DeleteTemplates
            | Self::GetTemplate
            | Self::CreateAuthenticationTemplate
            | Self::EditTemplate
            | Self::ListLibraryTemplates
            | Self::CreateLibraryTemplate
            | Self::MigrateTemplates
            | Self::CompareTemplates
            | Self::UnpauseTemplate
            | Self::ArchiveTemplates
            | Self::UnarchiveTemplates
            | Self::CreateTemplateGroup
            | Self::GetTemplateGroup
            | Self::UpdateTemplateGroup
            | Self::DeleteTemplateGroup => Scope::Templates,
            Self::CreateFlow
            | Self::ListFlows
            | Self::GetFlow
            | Self::UpdateFlow
            | Self::UploadFlowJson
            | Self::PreviewFlow
            | Self::PublishFlow
            | Self::DeprecateFlow
            | Self::DeleteFlow => Scope::Flows,
            Self::ListEvents | Self::GetEvent | Self::StreamEvents | Self::StreamNumberEvents => {
                Scope::Events
            }
            Self::ListConversations
            | Self::ListConversationMessages
            | Self::ReplyToConversation
            | Self::GetConversationWindow
            | Self::MarkConversationRead
            | Self::GetInboxMessage => Scope::Inbox,
            Self::IssueOtp | Self::VerifyOtp => Scope::Otp,
            Self::ListPaymentConfigurations
            | Self::CreatePaymentConfiguration
            | Self::GetPaymentConfiguration
            | Self::DeletePaymentConfiguration
            | Self::RegeneratePaymentOauthLink
            | Self::GetPayment
            | Self::RefundPayment => Scope::Payments,
            Self::StartSignup
            | Self::CompleteSignup
            | Self::ListSignupAttempts
            | Self::ResumeSignup => Scope::Signup,
            Self::EraseCustomer | Self::DeleteContactBookEntry => Scope::Erasure,
            Self::GetCommerceSettings
            | Self::UpdateCommerceSettings
            | Self::ListCatalogs
            | Self::GetCatalog
            | Self::ListCatalogProducts => Scope::Commerce,
            Self::CreateQrCode
            | Self::ListQrCodes
            | Self::GetQrCode
            | Self::UpdateQrCode
            | Self::DeleteQrCode => Scope::QrCodes,
            Self::GetMessagingAnalytics
            | Self::GetConversationAnalytics
            | Self::GetPricingAnalytics
            | Self::GetTemplateAnalytics => Scope::Analytics,
            Self::ListBlockedUsers | Self::BlockUsers | Self::UnblockUsers => Scope::Contacts,
            Self::CreateWebhookEndpoint
            | Self::ListWebhookEndpoints
            | Self::GetWebhookEndpoint
            | Self::UpdateWebhookEndpoint
            | Self::DeleteWebhookEndpoint
            | Self::RotateWebhookSecret
            | Self::TestWebhookEndpoint
            | Self::ListWebhookDeliveries
            | Self::RetryWebhookDelivery => Scope::Webhooks,
            Self::CreateGroup
            | Self::ListGroups
            | Self::GetGroup
            | Self::UpdateGroup
            | Self::DeleteGroup
            | Self::GetGroupInviteLink
            | Self::ResetGroupInviteLink
            | Self::ListGroupParticipants
            | Self::RemoveGroupParticipants
            | Self::ListGroupJoinRequests
            | Self::ApproveGroupJoinRequests
            | Self::RejectGroupJoinRequests
            | Self::GetGroupAnalytics => Scope::Groups,
            Self::GetRoutingRoles | Self::PassThread | Self::TakeThread | Self::ReleaseThread => {
                Scope::Routing
            }
        }
    }
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each id once, in snake case, and read back to its operation.
    #[test]
    fn each_operation_has_one_id() {
        let mut seen = std::collections::HashSet::new();
        for operation in Operation::ALL {
            let id = operation.id();
            assert!(seen.insert(id), "{id} twice");
            assert!(
                id.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{id}"
            );
            assert_eq!(Operation::from_id(id), Some(operation));
            assert_eq!(operation.to_string(), id);
        }
        assert_eq!(Operation::from_id("create_tenant"), None, "an admin's");
        assert_eq!(Operation::from_id("Send_Message"), None);
    }
}
