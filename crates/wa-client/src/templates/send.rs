//! Send-time template invocation: the `template` object of a message.
//!
//! This is the *contract* other modules build on (messages, OTP, the MM API):
//! `TemplateMessage::new(name, language_code)` and its JSON shape
//! `{"name": .., "language": {"code": ..}, "components": [..]}` are stable.
//! The component types are fleshed out by the templates module.

use serde::{Deserialize, Serialize};

/// The `template` object of a send request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateMessage {
    /// Template name.
    pub name: String,
    /// Language (locale code the template was approved in).
    pub language: TemplateLanguage,
    /// Parameters for header, body, buttons, carousel cards. Empty for
    /// templates without variables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<serde_json::Value>,
}

impl TemplateMessage {
    /// Invoke `name` in `language_code` (e.g. `en_US`), without parameters.
    pub fn new(name: impl Into<String>, language_code: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            language: TemplateLanguage {
                code: language_code.into(),
                policy: None,
            },
            components: Vec::new(),
        }
    }
}

/// `template.language`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateLanguage {
    /// Locale code, e.g. `en_US`, `fr`.
    pub code: String,
    /// `deterministic` (the only documented value), when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_shape_is_stable() {
        let t = TemplateMessage::new("hello_world", "en_US");
        assert_eq!(
            serde_json::to_value(&t).unwrap(),
            serde_json::json!({"name": "hello_world", "language": {"code": "en_US"}})
        );
    }
}
