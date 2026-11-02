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
    /// `update_profile`: a number's business profile changed.
    UpdateProfile,
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
}

impl Operation {
    /// Every tenant operation.
    pub const ALL: [Self; 39] = [
        Self::ListWabas,
        Self::ListNumbers,
        Self::GetNumber,
        Self::GetProfile,
        Self::UpdateProfile,
        Self::DisconnectWaba,
        Self::SendMessage,
        Self::MarkRead,
        Self::UploadMedia,
        Self::DownloadMedia,
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
    ];

    /// Its id: the HTTP adapter's `operationId`, e.g. `send_message`.
    pub fn id(self) -> &'static str {
        match self {
            Self::ListWabas => "list_wabas",
            Self::ListNumbers => "list_numbers",
            Self::GetNumber => "get_number",
            Self::GetProfile => "get_profile",
            Self::UpdateProfile => "update_profile",
            Self::DisconnectWaba => "disconnect_waba",
            Self::SendMessage => "send_message",
            Self::MarkRead => "mark_read",
            Self::UploadMedia => "upload_media",
            Self::DownloadMedia => "download_media",
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
        }
    }

    /// The operation of id `id`, if it is a tenant operation.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|operation| operation.id() == id)
    }

    /// The scope a key needs for it (docs/design/server.md, section 3.2).
    pub fn scope(self) -> Scope {
        match self {
            Self::ListWabas
            | Self::ListNumbers
            | Self::GetNumber
            | Self::GetProfile
            | Self::UpdateProfile
            | Self::DisconnectWaba => Scope::Numbers,
            Self::SendMessage | Self::MarkRead => Scope::Send,
            Self::UploadMedia | Self::DownloadMedia | Self::DeleteMedia => Scope::Media,
            Self::ListTemplates
            | Self::CreateTemplate
            | Self::DeleteTemplates
            | Self::GetTemplate
            | Self::CreateAuthenticationTemplate => Scope::Templates,
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
