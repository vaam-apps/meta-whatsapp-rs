//! Send a text message, then a template message, from one business number.
//!
//! The smallest useful program: a production client from a token
//! (`wa_rs::client()`), a free-form text, and a template. Free-form messages
//! are only delivered inside the 24-hour customer service window (the
//! customer wrote to you in the last 24 hours); templates go through any
//! time. The text is sent first and, when Meta answers that the window is
//! closed, the program says so and carries on with the template — branching
//! on [`ErrorKind`], never on the error text.
//!
//! | Variable | Required | What |
//! | --- | --- | --- |
//! | `WA_TOKEN` | yes | a system user access token with `whatsapp_business_messaging` |
//! | `WA_PHONE_NUMBER_ID` | yes | the business phone number **id** that sends (not the number) |
//! | `WA_TO` | yes | the recipient, E.164 with `+`, e.g. `+16505551234` |
//! | `WA_TEMPLATE` | no | an approved template without variables (default `hello_world`, Meta's sample template) |
//! | `WA_TEMPLATE_LANGUAGE` | no | the language it was approved in (default `en_US`) |
//!
//! ```text
//! WA_TOKEN=… WA_PHONE_NUMBER_ID=… WA_TO=+16505551234 \
//!   cargo run -p wa-rs --example send_message
//! ```

use anyhow::Context as _;
use wa_rs::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // RUST_LOG=info (or debug) shows what the library logs.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let client = wa_rs::client(env("WA_TOKEN")?)?;
    let messages = client.messages(env("WA_PHONE_NUMBER_ID")?);
    let to = Recipient::phone(env("WA_TO")?); // E.164, with `+`

    // Free-form: delivered only inside the 24-hour customer service window.
    let text = OutboundMessage::text(to.clone(), "Your order has shipped.");
    match messages.send(&text).await {
        Ok(sent) => println!("text accepted as {:?}", sent.message_id()),
        Err(e) if e.kind() == ErrorKind::CustomerServiceWindowClosed => println!("window closed"),
        Err(e) => return Err(e.into()),
    }

    // A template: any time.
    let hello = TemplateMessage::new(
        env_or("WA_TEMPLATE", "hello_world"),
        env_or("WA_TEMPLATE_LANGUAGE", "en_US"),
    );
    let sent = messages.send(&OutboundMessage::template(to, hello)).await?;
    // A 200 means "accepted"; delivery arrives later as status webhooks
    // keyed by this id.
    println!("template accepted as {:?}", sent.message_id());
    Ok(())
}

/// `std::env::var`, naming the variable when it is missing.
fn env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).with_context(|| format!("set {name} (see the example's header)"))
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}
