//! Conversational components: welcome message, ice breakers ("prompts") and
//! slash commands.
//!
//! Limits are the ones `business-phone-numbers/conversational-components`
//! states: at most 4 ice breakers of at most 80 characters, at most 30
//! commands with names of at most 32 characters and hints (descriptions) of
//! at most 256 characters. The reference adds that command names are unique
//! per number and carry no leading slash. The guide also says emojis are not
//! supported; that is not checked locally (there is no precise definition of
//! "emoji" to check against), Meta rejects them.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use wa_core::error::ValidationError;

/// Maximum number of ice breakers.
pub const MAX_PROMPTS: usize = 4;
/// Maximum characters per ice breaker.
pub const MAX_PROMPT_CHARS: usize = 80;
/// Maximum number of commands.
pub const MAX_COMMANDS: usize = 30;
/// Maximum characters per command name.
pub const MAX_COMMAND_NAME_CHARS: usize = 32;
/// Maximum characters per command description (hint).
pub const MAX_COMMAND_DESCRIPTION_CHARS: usize = 256;

/// One slash command.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct BotCommand {
    /// Command name, without the leading `/`.
    pub command_name: String,
    /// Hint shown next to the command.
    pub command_description: String,
}

impl BotCommand {
    /// Build a command.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            command_name: name.into(),
            command_description: description.into(),
        }
    }
}

/// Current configuration, from `GET /{PHONE_NUMBER_ID}?fields=conversational_automation`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct ConversationalAutomation {
    /// Whether the welcome message is on (not always returned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_welcome_message: Option<bool>,
    /// Ice breakers.
    #[serde(default)]
    pub prompts: Vec<String>,
    /// Slash commands.
    #[serde(default)]
    pub commands: Vec<BotCommand>,
}

/// Body of `POST /{PHONE_NUMBER_ID}/conversational_automation`. `None`
/// fields are left out of the request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ConversationalAutomationConfig {
    /// Turn the welcome message on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_welcome_message: Option<bool>,
    /// Ice breakers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    /// Slash commands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands: Option<Vec<BotCommand>>,
}

impl ConversationalAutomationConfig {
    /// Empty configuration (changes nothing until fields are set).
    pub fn new() -> Self {
        Self::default()
    }

    /// Turn the welcome message on or off.
    #[must_use]
    pub fn enable_welcome_message(mut self, enabled: bool) -> Self {
        self.enable_welcome_message = Some(enabled);
        self
    }

    /// Set the ice breakers.
    #[must_use]
    pub fn prompts<I, S>(mut self, prompts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.prompts = Some(prompts.into_iter().map(Into::into).collect());
        self
    }

    /// Set the commands.
    #[must_use]
    pub fn commands(mut self, commands: impl IntoIterator<Item = BotCommand>) -> Self {
        self.commands = Some(commands.into_iter().collect());
        self
    }

    /// Check the documented limits.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(prompts) = &self.prompts {
            if prompts.len() > MAX_PROMPTS {
                return Err(ValidationError::new(
                    "prompts",
                    format!("at most {MAX_PROMPTS} ice breakers"),
                ));
            }
            for (i, p) in prompts.iter().enumerate() {
                let n = p.chars().count();
                if n == 0 || n > MAX_PROMPT_CHARS {
                    return Err(ValidationError::new(
                        format!("prompts[{i}]"),
                        format!("must be 1-{MAX_PROMPT_CHARS} characters"),
                    ));
                }
            }
        }
        if let Some(commands) = &self.commands {
            if commands.len() > MAX_COMMANDS {
                return Err(ValidationError::new(
                    "commands",
                    format!("at most {MAX_COMMANDS} commands"),
                ));
            }
            let mut seen = HashSet::new();
            for (i, c) in commands.iter().enumerate() {
                let name = c.command_name.chars().count();
                if name == 0 || name > MAX_COMMAND_NAME_CHARS {
                    return Err(ValidationError::new(
                        format!("commands[{i}].command_name"),
                        format!("must be 1-{MAX_COMMAND_NAME_CHARS} characters"),
                    ));
                }
                if c.command_name.starts_with('/') {
                    return Err(ValidationError::new(
                        format!("commands[{i}].command_name"),
                        "must not start with `/`",
                    ));
                }
                if !seen.insert(c.command_name.as_str()) {
                    return Err(ValidationError::new(
                        format!("commands[{i}].command_name"),
                        "command names must be unique",
                    ));
                }
                let desc = c.command_description.chars().count();
                if desc == 0 || desc > MAX_COMMAND_DESCRIPTION_CHARS {
                    return Err(ValidationError::new(
                        format!("commands[{i}].command_description"),
                        format!("must be 1-{MAX_COMMAND_DESCRIPTION_CHARS} characters"),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// `{"conversational_automation": {...}, "id": "..."}`.
#[derive(Deserialize)]
pub(crate) struct AutomationEnvelope {
    #[serde(default)]
    pub(crate) conversational_automation: Option<ConversationalAutomation>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(err: ValidationError) -> String {
        err.field
    }

    #[test]
    fn documented_limits_are_enforced() {
        let ok = ConversationalAutomationConfig::new()
            .prompts(["a"; 4])
            .commands((0..30).map(|i| BotCommand::new(format!("c{i}"), "d")));
        assert!(ok.validate().is_ok());

        let too_many = ConversationalAutomationConfig::new().prompts(["a"; 5]);
        assert_eq!(field(too_many.validate().unwrap_err()), "prompts");
        let long = ConversationalAutomationConfig::new().prompts(["x".repeat(81)]);
        assert_eq!(field(long.validate().unwrap_err()), "prompts[0]");
        let edge = ConversationalAutomationConfig::new().prompts(["é".repeat(80)]);
        assert!(
            edge.validate().is_ok(),
            "limit counts characters, not bytes"
        );

        let cmds = ConversationalAutomationConfig::new()
            .commands((0..31).map(|i| BotCommand::new(format!("c{i}"), "d")));
        assert_eq!(field(cmds.validate().unwrap_err()), "commands");
        let name =
            ConversationalAutomationConfig::new().commands([BotCommand::new("n".repeat(33), "d")]);
        assert_eq!(
            field(name.validate().unwrap_err()),
            "commands[0].command_name"
        );
        let name32 = ConversationalAutomationConfig::new()
            .commands([BotCommand::new("n".repeat(32), "d".repeat(256))]);
        assert!(name32.validate().is_ok());
        let desc =
            ConversationalAutomationConfig::new().commands([BotCommand::new("n", "d".repeat(257))]);
        assert_eq!(
            field(desc.validate().unwrap_err()),
            "commands[0].command_description"
        );
        let slash =
            ConversationalAutomationConfig::new().commands([BotCommand::new("/imagine", "d")]);
        assert!(slash.validate().is_err());
        let dup = ConversationalAutomationConfig::new()
            .commands([BotCommand::new("a", "d"), BotCommand::new("a", "e")]);
        assert_eq!(
            field(dup.validate().unwrap_err()),
            "commands[1].command_name"
        );
    }
}
