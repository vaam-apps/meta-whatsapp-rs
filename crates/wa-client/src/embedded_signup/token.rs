//! The exchangeable code, the business token it becomes, and what
//! `debug_token` says about that token.

use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::{AppId, WabaId};
use wa_core::secret::{AccessToken, SecretBytes};

/// The permission whose `target_ids` are the WABAs a token can manage.
pub const WHATSAPP_BUSINESS_MANAGEMENT: &str = "whatsapp_business_management";
/// The permission to send messages and manage number settings.
pub const WHATSAPP_BUSINESS_MESSAGING: &str = "whatsapp_business_messaging";

/// The exchangeable token code from `FB.login`'s `authResponse.code`.
///
/// Single use, and valid for 30 seconds (`embedded-signup/implementation`):
/// exchange it as soon as it reaches your server. Held in [`SecretBytes`]
/// (zeroed on drop); `Debug` is redacted; the value is only readable through
/// [`Self::expose_secret`].
#[derive(Clone)]
pub struct SignupCode(SecretBytes);

impl SignupCode {
    /// Wrap a code. Rejects empty strings.
    pub fn new(code: impl Into<String>) -> Result<Self> {
        let code = SecretBytes::new(code.into().into_bytes());
        if secret_str(&code).trim().is_empty() {
            return Err(ValidationError::new("code", "required").into());
        }
        Ok(Self(code))
    }

    /// Read the code. Keep the borrow short; never log it.
    pub fn expose_secret(&self) -> &str {
        secret_str(&self.0)
    }
}

/// The text of a [`SecretBytes`] built from a `String`. Always valid UTF-8
/// by construction, so the fallback is unreachable.
fn secret_str(bytes: &SecretBytes) -> &str {
    std::str::from_utf8(bytes.expose_secret()).unwrap_or_default()
}

impl fmt::Debug for SignupCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SignupCode([REDACTED])")
    }
}

/// The business integration system user token ("business token") returned
/// by the code exchange.
///
/// Meta's Embedded Signup pages print the response as a bare
/// `<BUSINESS_TOKEN>` placeholder; the shape here is the standard Graph
/// `oauth/access_token` response (`access_token`, `token_type`,
/// `expires_in`). `Debug` never shows the token.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct BusinessToken {
    /// The token.
    pub access_token: AccessToken,
    /// Usually `bearer`.
    #[serde(default)]
    pub token_type: Option<String>,
    /// Lifetime in seconds, when the token expires.
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// The `data` object of `GET /debug_token`.
///
/// Timestamps are unix seconds as Meta sends them; `0` (as in the
/// `permissions` example for a system user token) means "does not expire".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct TokenDebug {
    /// App the token was issued for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<AppId>,
    /// Token type (`USER`, `SYSTEM_USER`, …).
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub token_type: Option<TokenType>,
    /// App name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<String>,
    /// Token expiry (unix seconds; `0` = never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// Data access expiry (unix seconds; `0` = never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_access_expires_at: Option<i64>,
    /// Issue time (unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<i64>,
    /// Whether the token is currently valid.
    #[serde(default)]
    pub is_valid: bool,
    /// Granted permissions.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Permissions with the object ids they apply to.
    #[serde(default)]
    pub granular_scopes: Vec<GranularScope>,
    /// User (or system user) the token represents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// One entry of `granular_scopes`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct GranularScope {
    /// Permission name.
    pub scope: String,
    /// Object ids the permission is limited to. Absent when the permission
    /// is not limited to specific objects (Meta's system user example).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ids: Option<Vec<String>>,
}

/// Token type reported by `debug_token`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum TokenType {
    /// User token.
    User,
    /// System user token (including business integration system users).
    SystemUser,
    /// A value this crate does not know yet.
    #[serde(other)]
    Unknown,
}

impl TokenDebug {
    /// `target_ids` of `scope`, if the token has that scope limited to
    /// specific objects.
    pub fn target_ids(&self, scope: &str) -> Option<&[String]> {
        self.granular_scopes
            .iter()
            .find(|g| g.scope == scope)
            .and_then(|g| g.target_ids.as_deref())
    }

    /// WABAs that granted the app `whatsapp_business_management`, most
    /// recently onboarded first (`solution-providers/manage-accounts`).
    pub fn waba_ids(&self) -> Vec<WabaId> {
        self.target_ids(WHATSAPP_BUSINESS_MANAGEMENT)
            .unwrap_or_default()
            .iter()
            .map(|id| WabaId::new(id.as_str()))
            .collect()
    }

    /// Whether `scope` was granted.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    /// `expires_at` as a time; `None` when absent or `0` (never expires).
    pub fn expires_at_time(&self) -> Option<OffsetDateTime> {
        self.expires_at
            .filter(|s| *s > 0)
            .and_then(|s| OffsetDateTime::from_unix_timestamp(s).ok())
    }
}

#[derive(Deserialize)]
pub(crate) struct DebugEnvelope {
    pub(crate) data: TokenDebug,
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    /// `solution-providers/manage-accounts`, "Get shared WABA ID with access
    /// token" (the page's `// …` comment removed: JSON has no comments).
    pub(crate) const MANAGE_ACCOUNTS_EXAMPLE: &str = r#"{
      "data" : {
        "app_id" : "670843887433847",
        "application" : "JaspersMarket",
        "data_access_expires_at" : 1672092840,
        "expires_at" : 1665090000,
        "granular_scopes" : [
          {"scope" : "whatsapp_business_management", "target_ids" : ["102289599326934", "101569239400667"]},
          {"scope" : "whatsapp_business_messaging", "target_ids" : ["102289599326934", "101569239400667"]}
        ],
        "is_valid" : true,
        "scopes" : ["whatsapp_business_management", "whatsapp_business_messaging", "public_profile"],
        "type" : "USER",
        "user_id" : "10222270944537964"
      }
    }"#;

    #[test]
    fn parses_manage_accounts_example() {
        let d: DebugEnvelope = serde_json::from_str(MANAGE_ACCOUNTS_EXAMPLE).unwrap();
        let d = d.data;
        assert!(d.is_valid);
        assert_eq!(d.token_type, Some(TokenType::User));
        assert_eq!(
            d.app_id.as_ref().map(AppId::as_str),
            Some("670843887433847")
        );
        assert_eq!(
            d.waba_ids(),
            vec![
                WabaId::new("102289599326934"),
                WabaId::new("101569239400667")
            ]
        );
        assert!(d.has_scope(WHATSAPP_BUSINESS_MESSAGING));
        assert_eq!(d.expires_at_time().unwrap().unix_timestamp(), 1665090000);
    }

    #[test]
    fn parses_permissions_example_without_target_ids() {
        // permissions, "Checking for granted permissions".
        let v = r#"{
          "data": {
            "app_id": "634974688087057",
            "type": "SYSTEM_USER",
            "application": "Lucky Shrub",
            "data_access_expires_at": 0,
            "expires_at": 0,
            "is_valid": true,
            "issued_at": 1712099387,
            "scopes": ["whatsapp_business_management", "whatsapp_business_messaging"],
            "granular_scopes": [
              {"scope": "whatsapp_business_management"},
              {"scope": "whatsapp_business_messaging"}
            ],
            "user_id": "104169029247128"
          }
        }"#;
        let d: DebugEnvelope = serde_json::from_str(v).unwrap();
        let d = d.data;
        assert_eq!(d.token_type, Some(TokenType::SystemUser));
        assert_eq!(d.target_ids(WHATSAPP_BUSINESS_MANAGEMENT), None);
        assert!(d.waba_ids().is_empty());
        assert_eq!(d.expires_at_time(), None, "0 means never");
    }

    #[test]
    fn code_and_token_are_redacted() {
        let code = SignupCode::new("AQBhlXsctMxJYbwbrpybxlo9").unwrap();
        assert_eq!(format!("{code:?}"), "SignupCode([REDACTED])");
        assert_eq!(code.expose_secret(), "AQBhlXsctMxJYbwbrpybxlo9");
        assert!(SignupCode::new(" ").is_err());
        assert!(SignupCode::new("").is_err());
        let t: BusinessToken =
            serde_json::from_str(r#"{"access_token": "EAAAN6tcBzAUBOwt", "token_type": "bearer"}"#)
                .unwrap();
        let dbg = format!("{t:?}");
        assert!(!dbg.contains("EAAAN6tcBzAUBOwt"), "{dbg}");
        assert_eq!(t.access_token.expose_secret(), "EAAAN6tcBzAUBOwt");
        assert_eq!(t.expires_in, None);
    }
}
