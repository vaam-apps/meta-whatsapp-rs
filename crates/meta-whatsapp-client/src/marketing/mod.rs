//! Marketing Messages API for WhatsApp (MM API): send marketing templates
//! through `/marketing_messages` with MM-only options, check or drive
//! onboarding, and the max price beta (its agreement, a template's max
//! price, duplicating a template at another max price, reach estimates);
//! plus Click-to-WhatsApp welcome message sequences.
//!
//! Docs: `marketing-messages/send-marketing-messages`,
//! `marketing-messages/onboarding`, `marketing-messages/onboard-business-customers`,
//! `marketing-messages/pricing`, `marketing-messages/pricing/enroll-max-price`,
//! `marketing-messages/pricing/duplicate-templates`,
//! `ctwa/welcome-message-sequences`,
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
//! | [`Client::marketing_account`](crate::Client::marketing_account) | WABA | [`MarketingAccount::onboarding_status`], [`MarketingAccount::owner_business_info`], [`MarketingAccount::cloud_api_marketing_disabled`], [`MarketingAccount::set_cloud_api_marketing_disabled`], [`MarketingAccount::duplicate_template`], [`MarketingAccount::reach_estimate`] |
//! | [`Client::marketing_business`](crate::Client::marketing_business) | business portfolio | [`MarketingBusiness::onboarding_status`], [`MarketingBusiness::client_wabas_with_status`] (+ `_stream`), [`MarketingBusiness::request_onboarding`], [`MarketingBusiness::max_price_agreement`], [`MarketingBusiness::sign_max_price_agreement`] |
//! | [`Client::marketing_template`](crate::Client::marketing_template) | template | [`MarketingTemplate::max_price`], [`MarketingTemplate::set_max_price`], [`MarketingTemplate::clear_max_price`] |
//! | [`Client::welcome_sequences`](crate::Client::welcome_sequences) | WABA | [`WelcomeSequences::create`], [`WelcomeSequences::update`], [`WelcomeSequences::list`], [`WelcomeSequences::get`], [`WelcomeSequences::delete`] |
//!
//! # The max price agreement is a legal act
//!
//! Max price needs Meta's Max Price Beta Agreement, signed once per
//! business portfolio. [`MarketingBusiness::sign_max_price_agreement`]
//! signs it on the business's behalf: it is its own explicit call, and no
//! other call makes it (no send, duplicate, reach estimate, template update
//! or builder; an error saying the agreement is missing is returned, never
//! answered by signing). Signing is the deployer's decision (AGENTS.md §
//! Decisions, `OPEN_QUESTIONS.md` #45). [`MarketingBusiness::max_price_agreement`]
//! only reads its state.
//!
//! Since the open beta (2026-10-01) a Solution Partner no longer
//! allowlists its clients: its own agreement covers the client WABAs
//! shared with or managed by it, so there is no allow list to manage.
//!
//! What lives elsewhere: message TTL, deep links and creative-optimization
//! opt-outs are *template* settings (templates module); click events
//! (`user_actions` on the `messages` field) and the `account_update`
//! onboarding events are webhooks (`meta-whatsapp-webhooks`); conversion
//! and cost metrics come from Meta's Insights API and the analytics module.
//! A GIF template header is defined with
//! [`HeaderFormat::Gif`](crate::templates::HeaderFormat::Gif); Meta's pages
//! show no send-time parameter for it, so none is typed
//! ([`Parameter::Other`](crate::templates::Parameter::Other) carries one
//! verbatim).
//!
//! # Errors to expect
//!
//! Branch on [`ErrorKind`](meta_whatsapp_core::ErrorKind):
//! `MarketingNotAllowed` (`131055`, `134100`: not a marketing template;
//! `131063`: marketing disabled on Cloud API), `TemplateSyncing` (`134101`:
//! wait ~10 minutes after creating or reviving a template), `TemplateUnavailable`
//! (`134102`: ad sync failed or not eligible — check
//! [`MarketingAccount::onboarding_status`]), `DuplicateOnboarding`
//! (`1752041`, Intent API), plus every Cloud API send error (`131049`,
//! `131050`, …).
//!
//! The max price codes of `marketing-messages/pricing` have no kind of
//! their own yet: read [`GraphApiError::code`](meta_whatsapp_core::GraphApiError)
//! for `131061` (a max-price template sent through Cloud API `/messages`),
//! `131065` (no max price delivery to the recipient's country) and `100`
//! with "You need to sign the testing legal agreement" (the agreement is
//! missing: see above, the client does not sign it for you).
//!
//! # Send a marketing message
//!
//! ```no_run
//! # async fn demo(client: meta_whatsapp_client::Client) -> meta_whatsapp_client::Result<()> {
//! use meta_whatsapp_client::marketing::MarketingOptions;
//! use meta_whatsapp_client::templates::TemplateMessage;
//! use meta_whatsapp_core::recipient::Recipient;
//!
//! let sent = client
//!     .marketing(meta_whatsapp_core::ids::PhoneNumberId::new("<PHONE_NUMBER_ID>")?)
//!     .send(
//!         &Recipient::phone("+16505551234"),
//!         &TemplateMessage::new("seasonal_sale_promo", "en"),
//!         &MarketingOptions::new().strict().message_activity_sharing(true),
//!     )
//!     .await?;
//! println!("sent {:?}", sent.message_id());
//! # Ok(()) }
//! ```

mod agreement;
mod onboarding;
mod pricing;
mod send;
mod welcome;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_max_price;
#[cfg(test)]
mod tests_welcome;

pub use agreement::{AgreementStatus, MaxPriceAgreement, MaxPriceAgreementSigner};

pub use onboarding::{
    BusinessOnboardingStatus, ClientWaba, ListClientWabas, MarketingAccount, MarketingBusiness,
    OnboardingRequested, OnboardingStatus, OwnerBusinessInfo, TermsStatus,
};
pub use pricing::{
    BidStrategy, DateInterval, DuplicateTemplate, MAX_COUNTRY_MULTIPLIER, MAX_COUNTRY_MULTIPLIERS,
    MarketingTemplate, MaxPrice, OptimizationSpec, ReachEstimate, ReachEstimatePoint,
    ReachEstimateQuery,
};
pub use send::{
    Marketing, MarketingOptions, MessageStatus, ProductPolicy, SendResponse, SentContact,
    SentMessage,
};
pub use welcome::{
    AutofillMessage, IceBreaker, ListWelcomeSequences, WelcomeMessage, WelcomeSequence,
    WelcomeSequenceCreated, WelcomeSequenceUpdate, WelcomeSequences,
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
            /// A value this version of `meta-whatsapp-rs` does not know yet, verbatim.
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
