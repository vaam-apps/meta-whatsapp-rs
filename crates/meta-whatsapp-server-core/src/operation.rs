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
}

impl Operation {
    /// Every tenant operation.
    pub const ALL: [Self; 16] = [
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
            | Self::GetTemplate => Scope::Templates,
            Self::ListEvents => Scope::Events,
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
