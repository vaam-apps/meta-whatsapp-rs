//! Template **definitions**: what you send to create or edit a template
//! (`POST /{waba}/message_templates`, `POST /{template_id}`), and what
//! `GET` returns in `components`.
//!
//! Not to be confused with [`super::TemplateMessage`], the *invocation* that
//! fills a template's placeholders when sending it.
//!
//! Pages: `templates/components`, `templates/overview`,
//! `templates/marketing-templates/*`, `templates/utility-templates/*`,
//! `templates/authentication-templates/*`, `catalogs/*-template-messages`,
//! `calling/call-button-messages-deep-links`, `business-scoped-user-ids`
//! (request-contact-info button).
//!
//! ## Wire shape and leniency
//!
//! Components and buttons are internally tagged on `type`. Serialization
//! uses the upper-case spelling of the reference schema (`"HEADER"`,
//! `"QUICK_REPLY"`), which is also what `GET` returns; the one exception is
//! `call_permission_request`, which every page spells in lower case and no
//! schema lists. Deserialization accepts the upper- and lower-case spellings
//! the pages use, and anything else — an unknown type, a known type with an
//! unexpected shape — lands in an `Other(serde_json::Value)` variant
//! verbatim, so a template Meta extends never fails a `list()` and still
//! round-trips through `edit()`.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use wa_core::Result;

use super::macros::string_enum;
use super::types::{
    DisplayFormat, ParameterFormat, SendType, TemplateCategory, TemplateSubCategory,
};
use super::validate;

/// A template to create (`POST /{waba}/message_templates`).
///
/// Build with [`TemplateDefinition::new`] and the component constructors on
/// [`TemplateComponent`]; [`TemplateDefinition::validate`] (called by
/// [`super::Templates::create`]) checks every documented hard limit first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateDefinition {
    /// Lowercase letters, digits and underscores, at most 512 characters.
    /// Not unique: one name exists once per language.
    pub name: String,
    /// Language and locale code (`templates/supported-languages`), e.g.
    /// `en_US`.
    pub language: String,
    /// Category; Meta may re-categorize (`templates/template-categorization`).
    pub category: TemplateCategory,
    /// Placeholder style. Meta assumes positional when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_format: Option<ParameterFormat>,
    /// Header, body (required), footer, buttons, carousel, … in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<TemplateComponent>,
    /// Let Meta re-assign the category. Since 2025-04-09 this is Meta's
    /// default behaviour anyway (`templates/template-categorization`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_category_change: Option<bool>,
    /// Opt out of CTA URL link tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cta_url_link_tracking_opted_out: Option<bool>,
    /// Message time-to-live in seconds (`templates/time-to-live`). The
    /// allowed range depends on the category; `-1` means 30 days for
    /// authentication and utility templates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_send_ttl_seconds: Option<i64>,
    /// Sub-category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_category: Option<TemplateSubCategory>,
    /// Display format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_format: Option<DisplayFormat>,
    /// Deliver only to the user's primary device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_primary_device_delivery_only: Option<bool>,
    /// Marketing Messages API send type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_type: Option<SendType>,
}

impl TemplateDefinition {
    /// A definition without components; add them with
    /// [`component`](Self::component).
    pub fn new(
        name: impl Into<String>,
        language: impl Into<String>,
        category: TemplateCategory,
    ) -> Self {
        Self {
            name: name.into(),
            language: language.into(),
            category,
            parameter_format: None,
            components: Vec::new(),
            allow_category_change: None,
            cta_url_link_tracking_opted_out: None,
            message_send_ttl_seconds: None,
            sub_category: None,
            display_format: None,
            is_primary_device_delivery_only: None,
            send_type: None,
        }
    }

    /// Append a component.
    #[must_use]
    pub fn component(mut self, component: TemplateComponent) -> Self {
        self.components.push(component);
        self
    }

    /// Set the placeholder style.
    #[must_use]
    pub fn parameter_format(mut self, format: ParameterFormat) -> Self {
        self.parameter_format = Some(format);
        self
    }

    /// Set the message time-to-live.
    #[must_use]
    pub fn message_send_ttl_seconds(mut self, seconds: i64) -> Self {
        self.message_send_ttl_seconds = Some(seconds);
        self
    }

    /// Set `allow_category_change`.
    #[must_use]
    pub fn allow_category_change(mut self, allow: bool) -> Self {
        self.allow_category_change = Some(allow);
        self
    }

    /// Check every limit the docs state (see `validate.rs` for the list,
    /// each with the page it comes from).
    pub fn validate(&self) -> Result<()> {
        validate::definition(self).map_err(Into::into)
    }
}

/// Fields of an existing template to change (`POST /{template_id}`).
///
/// Per `templates/template-management#edit-templates`: only `APPROVED`,
/// `REJECTED` or `PAUSED` templates can be edited; `components` replaces
/// *all* components; an approved template's category cannot change; an
/// approved template can be edited 10 times in 30 days (once per 24 h).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TemplateEdit {
    /// New category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<TemplateCategory>,
    /// Replacement components (all of them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<TemplateComponent>>,
    /// Placeholder style of the replacement components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter_format: Option<ParameterFormat>,
    /// Let Meta re-assign the category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_category_change: Option<bool>,
    /// Opt out of CTA URL link tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cta_url_link_tracking_opted_out: Option<bool>,
    /// Message time-to-live in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_send_ttl_seconds: Option<i64>,
    /// Sub-category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_category: Option<TemplateSubCategory>,
    /// Display format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_format: Option<DisplayFormat>,
    /// Deliver only to the user's primary device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_primary_device_delivery_only: Option<bool>,
}

impl TemplateEdit {
    /// Change only the category.
    pub fn category(category: TemplateCategory) -> Self {
        Self {
            category: Some(category),
            ..Self::default()
        }
    }

    /// Replace all components.
    pub fn components(components: Vec<TemplateComponent>) -> Self {
        Self {
            components: Some(components),
            ..Self::default()
        }
    }

    /// Check the documented limits that can be checked without knowing the
    /// template's current state.
    pub fn validate(&self) -> Result<()> {
        validate::edit(self).map_err(Into::into)
    }
}

/// One entry of a definition's `components` array.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum TemplateComponent {
    /// Text, media, location or product header (at most one).
    #[serde(alias = "header")]
    Header(HeaderComponent),
    /// Body (exactly one; the only required component).
    #[serde(alias = "body")]
    Body(BodyComponent),
    /// Footer (at most one).
    #[serde(alias = "footer")]
    Footer(FooterComponent),
    /// All buttons of the template, in display order.
    #[serde(alias = "buttons")]
    Buttons {
        /// The buttons.
        buttons: Vec<Button>,
    },
    /// Media-card or product-card carousel
    /// (`templates/marketing-templates/media-card-carousel-templates`,
    /// `catalogs/product-card-carousel-template-messages`).
    #[serde(alias = "carousel")]
    Carousel {
        /// 2 to 10 cards, all with the same components.
        cards: Vec<CarouselCard>,
    },
    /// Limited-time offer block
    /// (`templates/marketing-templates/limited-time-offer-templates`).
    #[serde(alias = "limited_time_offer")]
    LimitedTimeOffer {
        /// Offer text and expiration display.
        limited_time_offer: LimitedTimeOffer,
    },
    /// Call permission request (`templates/marketing-templates/call-permission-request-message-template`).
    #[serde(rename = "call_permission_request", alias = "CALL_PERMISSION_REQUEST")]
    CallPermissionRequest,
    /// Anything this version does not model, verbatim.
    #[serde(untagged)]
    Other(Value),
}

impl TemplateComponent {
    /// Text header without a placeholder.
    pub fn header_text(text: impl Into<String>) -> Self {
        Self::Header(HeaderComponent {
            format: HeaderFormat::Text,
            text: Some(text.into()),
            example: None,
        })
    }

    /// Text header with one positional placeholder (`{{1}}`) and its
    /// example value.
    pub fn header_text_positional(text: impl Into<String>, example: impl Into<String>) -> Self {
        Self::Header(HeaderComponent {
            format: HeaderFormat::Text,
            text: Some(text.into()),
            example: Some(HeaderExample {
                header_text: Some(vec![example.into()]),
                ..HeaderExample::default()
            }),
        })
    }

    /// Text header with one named placeholder and its example value.
    pub fn header_text_named(
        text: impl Into<String>,
        param_name: impl Into<String>,
        example: impl Into<String>,
    ) -> Self {
        Self::Header(HeaderComponent {
            format: HeaderFormat::Text,
            text: Some(text.into()),
            example: Some(HeaderExample {
                header_text_named_params: Some(vec![NamedParameterExample::new(
                    param_name, example,
                )]),
                ..HeaderExample::default()
            }),
        })
    }

    /// Media header (`Image`, `Video`, `Gif`, `Document`) with the example
    /// asset handle from the Resumable Upload API (`templates/template-media`).
    pub fn header_media(format: HeaderFormat, header_handle: impl Into<String>) -> Self {
        Self::Header(HeaderComponent {
            format,
            text: None,
            example: Some(HeaderExample {
                header_handle: Some(vec![header_handle.into()]),
                ..HeaderExample::default()
            }),
        })
    }

    /// Image header.
    pub fn header_image(header_handle: impl Into<String>) -> Self {
        Self::header_media(HeaderFormat::Image, header_handle)
    }

    /// Video header.
    pub fn header_video(header_handle: impl Into<String>) -> Self {
        Self::header_media(HeaderFormat::Video, header_handle)
    }

    /// Document header (PDF).
    pub fn header_document(header_handle: impl Into<String>) -> Self {
        Self::header_media(HeaderFormat::Document, header_handle)
    }

    /// Location header; the location itself is given at send time.
    /// Utility and marketing templates only.
    pub fn header_location() -> Self {
        Self::Header(HeaderComponent {
            format: HeaderFormat::Location,
            text: None,
            example: None,
        })
    }

    /// Product header (single-product templates and product-card carousel
    /// cards); the product is given at send time.
    pub fn header_product() -> Self {
        Self::Header(HeaderComponent {
            format: HeaderFormat::Product,
            text: None,
            example: None,
        })
    }

    /// Body without placeholders.
    pub fn body(text: impl Into<String>) -> Self {
        Self::Body(BodyComponent {
            text: Some(text.into()),
            ..BodyComponent::default()
        })
    }

    /// Body with positional placeholders and one example value per
    /// placeholder, in order.
    pub fn body_positional<I, S>(text: impl Into<String>, examples: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Body(BodyComponent {
            text: Some(text.into()),
            example: Some(BodyExample {
                body_text: Some(vec![examples.into_iter().map(Into::into).collect()]),
                body_text_named_params: None,
            }),
            add_security_recommendation: None,
        })
    }

    /// Body with named placeholders and `(name, example)` pairs.
    pub fn body_named<I, N, E>(text: impl Into<String>, examples: I) -> Self
    where
        I: IntoIterator<Item = (N, E)>,
        N: Into<String>,
        E: Into<String>,
    {
        Self::Body(BodyComponent {
            text: Some(text.into()),
            example: Some(BodyExample {
                body_text: None,
                body_text_named_params: Some(
                    examples
                        .into_iter()
                        .map(|(n, e)| NamedParameterExample::new(n, e))
                        .collect(),
                ),
            }),
            add_security_recommendation: None,
        })
    }

    /// Footer text.
    pub fn footer(text: impl Into<String>) -> Self {
        Self::Footer(FooterComponent {
            text: Some(text.into()),
            code_expiration_minutes: None,
        })
    }

    /// The buttons component.
    pub fn buttons(buttons: impl IntoIterator<Item = Button>) -> Self {
        Self::Buttons {
            buttons: buttons.into_iter().collect(),
        }
    }

    /// A carousel of cards.
    pub fn carousel(cards: impl IntoIterator<Item = CarouselCard>) -> Self {
        Self::Carousel {
            cards: cards.into_iter().collect(),
        }
    }

    /// A limited-time offer block.
    pub fn limited_time_offer(text: impl Into<String>, has_expiration: Option<bool>) -> Self {
        Self::LimitedTimeOffer {
            limited_time_offer: LimitedTimeOffer {
                text: text.into(),
                has_expiration,
            },
        }
    }

    /// The call permission request component.
    pub fn call_permission_request() -> Self {
        Self::CallPermissionRequest
    }
}

string_enum! {
    /// Header `format`.
    pub enum HeaderFormat {
        /// Text, at most one placeholder.
        Text => "TEXT",
        /// Image asset.
        Image => "IMAGE",
        /// Video asset.
        Video => "VIDEO",
        /// GIF (an mp4, Marketing Messages API only, `templates/components#media-header`).
        Gif => "GIF",
        /// Document (PDF).
        Document => "DOCUMENT",
        /// Map; the location is given at send time.
        Location => "LOCATION",
        /// Catalog product; the product is given at send time.
        Product => "PRODUCT",
    }
}

impl HeaderFormat {
    /// Image, video, GIF or document: needs a `header_handle` example.
    pub fn is_media(&self) -> bool {
        matches!(self, Self::Image | Self::Video | Self::Gif | Self::Document)
    }
}

/// `HEADER` component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderComponent {
    /// Kind of header.
    pub format: HeaderFormat,
    /// Header text (`Text` only), at most 60 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Placeholder example or media handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<HeaderExample>,
}

/// `HEADER.example`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HeaderExample {
    /// Positional placeholder example (one value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_text: Option<Vec<String>>,
    /// Named placeholder example (one entry).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_text_named_params: Option<Vec<NamedParameterExample>>,
    /// Media asset handle from the Resumable Upload API. `GET` returns a
    /// CDN URL here instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_handle: Option<Vec<String>>,
}

/// `BODY` component.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BodyComponent {
    /// Body text, at most 1024 characters (600 in limited-time-offer
    /// templates, 160 in single-product templates and carousel cards).
    /// Absent in authentication templates, whose text is preset by Meta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Placeholder examples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<BodyExample>,
    /// Authentication templates: append "For your security, do not share
    /// this code."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_security_recommendation: Option<bool>,
}

/// `BODY.example`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BodyExample {
    /// Positional examples. Always serialized as `[[..]]`, the shape of the
    /// reference schema and most examples; `"x"` and `["x", ..]` (also seen
    /// in the docs) are accepted when parsing.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_body_text"
    )]
    pub body_text: Option<Vec<Vec<String>>>,
    /// Named examples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_text_named_params: Option<Vec<NamedParameterExample>>,
}

fn de_body_text<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Vec<Vec<String>>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Shape {
        Nested(Vec<Vec<String>>),
        Flat(Vec<String>),
        Single(String),
    }
    Ok(
        Option::<Shape>::deserialize(deserializer)?.map(|shape| match shape {
            Shape::Nested(v) => v,
            Shape::Flat(v) => vec![v],
            Shape::Single(s) => vec![vec![s]],
        }),
    )
}

/// A named placeholder's example (`{"param_name", "example"}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedParameterExample {
    /// Placeholder name without braces.
    pub param_name: String,
    /// Example value.
    pub example: String,
}

impl NamedParameterExample {
    /// Build one.
    pub fn new(param_name: impl Into<String>, example: impl Into<String>) -> Self {
        Self {
            param_name: param_name.into(),
            example: example.into(),
        }
    }
}

/// `FOOTER` component.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FooterComponent {
    /// Footer text, at most 60 characters. Absent in authentication
    /// templates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Authentication templates: "This code expires in N minutes." (1–90).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_expiration_minutes: Option<u32>,
}

/// One carousel card: its own header (required), optional body and buttons.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarouselCard {
    /// Card components.
    pub components: Vec<TemplateComponent>,
}

impl CarouselCard {
    /// Build a card.
    pub fn new(components: impl IntoIterator<Item = TemplateComponent>) -> Self {
        Self {
            components: components.into_iter().collect(),
        }
    }
}

/// The `limited_time_offer` object of a `LIMITED_TIME_OFFER` component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitedTimeOffer {
    /// Offer text, at most 16 characters.
    pub text: String,
    /// Show the expiration timer (the expiry itself is given at send time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_expiration: Option<bool>,
}

/// A button of a `BUTTONS` component.
///
/// Limits per template (`templates/components#buttons`): 10 buttons in
/// total, quick replies grouped together, at most 2 URL, 1 phone number and
/// 1 copy code button.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum Button {
    /// Sends `text` back as a message when tapped.
    #[serde(alias = "quick_reply")]
    QuickReply {
        /// Label, at most 25 characters.
        text: String,
    },
    /// Opens `url`, optionally with one placeholder appended at the end.
    #[serde(alias = "url")]
    Url {
        /// Label, at most 25 characters.
        text: String,
        /// At most 2000 characters.
        url: String,
        /// Example for the placeholder, required when `url` has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        example: Option<Vec<String>>,
    },
    /// Calls `phone_number`.
    #[serde(alias = "phone_number")]
    PhoneNumber {
        /// Label, at most 25 characters.
        text: String,
        /// At most 20 characters.
        phone_number: String,
    },
    /// Copies a code given at send time (coupon templates).
    #[serde(alias = "copy_code")]
    CopyCode {
        /// Example code, at most 20 characters (15 in limited-time offers).
        example: String,
    },
    /// Authentication one-time-password button. Meta turns it into a `URL`
    /// button on creation, so `GET` never returns this variant.
    #[serde(alias = "otp")]
    Otp(OtpButton),
    /// Opens a WhatsApp Flow (`flows/guides/flows-templates`).
    #[serde(alias = "flow")]
    Flow(FlowButton),
    /// "View catalog" (`catalogs/catalog-template-messages`).
    #[serde(alias = "catalog")]
    Catalog {
        /// Label.
        text: String,
    },
    /// Multi-product message button (`catalogs/mpm-template-messages`).
    #[serde(alias = "mpm")]
    Mpm {
        /// Label.
        text: String,
    },
    /// Single-product message "View" button (`catalogs/spm-template-messages`).
    #[serde(alias = "spm")]
    Spm {
        /// Label.
        text: String,
    },
    /// WhatsApp call to the business
    /// (`calling/call-button-messages-deep-links`).
    #[serde(alias = "voice_call")]
    VoiceCall {
        /// Label, at most 20 characters; Meta defaults to "Call Now".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Button lifetime in minutes, 1440–43200.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl_minutes: Option<u32>,
    },
    /// Asks the user to share their phone number (`business-scoped-user-ids`).
    #[serde(alias = "request_contact_info")]
    RequestContactInfo {
        /// Cannot be customized: if present it must be exactly
        /// `"Share Contact Info"`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// Anything this version does not model, verbatim.
    #[serde(untagged)]
    Other(Value),
}

impl Button {
    /// Quick-reply button.
    pub fn quick_reply(text: impl Into<String>) -> Self {
        Self::QuickReply { text: text.into() }
    }

    /// URL button without a placeholder.
    pub fn url(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self::Url {
            text: text.into(),
            url: url.into(),
            example: None,
        }
    }

    /// URL button whose URL ends with one placeholder, with its example.
    pub fn url_with_example(
        text: impl Into<String>,
        url: impl Into<String>,
        example: impl Into<String>,
    ) -> Self {
        Self::Url {
            text: text.into(),
            url: url.into(),
            example: Some(vec![example.into()]),
        }
    }

    /// Phone-number button.
    pub fn phone_number(text: impl Into<String>, phone_number: impl Into<String>) -> Self {
        Self::PhoneNumber {
            text: text.into(),
            phone_number: phone_number.into(),
        }
    }

    /// Copy-code button with its example code.
    pub fn copy_code(example: impl Into<String>) -> Self {
        Self::CopyCode {
            example: example.into(),
        }
    }

    /// Catalog button.
    pub fn catalog(text: impl Into<String>) -> Self {
        Self::Catalog { text: text.into() }
    }

    /// Multi-product message button.
    pub fn mpm(text: impl Into<String>) -> Self {
        Self::Mpm { text: text.into() }
    }

    /// Single-product message button.
    pub fn spm(text: impl Into<String>) -> Self {
        Self::Spm { text: text.into() }
    }

    /// Voice-call button.
    pub fn voice_call(text: Option<String>, ttl_minutes: Option<u32>) -> Self {
        Self::VoiceCall { text, ttl_minutes }
    }

    /// Request-contact-info button (label is fixed by Meta).
    pub fn request_contact_info() -> Self {
        Self::RequestContactInfo { text: None }
    }
}

/// The OTP button of an authentication template, by `otp_type`
/// (`templates/authentication-templates/*`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "otp_type", rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum OtpButton {
    /// Copies the code to the clipboard.
    #[serde(alias = "copy_code")]
    CopyCode {
        /// Label, at most 25 characters; defaults to a localized "Copy Code".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// Android one-tap autofill after an app handshake; falls back to copy
    /// code.
    #[serde(alias = "one_tap")]
    OneTap {
        /// Copy-code fallback label, at most 25 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Autofill label, at most 25 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        autofill_text: Option<String>,
        /// 1 to 5 Android apps allowed to receive the code.
        #[serde(default)]
        supported_apps: Vec<SupportedApp>,
    },
    /// Android zero-tap: WhatsApp broadcasts the code straight to the app;
    /// falls back to one-tap, then copy code.
    #[serde(alias = "zero_tap")]
    ZeroTap {
        /// Copy-code fallback label, at most 25 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// One-tap fallback label, at most 25 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        autofill_text: Option<String>,
        /// Must be `true`: you accept the zero-tap terms. Meta refuses to
        /// create the template otherwise.
        zero_tap_terms_accepted: bool,
        /// 1 to 5 Android apps allowed to receive the code.
        #[serde(default)]
        supported_apps: Vec<SupportedApp>,
    },
}

/// An Android app allowed to receive one-tap / zero-tap codes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportedApp {
    /// Package name: at least two dot-separated segments, each starting
    /// with a letter, `[a-zA-Z0-9_]` only, at most 224 characters.
    pub package_name: String,
    /// App signing key hash: exactly 11 characters of `[a-zA-Z0-9+/=]`.
    pub signature_hash: String,
}

impl SupportedApp {
    /// Build one.
    pub fn new(package_name: impl Into<String>, signature_hash: impl Into<String>) -> Self {
        Self {
            package_name: package_name.into(),
            signature_hash: signature_hash.into(),
        }
    }
}

string_enum! {
    /// Flow button `flow_action`.
    pub enum FlowAction {
        /// Open a screen (`navigate_screen`). Meta's default.
        Navigate => "navigate",
        /// Start with a call to the Flow's data endpoint.
        DataExchange => "data_exchange",
    }
}

/// `FLOW` button. Fields from the reference schema and
/// `flows/guides/flows-templates`; the docs mirror has no complete creation
/// example for it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FlowButton {
    /// Label.
    pub text: String,
    /// Flow id. Exactly one of `flow_id`, `flow_name`, `flow_json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_id: Option<String>,
    /// Flow name (alternative to `flow_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_name: Option<String>,
    /// Inline Flow JSON (alternative to `flow_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_json: Option<String>,
    /// `navigate` (default) or `data_exchange`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_action: Option<FlowAction>,
    /// First screen; required when `flow_action` is `navigate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigate_screen: Option<String>,
}

impl FlowButton {
    /// A button opening the Flow with id `flow_id`.
    pub fn by_id(text: impl Into<String>, flow_id: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            flow_id: Some(flow_id.into()),
            ..Self::default()
        }
    }

    /// Set `flow_action: navigate` and the first screen.
    #[must_use]
    pub fn navigate(mut self, screen: impl Into<String>) -> Self {
        self.flow_action = Some(FlowAction::Navigate);
        self.navigate_screen = Some(screen.into());
        self
    }

    /// Set `flow_action: data_exchange`.
    #[must_use]
    pub fn data_exchange(mut self) -> Self {
        self.flow_action = Some(FlowAction::DataExchange);
        self
    }
}
