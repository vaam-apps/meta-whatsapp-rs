//! Send-time template invocation: the `template` object of a message.
//!
//! This is the *contract* other modules build on (messages, OTP, the MM API):
//! `TemplateMessage::new(name, language_code)` and its JSON shape
//! `{"name": .., "language": {"code": ..}, "components": [..]}` are stable,
//! `components` is omitted when empty.
//!
//! Shapes come from the send examples of `templates/overview`,
//! `templates/components`, `templates/template-media`,
//! `templates/tap-target-url-title-override`, `templates/marketing-templates/*`,
//! `templates/utility-templates/*`, `templates/authentication-templates/*`,
//! `catalogs/*-template-messages`, `flows/guides/flows-templates` and
//! `calling/call-button-messages-deep-links`.
//!
//! ## Button `index`
//!
//! Meta's examples write the button index both as a string (`"0"`, the
//! authentication, Flow, carousel and URL-encoding examples) and as a number
//! (`0`, the coupon, limited-time-offer and catalog examples). It is sent as
//! a string here and either form is accepted when parsing.
//!
//! ## Debug output
//!
//! [`Parameter`]'s `Debug` redacts `text` and `coupon_code` values: those
//! carry one-time passcodes in authentication templates (and customer data
//! elsewhere), and a template message is exactly the kind of value that
//! ends up in a `tracing` field.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use wa_core::Result;
use wa_core::error::ValidationError;
use wa_core::ids::{CatalogId, MediaId};

use super::macros::string_enum;

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
    pub components: Vec<SendComponent>,
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

    /// Set `language.policy` to `deterministic`, the only documented value.
    #[must_use]
    pub fn deterministic(mut self) -> Self {
        self.language.policy = Some("deterministic".to_owned());
        self
    }

    /// Append a component.
    #[must_use]
    pub fn component(mut self, component: SendComponent) -> Self {
        self.components.push(component);
        self
    }

    /// Header parameter (text, media, location or product).
    #[must_use]
    pub fn header(self, parameter: Parameter) -> Self {
        self.component(SendComponent::header(parameter))
    }

    /// Body parameters, in placeholder order for positional templates.
    #[must_use]
    pub fn body(self, parameters: impl IntoIterator<Item = Parameter>) -> Self {
        self.component(SendComponent::body(parameters))
    }

    /// Value for the placeholder of the URL button at `index` (also the
    /// OTP of an authentication template's button).
    #[must_use]
    pub fn url_button(self, index: u32, suffix: impl Into<String>) -> Self {
        self.component(SendComponent::url_button(index, suffix))
    }

    /// Payload returned in the webhook when the quick reply at `index` is
    /// tapped.
    #[must_use]
    pub fn quick_reply_button(self, index: u32, payload: impl Into<String>) -> Self {
        self.component(SendComponent::quick_reply_button(index, payload))
    }

    /// Code copied by the copy-code button at `index`.
    #[must_use]
    pub fn copy_code_button(self, index: u32, code: impl Into<String>) -> Self {
        self.component(SendComponent::copy_code_button(index, code))
    }

    /// Flow token and first-screen data for the Flow button at `index`.
    #[must_use]
    pub fn flow_button(
        self,
        index: u32,
        flow_token: Option<String>,
        flow_action_data: Option<Value>,
    ) -> Self {
        self.component(SendComponent::flow_button(
            index,
            flow_token,
            flow_action_data,
        ))
    }

    /// Catalog button, optionally choosing the header thumbnail product.
    #[must_use]
    pub fn catalog_button(self, index: u32, thumbnail_product_retailer_id: Option<String>) -> Self {
        self.component(SendComponent::catalog_button(
            index,
            thumbnail_product_retailer_id,
        ))
    }

    /// Multi-product button: thumbnail product and sections.
    #[must_use]
    pub fn mpm_button(
        self,
        index: u32,
        thumbnail_product_retailer_id: impl Into<String>,
        sections: impl IntoIterator<Item = MpmSection>,
    ) -> Self {
        self.component(SendComponent::mpm_button(
            index,
            thumbnail_product_retailer_id,
            sections,
        ))
    }

    /// Voice-call button overrides.
    #[must_use]
    pub fn voice_call_button(self, ttl_minutes: Option<u32>, payload: Option<String>) -> Self {
        self.component(SendComponent::voice_call_button(ttl_minutes, payload))
    }

    /// Carousel cards.
    #[must_use]
    pub fn carousel(self, cards: impl IntoIterator<Item = CarouselCardParameters>) -> Self {
        self.component(SendComponent::Carousel {
            cards: cards.into_iter().collect(),
        })
    }

    /// Limited-time offer expiry, UNIX time in milliseconds.
    #[must_use]
    pub fn limited_time_offer(self, expiration_time_ms: i64) -> Self {
        self.component(SendComponent::LimitedTimeOffer {
            parameters: vec![Parameter::LimitedTimeOffer {
                limited_time_offer: LimitedTimeOfferParameter { expiration_time_ms },
            }],
        })
    }

    /// Tap-target override: the message opens `url` with `title`
    /// (`templates/tap-target-url-title-override`).
    #[must_use]
    pub fn tap_target(self, url: impl Into<String>, title: impl Into<String>) -> Self {
        self.component(SendComponent::TapTargetConfiguration {
            parameters: vec![Parameter::TapTargetConfiguration {
                tap_target_configuration: vec![TapTarget {
                    url: url.into(),
                    title: title.into(),
                }],
            }],
        })
    }

    /// Check the documented limits that do not depend on the approved
    /// template (whose definition is not known here).
    pub fn validate(&self) -> Result<()> {
        super::validate::name(&self.name, "template.name")?;
        if self.language.code.trim().is_empty() {
            return Err(ValidationError::new("template.language.code", "must not be empty").into());
        }
        validate_components(&self.components, "template.components")
    }
}

fn validate_components(list: &[SendComponent], path: &str) -> Result<()> {
    for (i, c) in list.iter().enumerate() {
        let field = format!("{path}[{i}]");
        match c {
            SendComponent::Carousel { cards } => {
                // Media and product card carousels: "up to 10" cards.
                if cards.len() > 10 {
                    return Err(
                        ValidationError::new(format!("{field}.cards"), "at most 10 cards").into(),
                    );
                }
                for (j, card) in cards.iter().enumerate() {
                    validate_components(
                        &card.components,
                        &format!("{field}.cards[{j}].components"),
                    )?;
                }
            }
            SendComponent::Header { parameters }
            | SendComponent::Body { parameters }
            | SendComponent::Button { parameters, .. } => {
                for (j, p) in parameters.iter().enumerate() {
                    validate_parameter(p, &format!("{field}.parameters[{j}]"))?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_parameter(p: &Parameter, field: &str) -> Result<()> {
    let fail = |f: String, r: &str| Err(ValidationError::new(f, r).into());
    match p {
        // `templates/marketing-templates/coupon-templates`: "Maximum 20 characters".
        Parameter::CouponCode { coupon_code } if coupon_code.chars().count() > 20 => fail(
            format!("{field}.coupon_code"),
            "must be at most 20 characters",
        ),
        // `calling/call-button-messages-deep-links`: 1–43200 at send time.
        Parameter::TtlMinutes { ttl_minutes } if !(1..=43200).contains(ttl_minutes) => fail(
            format!("{field}.ttl_minutes"),
            "must be between 1 and 43200",
        ),
        // Same page: payload "Maximum 512 characters".
        Parameter::Payload { payload } if payload.chars().count() > 512 => {
            fail(format!("{field}.payload"), "must be at most 512 characters")
        }
        Parameter::Action { action } => {
            let Some(sections) = &action.sections else {
                return Ok(());
            };
            // `catalogs/mpm-template-messages`: up to 10 sections, titles of
            // 24 characters, 30 products "total, across all sections".
            if sections.is_empty() || sections.len() > 10 {
                return fail(format!("{field}.action.sections"), "1 to 10 sections");
            }
            let mut total = 0;
            for (i, s) in sections.iter().enumerate() {
                if s.title.trim().is_empty() || s.title.chars().count() > 24 {
                    return fail(
                        format!("{field}.action.sections[{i}].title"),
                        "1 to 24 characters",
                    );
                }
                total += s.product_items.len();
            }
            if total > 30 {
                return fail(
                    format!("{field}.action.sections"),
                    "at most 30 products across all sections",
                );
            }
            Ok(())
        }
        _ => Ok(()),
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

string_enum! {
    /// `sub_type` of a button component. Spelled as each page does,
    /// including the catalog page's upper-case `CATALOG`.
    pub enum ButtonSubType {
        /// URL button (also authentication OTP buttons).
        Url => "url",
        /// Quick-reply button.
        QuickReply => "quick_reply",
        /// Copy-code button (coupons, limited-time offers).
        CopyCode => "copy_code",
        /// Flow button.
        Flow => "flow",
        /// Catalog button.
        Catalog => "CATALOG",
        /// Multi-product message button.
        Mpm => "mpm",
        /// Voice-call button (sent without an `index`).
        VoiceCall => "voice_call",
    }
}

/// One entry of `template.components` in a send request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SendComponent {
    /// Header parameter.
    #[serde(alias = "HEADER")]
    Header {
        /// One parameter.
        parameters: Vec<Parameter>,
    },
    /// Body parameters.
    #[serde(alias = "BODY")]
    Body {
        /// In placeholder order (positional) or with `parameter_name`.
        parameters: Vec<Parameter>,
    },
    /// Parameters of one button.
    #[serde(alias = "BUTTON")]
    Button {
        /// Button type.
        sub_type: ButtonSubType,
        /// Zero-based position of the button in the template. Absent for
        /// voice-call buttons.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "ser_index",
            deserialize_with = "de_index"
        )]
        index: Option<u32>,
        /// Button parameters; omitted when empty (e.g. a catalog button
        /// without a thumbnail choice).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        parameters: Vec<Parameter>,
    },
    /// Carousel cards.
    #[serde(alias = "CAROUSEL")]
    Carousel {
        /// One entry per card.
        cards: Vec<CarouselCardParameters>,
    },
    /// Limited-time offer expiry.
    #[serde(alias = "LIMITED_TIME_OFFER")]
    LimitedTimeOffer {
        /// One `limited_time_offer` parameter.
        parameters: Vec<Parameter>,
    },
    /// Tap-target title/URL override.
    #[serde(alias = "TAP_TARGET_CONFIGURATION")]
    TapTargetConfiguration {
        /// One `tap_target_configuration` parameter.
        parameters: Vec<Parameter>,
    },
    /// Anything this version does not model, verbatim.
    #[serde(untagged)]
    Other(Value),
}

#[allow(clippy::ref_option, clippy::trivially_copy_pass_by_ref)] // serde's `serialize_with` signature
fn ser_index<S: Serializer>(
    index: &Option<u32>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    match index {
        Some(i) => serializer.serialize_str(&i.to_string()),
        None => serializer.serialize_none(),
    }
}

fn de_index<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<u32>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Index {
        Number(u32),
        Text(String),
    }
    match Option::<Index>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Index::Number(n)) => Ok(Some(n)),
        Some(Index::Text(s)) => {
            s.trim().parse().map(Some).map_err(|_| {
                serde::de::Error::custom(format!("button index `{s}` is not a number"))
            })
        }
    }
}

impl SendComponent {
    /// Header with one parameter.
    pub fn header(parameter: Parameter) -> Self {
        Self::Header {
            parameters: vec![parameter],
        }
    }

    /// Body with parameters.
    pub fn body(parameters: impl IntoIterator<Item = Parameter>) -> Self {
        Self::Body {
            parameters: parameters.into_iter().collect(),
        }
    }

    fn button(sub_type: ButtonSubType, index: Option<u32>, parameters: Vec<Parameter>) -> Self {
        Self::Button {
            sub_type,
            index,
            parameters,
        }
    }

    /// URL button placeholder value.
    pub fn url_button(index: u32, suffix: impl Into<String>) -> Self {
        Self::button(
            ButtonSubType::Url,
            Some(index),
            vec![Parameter::text(suffix)],
        )
    }

    /// Quick-reply payload.
    pub fn quick_reply_button(index: u32, payload: impl Into<String>) -> Self {
        Self::button(
            ButtonSubType::QuickReply,
            Some(index),
            vec![Parameter::payload(payload)],
        )
    }

    /// Copy-code value.
    pub fn copy_code_button(index: u32, code: impl Into<String>) -> Self {
        Self::button(
            ButtonSubType::CopyCode,
            Some(index),
            vec![Parameter::coupon_code(code)],
        )
    }

    /// Flow button action (`flow_token` defaults to `"unused"` at Meta).
    pub fn flow_button(
        index: u32,
        flow_token: Option<String>,
        flow_action_data: Option<Value>,
    ) -> Self {
        Self::button(
            ButtonSubType::Flow,
            Some(index),
            vec![Parameter::Action {
                action: ParameterAction {
                    flow_token,
                    flow_action_data,
                    ..ParameterAction::default()
                },
            }],
        )
    }

    /// Catalog button; without a thumbnail the parameters are omitted and
    /// Meta uses the first catalog item.
    pub fn catalog_button(index: u32, thumbnail_product_retailer_id: Option<String>) -> Self {
        let parameters = thumbnail_product_retailer_id
            .map(|id| {
                vec![Parameter::Action {
                    action: ParameterAction {
                        thumbnail_product_retailer_id: Some(id),
                        ..ParameterAction::default()
                    },
                }]
            })
            .unwrap_or_default();
        Self::button(ButtonSubType::Catalog, Some(index), parameters)
    }

    /// Multi-product button.
    pub fn mpm_button(
        index: u32,
        thumbnail_product_retailer_id: impl Into<String>,
        sections: impl IntoIterator<Item = MpmSection>,
    ) -> Self {
        Self::button(
            ButtonSubType::Mpm,
            Some(index),
            vec![Parameter::Action {
                action: ParameterAction {
                    thumbnail_product_retailer_id: Some(thumbnail_product_retailer_id.into()),
                    sections: Some(sections.into_iter().collect()),
                    ..ParameterAction::default()
                },
            }],
        )
    }

    /// Voice-call button overrides (no `index`, per the calling page).
    pub fn voice_call_button(ttl_minutes: Option<u32>, payload: Option<String>) -> Self {
        let mut parameters = Vec::new();
        if let Some(ttl_minutes) = ttl_minutes {
            parameters.push(Parameter::TtlMinutes { ttl_minutes });
        }
        if let Some(payload) = payload {
            parameters.push(Parameter::Payload { payload });
        }
        Self::button(ButtonSubType::VoiceCall, None, parameters)
    }
}

/// One card of a `carousel` send component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarouselCardParameters {
    /// Zero-based card position.
    pub card_index: u32,
    /// The card's header, body and button parameters.
    pub components: Vec<SendComponent>,
}

impl CarouselCardParameters {
    /// A card.
    pub fn new(card_index: u32, components: impl IntoIterator<Item = SendComponent>) -> Self {
        Self {
            card_index,
            components: components.into_iter().collect(),
        }
    }
}

/// A parameter value, tagged on `type`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Parameter {
    /// Text. `parameter_name` is required for named templates.
    Text {
        /// Placeholder name (named templates).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parameter_name: Option<String>,
        /// The value. URL-button values must be percent-encoded by the
        /// caller (`templates/components#url-encoding`).
        text: String,
    },
    /// Localized currency.
    Currency {
        /// The amount.
        currency: Currency,
    },
    /// Localized date/time.
    DateTime {
        /// The value.
        date_time: DateTime,
    },
    /// Image header.
    Image {
        /// Uploaded media id or public link.
        image: MediaSource,
    },
    /// Video header.
    Video {
        /// Uploaded media id or public link.
        video: MediaSource,
    },
    /// Document header.
    Document {
        /// Media and optional filename.
        document: DocumentMedia,
    },
    /// Location header.
    Location {
        /// Coordinates and labels.
        location: TemplateLocation,
    },
    /// Catalog product header (single-product templates, product cards).
    Product {
        /// The product.
        product: ProductReference,
    },
    /// Quick-reply / voice-call payload.
    Payload {
        /// Returned in webhooks.
        payload: String,
    },
    /// Copy-code button value.
    CouponCode {
        /// Code copied to the clipboard, at most 20 characters.
        coupon_code: String,
    },
    /// Flow, catalog and MPM button actions.
    Action {
        /// The action object.
        action: ParameterAction,
    },
    /// Limited-time offer expiry.
    LimitedTimeOffer {
        /// The expiry.
        limited_time_offer: LimitedTimeOfferParameter,
    },
    /// Tap-target override.
    TapTargetConfiguration {
        /// Title and URL (one entry in the docs).
        tap_target_configuration: Vec<TapTarget>,
    },
    /// Voice-call button lifetime override.
    TtlMinutes {
        /// 1–43200 minutes.
        ttl_minutes: u32,
    },
    /// Anything this version does not model, verbatim.
    #[serde(untagged)]
    Other(Value),
}

struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Debug for Parameter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { parameter_name, .. } => f
                .debug_struct("Text")
                .field("parameter_name", parameter_name)
                .field("text", &Redacted)
                .finish(),
            Self::CouponCode { .. } => f
                .debug_struct("CouponCode")
                .field("coupon_code", &Redacted)
                .finish(),
            Self::Currency { currency } => f
                .debug_struct("Currency")
                .field("currency", currency)
                .finish(),
            Self::DateTime { date_time } => f
                .debug_struct("DateTime")
                .field("date_time", date_time)
                .finish(),
            Self::Image { image } => f.debug_struct("Image").field("image", image).finish(),
            Self::Video { video } => f.debug_struct("Video").field("video", video).finish(),
            Self::Document { document } => f
                .debug_struct("Document")
                .field("document", document)
                .finish(),
            Self::Location { location } => f
                .debug_struct("Location")
                .field("location", location)
                .finish(),
            Self::Product { product } => {
                f.debug_struct("Product").field("product", product).finish()
            }
            Self::Payload { payload } => {
                f.debug_struct("Payload").field("payload", payload).finish()
            }
            Self::Action { action } => f.debug_struct("Action").field("action", action).finish(),
            Self::LimitedTimeOffer { limited_time_offer } => f
                .debug_struct("LimitedTimeOffer")
                .field("limited_time_offer", limited_time_offer)
                .finish(),
            Self::TapTargetConfiguration {
                tap_target_configuration,
            } => f
                .debug_struct("TapTargetConfiguration")
                .field("tap_target_configuration", tap_target_configuration)
                .finish(),
            Self::TtlMinutes { ttl_minutes } => f
                .debug_struct("TtlMinutes")
                .field("ttl_minutes", ttl_minutes)
                .finish(),
            Self::Other(v) => f.debug_tuple("Other").field(v).finish(),
        }
    }
}

impl Parameter {
    /// Positional text value.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            parameter_name: None,
            text: text.into(),
        }
    }

    /// Named text value.
    pub fn named(parameter_name: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Text {
            parameter_name: Some(parameter_name.into()),
            text: text.into(),
        }
    }

    /// Currency: `fallback_value` shown where localization fails, ISO 4217
    /// `code`, and the amount multiplied by 1000.
    pub fn currency(
        fallback_value: impl Into<String>,
        code: impl Into<String>,
        amount_1000: i64,
    ) -> Self {
        Self::Currency {
            currency: Currency {
                fallback_value: fallback_value.into(),
                code: code.into(),
                amount_1000,
            },
        }
    }

    /// Date/time shown as `fallback_value`.
    pub fn date_time(fallback_value: impl Into<String>) -> Self {
        Self::DateTime {
            date_time: DateTime {
                fallback_value: fallback_value.into(),
            },
        }
    }

    /// Image by uploaded media id.
    pub fn image_id(id: impl Into<MediaId>) -> Self {
        Self::Image {
            image: MediaSource::Id(id.into()),
        }
    }

    /// Image by public link.
    pub fn image_link(link: impl Into<String>) -> Self {
        Self::Image {
            image: MediaSource::Link(link.into()),
        }
    }

    /// Video by uploaded media id.
    pub fn video_id(id: impl Into<MediaId>) -> Self {
        Self::Video {
            video: MediaSource::Id(id.into()),
        }
    }

    /// Video by public link.
    pub fn video_link(link: impl Into<String>) -> Self {
        Self::Video {
            video: MediaSource::Link(link.into()),
        }
    }

    /// Document by uploaded media id, with an optional filename.
    pub fn document_id(id: impl Into<MediaId>, filename: Option<String>) -> Self {
        Self::Document {
            document: DocumentMedia {
                source: MediaSource::Id(id.into()),
                filename,
            },
        }
    }

    /// Document by public link, with an optional filename.
    pub fn document_link(link: impl Into<String>, filename: Option<String>) -> Self {
        Self::Document {
            document: DocumentMedia {
                source: MediaSource::Link(link.into()),
                filename,
            },
        }
    }

    /// Location header value.
    pub fn location(location: TemplateLocation) -> Self {
        Self::Location { location }
    }

    /// Catalog product.
    pub fn product(
        product_retailer_id: impl Into<String>,
        catalog_id: impl Into<CatalogId>,
    ) -> Self {
        Self::Product {
            product: ProductReference {
                product_retailer_id: product_retailer_id.into(),
                catalog_id: catalog_id.into(),
            },
        }
    }

    /// Quick-reply payload.
    pub fn payload(payload: impl Into<String>) -> Self {
        Self::Payload {
            payload: payload.into(),
        }
    }

    /// Copy-code value.
    pub fn coupon_code(code: impl Into<String>) -> Self {
        Self::CouponCode {
            coupon_code: code.into(),
        }
    }
}

/// `currency` parameter object (`templates/template-media`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Currency {
    /// Shown when localization is unavailable.
    pub fallback_value: String,
    /// ISO 4217 currency code.
    pub code: String,
    /// Amount × 1000.
    pub amount_1000: i64,
}

/// `date_time` parameter object (`templates/template-media`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateTime {
    /// The text shown.
    pub fallback_value: String,
}

/// Media by uploaded id (`{"id": ..}`) or public link (`{"link": ..}`).
/// Meta recommends uploading (`templates/template-media`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaSource {
    /// Uploaded media id.
    Id(MediaId),
    /// Publicly reachable URL.
    Link(String),
}

/// `document` parameter object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentMedia {
    /// Id or link.
    #[serde(flatten)]
    pub source: MediaSource,
    /// Filename with extension (`messages/document-messages`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

/// `location` parameter object. Coordinates are strings in every example.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateLocation {
    /// Latitude, decimal degrees.
    pub latitude: String,
    /// Longitude, decimal degrees.
    pub longitude: String,
    /// Location name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

impl TemplateLocation {
    /// From numeric coordinates.
    pub fn new(latitude: f64, longitude: f64) -> Self {
        Self {
            latitude: latitude.to_string(),
            longitude: longitude.to_string(),
            name: None,
            address: None,
        }
    }
}

/// `product` parameter object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductReference {
    /// Product SKU ("Content ID" in Commerce Manager).
    pub product_retailer_id: String,
    /// Connected catalog id.
    pub catalog_id: CatalogId,
}

/// `action` parameter object; which fields apply depends on the button.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ParameterAction {
    /// Flow buttons: token echoed in Flow responses (Meta default `"unused"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_token: Option<String>,
    /// Flow buttons: data for the first screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_action_data: Option<Value>,
    /// Catalog and MPM buttons: product whose image becomes the header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_product_retailer_id: Option<String>,
    /// MPM buttons: up to 10 sections, 30 products in total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<MpmSection>>,
}

/// A multi-product message section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MpmSection {
    /// Section title, at most 24 characters.
    pub title: String,
    /// Products in the section.
    pub product_items: Vec<ProductItem>,
}

impl MpmSection {
    /// A section listing product SKUs.
    pub fn new<I, S>(title: impl Into<String>, product_retailer_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            title: title.into(),
            product_items: product_retailer_ids
                .into_iter()
                .map(|id| ProductItem {
                    product_retailer_id: id.into(),
                })
                .collect(),
        }
    }
}

/// A product in an MPM section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductItem {
    /// Product SKU.
    pub product_retailer_id: String,
}

/// `limited_time_offer` parameter object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitedTimeOfferParameter {
    /// Offer expiry, UNIX time in milliseconds.
    pub expiration_time_ms: i64,
}

/// One tap-target override.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapTarget {
    /// URL opened on tap.
    pub url: String,
    /// Title shown.
    pub title: String,
}
