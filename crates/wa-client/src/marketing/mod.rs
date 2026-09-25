//! Marketing Messages API for WhatsApp (MM API): send marketing templates
//! through `/marketing_messages` with MM-only options, and check or drive
//! onboarding.
//!
//! Docs: `marketing-messages/send-marketing-messages`,
//! `marketing-messages/onboarding`, `marketing-messages/onboard-business-customers`,
//! `marketing-messages/pricing`,
//! `reference/whatsapp-business-phone-number/marketing-messages-api-for-whatsapp`,
//! `reference/business/whatsapp-business-partner-onboarding-to-mm-lite-api`,
//! `support/error-codes` (MM API section).
//!
//! Doc paths are relative to
//! `https://developers.facebook.com/documentation/business-messaging/whatsapp/`
//! (append `.md` for Markdown; `just meta-docs` mirrors them locally).
//!
//! | Accessor | Scope | Calls |
//! | --- | --- | --- |
//! | [`Client::marketing`](crate::Client::marketing) | phone number | [`Marketing::send`] |
//! | [`Client::marketing_account`](crate::Client::marketing_account) | WABA | [`MarketingAccount::onboarding_status`], [`MarketingAccount::owner_business_info`], [`MarketingAccount::cloud_api_marketing_disabled`], [`MarketingAccount::set_cloud_api_marketing_disabled`] |
//! | [`Client::marketing_business`](crate::Client::marketing_business) | business portfolio | [`MarketingBusiness::onboarding_status`], [`MarketingBusiness::client_wabas_with_status`] (+ `_stream`), [`MarketingBusiness::request_onboarding`] |
//!
//! What lives elsewhere: message TTL, max price (`optimization_spec`),
//! deep links and creative-optimization opt-outs are *template* settings
//! (templates module); click events (`user_actions` on the `messages`
//! field) and the `account_update` onboarding events are webhooks
//! (`wa-webhooks`); conversion and cost metrics come from Meta's Insights
//! API and the analytics module. Not wrapped: the max-price beta agreement
//! and partner allowlist (`marketing-messages/pricing/enroll-max-price`) and
//! reach estimates (`/{WABA_ID}/reachestimate`).
//!
//! # Errors to expect
//!
//! Branch on [`ErrorKind`](wa_core::ErrorKind):
//! `MarketingNotAllowed` (`131055`, `134100`: not a marketing template;
//! `131063`: marketing disabled on Cloud API), `TemplateSyncing` (`134101`:
//! wait ~10 minutes after creating or reviving a template), `TemplateUnavailable`
//! (`134102`: ad sync failed or not eligible — check
//! [`MarketingAccount::onboarding_status`]), `DuplicateOnboarding`
//! (`1752041`, Intent API), plus every Cloud API send error (`131049`,
//! `131050`, …).
//!
//! # Send a marketing message
//!
//! ```no_run
//! # async fn demo(client: wa_client::Client) -> wa_client::Result<()> {
//! use wa_client::marketing::MarketingOptions;
//! use wa_client::templates::TemplateMessage;
//! use wa_core::recipient::Recipient;
//!
//! let sent = client
//!     .marketing("<PHONE_NUMBER_ID>")
//!     .send(
//!         &Recipient::phone("+16505551234"),
//!         &TemplateMessage::new("seasonal_sale_promo", "en"),
//!         &MarketingOptions::new().strict().message_activity_sharing(true),
//!     )
//!     .await?;
//! println!("sent {:?}", sent.message_id());
//! # Ok(()) }
//! ```

mod onboarding;
mod send;

#[cfg(test)]
mod tests;

pub use onboarding::{
    BusinessOnboardingStatus, ClientWaba, ListClientWabas, MarketingAccount, MarketingBusiness,
    OnboardingRequested, OnboardingStatus, OwnerBusinessInfo, TermsStatus,
};
pub use send::{
    Marketing, MarketingOptions, MessageStatus, ProductPolicy, SendResponse, SentContact,
    SentMessage,
};

/// A string enum Meta may extend: documented values get variants, anything
/// else is kept verbatim in `Unknown(String)` so it round-trips and shows
/// up in logs. (Same shape as the Flows module's; each module keeps its own
/// copy so the two stay independent.)
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $wire:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        #[non_exhaustive]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value this version of `wa-rs` does not know yet, verbatim.
            Unknown(String),
        }

        impl $name {
            /// The value as it appears on the wire.
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $wire, )+
                    Self::Unknown(other) => other,
                }
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value {
                    $( $wire => Self::$variant, )+
                    other => Self::Unknown(other.to_owned()),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let raw = <String as ::serde::Deserialize>::deserialize(d)?;
                Ok(Self::from(raw.as_str()))
            }
        }
    };
}
use wire_enum;
