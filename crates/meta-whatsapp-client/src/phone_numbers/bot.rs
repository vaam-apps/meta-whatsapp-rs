//! WhatsApp Business Bot details: `GET /{WABA-Bot-ID}`
//! (`reference/whatsapp-business-bot/bot-details-api`).
//!
//! The bot node holds the same conversational components a number has
//! (prompts, commands, the welcome message flag); configure them per
//! number with
//! [`PhoneNumber::configure_conversational_automation`](super::PhoneNumber::configure_conversational_automation).
//!
//! # Where the docs stop
//!
//! The page has no response example (the tests use its schema's field
//! names), and none of Meta's mirrored pages says where a bot id comes
//! from.

use meta_whatsapp_core::Result;
use meta_whatsapp_core::ids::WabaBotId;
use serde::{Deserialize, Serialize};

use super::{BotCommand, fields_param};
use crate::Client;

/// Entry point, see [`Client::waba_bot`].
#[derive(Debug, Clone)]
pub struct WabaBot {
    client: Client,
    bot_id: WabaBotId,
}

impl Client {
    /// [`WabaBot`] API for `bot_id`.
    pub fn waba_bot(&self, bot_id: impl Into<WabaBotId>) -> WabaBot {
        WabaBot {
            client: self.clone(),
            bot_id: bot_id.into(),
        }
    }
}

/// A WhatsApp Business Bot (`WhatsAppBusinessBot`). Which fields are
/// present depends on the `fields` asked for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[non_exhaustive]
pub struct WabaBotInfo {
    /// The bot id.
    pub id: WabaBotId,
    /// Ice breakers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<String>,
    /// Slash commands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<BotCommand>,
    /// Whether the welcome message is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_welcome_message: Option<bool>,
}

impl WabaBot {
    /// The id this API is scoped to.
    pub fn id(&self) -> &WabaBotId {
        &self.bot_id
    }

    /// The client this API uses.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `GET /{WABA-Bot-ID}` with `fields` (empty = Meta's defaults:
    /// `prompts`, `commands`, `enable_welcome_message`; `id` is also
    /// available).
    pub async fn get(&self, fields: &[&str]) -> Result<WabaBotInfo> {
        self.client
            .get_at(&[self.bot_id.as_str()])
            .query_opt("fields", fields_param(fields))
            .context("WhatsApp Business Bot")
            .send()
            .await
    }
}

#[cfg(test)]
mod tests {
    use http::Method;
    use meta_whatsapp_core::testing::ScriptedTransport;
    use pretty_assertions::assert_eq;
    use serde_json::json;

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

    #[tokio::test]
    async fn get_reads_the_bot() {
        // The page has no response example: this is its schema's fields.
        let t = ScriptedTransport::new();
        t.push_json(
            200,
            json!({
                "id": "712345678901234",
                "prompts": ["Book a flight", "plan a vacation"],
                "commands": [
                    {"command_name": "tickets", "command_description": "Book flight tickets"},
                    {"command_name": "hotel", "command_description": "Book hotel"}
                ],
                "enable_welcome_message": true
            }),
        );
        let bot = client(&t)
            .waba_bot("712345678901234")
            .get(&["id", "prompts", "commands", "enable_welcome_message"])
            .await
            .unwrap();
        let req = t.last_request().unwrap();
        assert_eq!(req.method, Method::GET);
        assert_eq!(req.path(), "/v25.0/712345678901234");
        assert_eq!(
            req.query("fields").as_deref(),
            Some("id,prompts,commands,enable_welcome_message")
        );
        assert_eq!(req.bearer(), Some("TOKEN"));
        assert_eq!(bot.id, WabaBotId::new("712345678901234"));
        assert_eq!(bot.prompts, vec!["Book a flight", "plan a vacation"]);
        assert_eq!(
            bot.commands,
            vec![
                BotCommand::new("tickets", "Book flight tickets"),
                BotCommand::new("hotel", "Book hotel")
            ]
        );
        assert_eq!(bot.enable_welcome_message, Some(true));
        assert_eq!(t.remaining(), 0);

        // Meta's defaults: no `fields`; absent lists parse as empty.
        t.push_json(200, json!({"id": "712345678901234"}));
        let bare = client(&t)
            .waba_bot("712345678901234")
            .get(&[])
            .await
            .unwrap();
        assert_eq!(t.last_request().unwrap().query("fields"), None);
        assert!(bare.prompts.is_empty() && bare.commands.is_empty());
        assert_eq!(t.remaining(), 0);
    }

    #[tokio::test]
    async fn a_bot_id_stays_one_segment() {
        let t = ScriptedTransport::new();
        t.push_json(200, json!({"id": "1"}));
        client(&t)
            .waba_bot("1/subscribed_apps")
            .get(&[])
            .await
            .unwrap();
        assert_eq!(
            t.last_request().unwrap().path(),
            "/v25.0/1%2Fsubscribed_apps"
        );
        assert!(client(&t).waba_bot("..").get(&[]).await.is_err());
        assert_eq!(t.requests().len(), 1);
        assert_eq!(t.remaining(), 0);
    }
}
